//! Proves data survives a daemon restart — at the process boundary, on
//! purpose: SurrealDB's embedded SurrealKV engine does not release its file
//! lock when a `Surreal` handle merely drops within the same process (see
//! `store::tests`'s comment), so this spawns two genuinely separate
//! `memcastle serve` processes against the same palace directory, which is
//! also the more representative test of the actual claim ("stop and restart
//! the daemon; your data is still there").
//!
//! These deliberately keep the default `store.sync` (a flush per commit): they
//! SIGKILL a daemon and expect everything it acknowledged to be there, which a
//! relaxed sync does not promise. Do not set `MEMCASTLE_STORE_SYNC` here to
//! speed them up.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use memcastle::server::lifecycle::{RuntimeInfo, read_if_live};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

mod common;

use common::wait_for_all_jobs_completed;

#[tokio::test]
async fn drawers_survive_a_daemon_restart_against_the_same_palace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let fixture = dir.path().join("fixture");
    std::fs::create_dir_all(&fixture).expect("create fixture dir");
    std::fs::write(
        fixture.join("note.txt"),
        "a note written before the restart",
    )
    .expect("write fixture file");

    let bin = cargo_bin("memcastle");
    let client = reqwest::Client::new();

    // Round 1: a fresh daemon mines the fixture, then is asked to stop.
    {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        client
            .post(format!("{base}/api/jobs"))
            .json(&serde_json::json!({
                "type": "mine",
                "path": fixture,
                "wing": null,
                "requested_by": "test",
            }))
            .send()
            .await
            .expect("submit mine job")
            .error_for_status()
            .expect("mine job accepted");

        wait_for_all_jobs_completed(&client, &base, 1).await;
        stop_daemon(&bin, &palace, &mut child).await;
    }

    // Round 2: a brand-new process, same palace directory — the drawer the
    // first process mined must still be there and still searchable.
    {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        let hits: Vec<serde_json::Value> = client
            .get(format!("{base}/api/search"))
            .query(&[("q", "note written before"), ("limit", "10")])
            .send()
            .await
            .expect("search request")
            .json()
            .await
            .expect("search response is json");
        assert!(
            !hits.is_empty(),
            "expected the drawer mined before the restart to still be searchable"
        );

        stop_daemon(&bin, &palace, &mut child).await;
    }
}

#[tokio::test]
async fn checkpoint_drawers_survive_a_daemon_restart_against_the_same_palace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");

    let bin = cargo_bin("memcastle");
    let client = reqwest::Client::new();

    // Round 1: a fresh daemon runs a checkpoint job, then is asked to stop.
    {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        client
            .post(format!("{base}/api/jobs"))
            .json(&serde_json::json!({
                "type": "checkpoint",
                "payload": {
                    "items": [{
                        "destination": "general",
                        "content": "a checkpoint written before the restart",
                        "tags": [],
                        "source": { "kind": "manual", "uri": null, "agent": "test" },
                        "fact": null,
                    }],
                },
                "requested_by": "test",
            }))
            .send()
            .await
            .expect("submit checkpoint job")
            .error_for_status()
            .expect("checkpoint job accepted");

        wait_for_all_jobs_completed(&client, &base, 1).await;
        stop_daemon(&bin, &palace, &mut child).await;
    }

    // Round 2: a brand-new process, same palace directory — the drawer the
    // first process checkpointed must still be there and still searchable.
    {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        let hits: Vec<serde_json::Value> = client
            .get(format!("{base}/api/search"))
            .query(&[("q", "checkpoint written before"), ("limit", "10")])
            .send()
            .await
            .expect("search request")
            .json()
            .await
            .expect("search response is json");
        assert!(
            !hits.is_empty(),
            "expected the drawer checkpointed before the restart to still be searchable"
        );

        // The way an agent actually asks: a query with a word the stored
        // text lacks ("preferences"), which a strict match alone would fail.
        let hits: Vec<serde_json::Value> = client
            .get(format!("{base}/api/recall"))
            .query(&[
                ("q", "checkpoint before the restart preferences"),
                ("limit", "10"),
            ])
            .send()
            .await
            .expect("recall request")
            .json()
            .await
            .expect("recall response is json");
        assert!(
            !hits.is_empty(),
            "expected recall to find the pre-restart drawer despite an unmatched word"
        );

        stop_daemon(&bin, &palace, &mut child).await;
    }
}

/// Invariant #3 at the process boundary: a job left `Running` by a daemon
/// that died uncleanly (SIGKILL — no shutdown hook runs) is picked up by
/// the next daemon's `server::run` -> `Scheduler::recover` and finished.
///
/// Lives here rather than in `tests/server.rs` because seeding a `Running`
/// row needs the palace's SurrealKV lock, which an in-process test cannot
/// take back after a `Surreal` handle drops (see `store::tests`); a second
/// real process is the only way to have "the previous owner is gone".
#[tokio::test]
async fn a_job_running_when_the_daemon_is_killed_is_recovered_and_finished_after_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let bin = cargo_bin("memcastle");
    let client = reqwest::Client::new();

    // Round 1: start a demo job long enough (150ms/step) to still be
    // `Running` when we kill the process, then SIGKILL it mid-flight.
    let job_id = {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        let submitted: serde_json::Value = client
            .post(format!("{base}/api/jobs"))
            .json(&serde_json::json!({ "type": "demo", "steps": 400, "requested_by": "test" }))
            .send()
            .await
            .expect("submit demo job")
            .error_for_status()
            .expect("demo job accepted")
            .json()
            .await
            .expect("job json");
        let id = submitted["id"].as_str().expect("job id").to_string();

        // Wait for real progress, so the checkpoint the recovered job
        // resumes from is provably non-empty.
        wait_for_job(&client, &base, &id, |job| {
            job["status"] == "running" && job["progress"]["current"].as_u64().unwrap_or(0) >= 2
        })
        .await;

        child.kill().await.expect("SIGKILL the daemon");
        // `kill` reaps the process; the registry file it never got to
        // remove would otherwise make the next spawn look already-live on
        // platforms where liveness is not probed.
        memcastle::server::lifecycle::remove(&palace);
        id
    };

    // Round 2: a new daemon on the same palace must not leave the job
    // stranded in `Running` — it is requeued, re-claimed (attempt 2) and
    // resumed past the steps already done.
    let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
    let base = format!("http://{}", info.bind_addr);
    let job = wait_for_job(&client, &base, &job_id, |job| {
        job["attempt"].as_u64().unwrap_or(0) >= 2
    })
    .await;
    assert!(
        job["progress"]["current"].as_u64().unwrap_or(0) >= 2,
        "a recovered job must resume from its checkpoint, not restart: {job}"
    );
    assert!(
        matches!(
            job["status"].as_str(),
            Some("running" | "queued" | "completed")
        ),
        "a recovered job must be back in the queue's normal flow: {job}"
    );

    stop_daemon(&bin, &palace, &mut child).await;
}

/// Invariant #3 for a user's intent, not just the job: a pause or cancel the
/// daemon acknowledged must survive a SIGKILL that lands before the handler
/// honoured it. The request is written to the job record before the API
/// answers, so `Scheduler::recover` finds it on restart.
///
/// Passes whether the kill lands before the handler notices the request
/// (recovery honours the persisted flag) or after (the handler already did);
/// what it must never do is run the job again.
async fn an_acknowledged_stop_request_survives_a_sigkill(action: &str, expected_status: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let bin = cargo_bin("memcastle");
    let client = reqwest::Client::new();

    let job_id = {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);
        let submitted: serde_json::Value = client
            .post(format!("{base}/api/jobs"))
            .json(&serde_json::json!({ "type": "demo", "steps": 400, "requested_by": "test" }))
            .send()
            .await
            .expect("submit demo job")
            .json()
            .await
            .expect("job json");
        let id = submitted["id"].as_str().expect("job id").to_string();
        wait_for_job(&client, &base, &id, |job| {
            job["status"] == "running" && job["progress"]["current"].as_u64().unwrap_or(0) >= 2
        })
        .await;

        client
            .post(format!("{base}/api/jobs/{id}/{action}"))
            .send()
            .await
            .expect("send the request")
            .error_for_status()
            .expect("the request is acknowledged");
        // Straight away: the handler only looks every 150ms.
        child.kill().await.expect("SIGKILL the daemon");
        memcastle::server::lifecycle::remove(&palace);
        id
    };

    let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
    let base = format!("http://{}", info.bind_addr);
    let job = wait_for_job(&client, &base, &job_id, |job| {
        job["status"] == expected_status
    })
    .await;
    // Give a wrongly re-queued job time to be re-claimed, then check it
    // really stayed put.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let later: serde_json::Value = client
        .get(format!("{base}/api/jobs/{job_id}"))
        .send()
        .await
        .expect("get job")
        .json()
        .await
        .expect("job json");
    assert_eq!(later["status"], expected_status, "{later}");
    assert_eq!(
        later["attempt"], job["attempt"],
        "the job must not have been claimed again: {later}"
    );

    stop_daemon(&bin, &palace, &mut child).await;
}

#[tokio::test]
async fn a_cancel_requested_before_a_sigkill_ends_cancelled_after_restart_not_rerun() {
    an_acknowledged_stop_request_survives_a_sigkill("cancel", "cancelled").await;
}

#[tokio::test]
async fn a_pause_requested_before_a_sigkill_comes_back_paused_after_restart() {
    an_acknowledged_stop_request_survives_a_sigkill("pause", "paused").await;
}

/// `memcastle daemon restart` must bring the daemon back the way it was asked to,
/// and only say so once it is serving: a bare `memcastle serve` would drop
/// `--bind`/`--port` and come back on the default address.
#[tokio::test]
async fn restart_brings_the_daemon_back_on_the_requested_address() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let bin = cargo_bin("memcastle");

    let (mut child, first) = spawn_daemon_and_wait(&bin, &palace).await;

    // A port that was free a moment ago; the restarted daemon takes it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let wanted = format!("127.0.0.1:{port}");
    assert_ne!(first.bind_addr, wanted);

    // Output goes to files, not pipes: on Windows the daemon `restart` leaves
    // running inherits every inheritable handle of its parent, including the
    // write end of a captured pipe, so waiting for the pipe to close would
    // wait for the daemon to exit.
    let stdout_file = dir.path().join("restart.out");
    let stderr_file = dir.path().join("restart.err");
    let status = Command::new(&bin)
        .args([
            "daemon",
            "restart",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .env("MEMCASTLE_PALACE_PATH", &palace)
        .stdout(std::fs::File::create(&stdout_file).expect("stdout file"))
        .stderr(std::fs::File::create(&stderr_file).expect("stderr file"))
        .status()
        .await
        .expect("run `memcastle daemon restart`");
    assert!(
        status.success(),
        "restart failed: {}",
        std::fs::read_to_string(&stderr_file).unwrap_or_default()
    );
    let stdout = std::fs::read_to_string(&stdout_file).expect("restart output");
    assert!(
        stdout.contains(&wanted),
        "it must report where it came back: {stdout}"
    );

    // By the time it returned the daemon was serving, on that address.
    let second = read_if_live(&palace).expect("the restarted daemon is registered");
    assert_eq!(second.bind_addr, wanted);
    let health = reqwest::get(format!("http://{wanted}/api/health"))
        .await
        .expect("health request");
    assert!(health.status().is_success());

    // Tear down the daemon `restart` spawned (the original one is gone).
    let stopped = Command::new(&bin)
        .args(["daemon", "stop"])
        .env("MEMCASTLE_PALACE_PATH", &palace)
        .status()
        .await
        .expect("run `memcastle daemon stop`");
    assert!(stopped.success());
    for _ in 0..150 {
        if read_if_live(&palace).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = child.wait().await;
}

/// What a `memcastle` CLI run printed, with the exit status.
struct CliRun {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run `memcastle <args>` against `palace` with output going to files, not
/// pipes: on Windows a daemon the command leaves running inherits every
/// inheritable handle of its parent, including the write end of a captured
/// pipe, so waiting for the pipe to close would wait for the daemon to exit.
async fn run_cli(bin: &Path, dir: &Path, palace: &Path, args: &[&str]) -> CliRun {
    let stdout_file = dir.join("cli.out");
    let stderr_file = dir.join("cli.err");
    let status = Command::new(bin)
        .args(args)
        .env("MEMCASTLE_PALACE_PATH", palace)
        // Nothing here may talk to a developer's own daemon or token.
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdout(std::fs::File::create(&stdout_file).expect("stdout file"))
        .stderr(std::fs::File::create(&stderr_file).expect("stderr file"))
        .status()
        .await
        .expect("run memcastle");
    CliRun {
        success: status.success(),
        stdout: std::fs::read_to_string(&stdout_file).unwrap_or_default(),
        stderr: std::fs::read_to_string(&stderr_file).unwrap_or_default(),
    }
}

/// Stop the daemon of `palace` through the CLI and wait for it to unregister,
/// for a test whose daemon is not a child of the test process.
async fn stop_detached_daemon(bin: &Path, dir: &Path, palace: &Path) {
    let stopped = run_cli(bin, dir, palace, &["daemon", "stop"]).await;
    assert!(stopped.success, "daemon stop failed: {}", stopped.stderr);
    for _ in 0..150 {
        if read_if_live(palace).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the daemon still holds the palace 15s after `daemon stop`");
}

/// `memcastle daemon start` must return only once a daemon is really serving,
/// and leave it running after the command has exited.
#[tokio::test]
async fn daemon_start_brings_up_a_detached_daemon_that_serves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let bin = cargo_bin("memcastle");
    assert!(read_if_live(&palace).is_none());

    let started = run_cli(
        &bin,
        dir.path(),
        &palace,
        &["daemon", "start", "--bind", "127.0.0.1", "--port", "0"],
    )
    .await;
    assert!(started.success, "daemon start failed: {}", started.stderr);

    // The command has exited, yet the daemon it started is up and registered.
    let info = read_if_live(&palace).expect("the started daemon is registered");
    assert!(
        started.stdout.contains("started:") && started.stdout.contains(&info.bind_addr),
        "it must report where it is serving: {}",
        started.stdout
    );
    let health = reqwest::get(format!("http://{}/api/health", info.bind_addr))
        .await
        .expect("health request");
    assert!(health.status().is_success());

    stop_detached_daemon(&bin, dir.path(), &palace).await;
}

/// A second daemon would only die on the palace lock, so `daemon start` says
/// so up front and points at the command that replaces the running one.
#[tokio::test]
async fn daemon_start_refuses_when_a_daemon_is_already_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let bin = cargo_bin("memcastle");

    let (mut child, first) = spawn_daemon_and_wait(&bin, &palace).await;

    let again = run_cli(&bin, dir.path(), &palace, &["daemon", "start"]).await;
    assert!(!again.success, "a second start must fail: {}", again.stdout);
    assert!(
        again.stderr.contains("memcastle::client::already_running"),
        "{}",
        again.stderr
    );
    // The refusal must not have disturbed the daemon that was running.
    assert_eq!(
        read_if_live(&palace).expect("still registered").pid,
        first.pid
    );

    stop_daemon(&bin, &palace, &mut child).await;
}

async fn wait_for_job(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    mut done: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..600 {
        let job: serde_json::Value = client
            .get(format!("{base}/api/jobs/{id}"))
            .send()
            .await
            .expect("get job")
            .json()
            .await
            .expect("job json");
        if done(&job) {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("job {id} never reached the expected state within 60s");
}

/// Spawn `memcastle serve` against `palace` and wait for it to register —
/// i.e. for its listener to actually be up, not just the OS process to
/// exist.
///
/// Diagnostic on purpose: this test's failures on Windows CI (see git log
/// for the two prior, unsuccessful attempts at a fix) kept happening with
/// zero information, because the daemon's own stdout/stderr were discarded.
/// Piping stderr and surfacing it — whether the process exited early or is
/// still running once the deadline passes — turns "didn't start in time"
/// into an actual, actionable error message on the next failure, instead of
/// guessing again.
async fn spawn_daemon_and_wait(bin: &Path, palace: &Path) -> (Child, RuntimeInfo) {
    let mut child = Command::new(bin)
        .arg("serve")
        .env("MEMCASTLE_PALACE_PATH", palace)
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "0")
        // The test daemon runs without authentication, whatever the developer's shell exports.
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // If the test panics before explicitly stopping the daemon, don't
        // leak a process holding the palace's SurrealKV lock forever.
        .kill_on_drop(true)
        .spawn()
        .expect("spawn `memcastle serve`");

    // Generous relative to the ~200ms an idle daemon actually takes to
    // bind: a real subprocess (as opposed to `TestDaemon`'s in-process
    // task) under Windows CI's `cargo llvm-cov` instrumentation, paying for
    // SurrealDB 3.x's heavier embedded-engine startup, has been observed
    // needing much longer than that.
    for _ in 0..1200 {
        if let Some(info) = read_if_live(palace) {
            return (child, info);
        }
        if let Ok(Some(status)) = child.try_wait() {
            let stderr = drain_stderr(&mut child).await;
            panic!("daemon exited with {status} before registering; stderr:\n{stderr}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stderr = drain_stderr(&mut child).await;
    panic!("daemon did not register within 60s; stderr so far:\n{stderr}");
}

/// Best-effort read of whatever a still-running or just-exited child has
/// written to stderr so far. Bounded, not a plain `read_to_string`: on a
/// still-running process the pipe never reaches EOF, so an unbounded read
/// would hang this diagnostic itself.
async fn drain_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut out) = child.stderr.take() {
        let _ =
            tokio::time::timeout(Duration::from_millis(500), out.read_to_string(&mut stderr)).await;
    }
    stderr
}

async fn stop_daemon(bin: &Path, palace: &Path, child: &mut Child) {
    let status = Command::new(bin)
        .args(["daemon", "stop"])
        .env("MEMCASTLE_PALACE_PATH", palace)
        .status()
        .await
        .expect("run `memcastle daemon stop`");
    assert!(status.success(), "`memcastle daemon stop` should succeed");

    tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("daemon process exits within 15s of being asked to stop")
        .expect("daemon process can be waited on");
}

/// A unit vector along `axis` of the stored dimension (768): two on different
/// axes are orthogonal, so which drawer a vector search returns is exact.
fn axis_vector(axis: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; 768];
    v[axis] = 1.0;
    v
}

#[tokio::test]
async fn vector_index_validity_and_graph_links_survive_a_daemon_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let bin = cargo_bin("memcastle");
    let client = reqwest::Client::new();

    let write = |base: String, content: &'static str| {
        let client = client.clone();
        async move {
            let drawer: serde_json::Value = client
                .post(format!("{base}/api/wings/w/rooms/r/drawers"))
                .json(&serde_json::json!({ "content": content }))
                .send()
                .await
                .expect("create drawer")
                .error_for_status()
                .expect("drawer accepted")
                .json()
                .await
                .expect("drawer json");
            drawer["id"].as_str().expect("drawer id").to_string()
        }
    };

    // Round 1: derived data is written, then the daemon is stopped.
    let moment;
    {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        let kept = write(base.clone(), "persisted vector memory about harbours").await;
        let old = write(base.clone(), "persisted belief that the ferry runs daily").await;
        let sibling = write(base.clone(), "persisted note about the ferry timetable").await;
        for (id, axis) in [(&kept, 0), (&old, 1)] {
            client
                .put(format!("{base}/api/drawers/{id}/embedding"))
                .json(&serde_json::json!({ "embedding": axis_vector(axis) }))
                .send()
                .await
                .expect("put embedding")
                .error_for_status()
                .expect("embedding accepted");
        }
        for id in [&old, &sibling] {
            client
                .post(format!("{base}/api/drawers/{id}/mentions"))
                .json(&serde_json::json!({ "name": "ferry", "kind": "service" }))
                .send()
                .await
                .expect("mention")
                .error_for_status()
                .expect("mention accepted");
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        moment = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        tokio::time::sleep(Duration::from_millis(60)).await;
        client
            .post(format!("{base}/api/drawers/{old}/supersede"))
            .json(&serde_json::json!({ "content": "persisted belief that the ferry runs weekly" }))
            .send()
            .await
            .expect("supersede")
            .error_for_status()
            .expect("supersede accepted");

        stop_daemon(&bin, &palace, &mut child).await;
    }

    // Round 2: a new process over the same files finds everything again.
    {
        let (mut child, info) = spawn_daemon_and_wait(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);
        let contents = |hits: &serde_json::Value| -> Vec<String> {
            hits.as_array()
                .expect("hits")
                .iter()
                .map(|hit| hit["content"].as_str().expect("content").to_string())
                .collect()
        };

        // The HNSW index serves a vector search without any re-embedding.
        let semantic: serde_json::Value = client
            .post(format!("{base}/api/search"))
            .json(&serde_json::json!({
                "text": "",
                "ranking": "semantic",
                "limit": 1,
                "query_embedding": axis_vector(0),
            }))
            .send()
            .await
            .expect("semantic search")
            .json()
            .await
            .expect("json");
        assert_eq!(
            contents(&semantic),
            ["persisted vector memory about harbours"],
            "the vector index must survive the restart"
        );

        // Validity survived: current search sees the correction only, and a
        // search as of before the correction sees the old belief.
        let current: serde_json::Value = client
            .get(format!("{base}/api/search"))
            .query(&[("q", "persisted belief ferry"), ("ranking", "lexical")])
            .send()
            .await
            .expect("current search")
            .json()
            .await
            .expect("json");
        assert_eq!(
            contents(&current),
            ["persisted belief that the ferry runs weekly"]
        );
        let then: serde_json::Value = client
            .get(format!("{base}/api/search"))
            .query(&[
                ("q", "persisted belief ferry"),
                ("ranking", "lexical"),
                ("as_of", moment.as_str()),
            ])
            .send()
            .await
            .expect("as-of search")
            .json()
            .await
            .expect("json");
        assert_eq!(
            contents(&then),
            ["persisted belief that the ferry runs daily"]
        );

        // The graph links survived: expanding from the sibling reaches the
        // drawer that shares the `ferry` entity with it.
        let expanded: serde_json::Value = client
            .get(format!("{base}/api/search"))
            .query(&[
                ("q", "timetable"),
                ("ranking", "lexical"),
                ("include_historical", "true"),
                ("expand", "true"),
            ])
            .send()
            .await
            .expect("expanded search")
            .json()
            .await
            .expect("json");
        let found = contents(&expanded);
        assert_eq!(found[0], "persisted note about the ferry timetable");
        assert!(
            found.contains(&"persisted belief that the ferry runs daily".to_string()),
            "graph expansion must reach the linked drawer after a restart: {found:?}"
        );

        stop_daemon(&bin, &palace, &mut child).await;
    }
}
