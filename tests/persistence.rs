//! Proves data survives a daemon restart — at the process boundary, on
//! purpose: SurrealDB's embedded RocksDB engine does not release its file
//! lock when a `Surreal` handle merely drops within the same process (see
//! `store::tests`'s comment), so this spawns two genuinely separate
//! `memcastle serve` processes against the same palace directory, which is
//! also the more representative test of the actual claim ("stop and restart
//! the daemon; your data is still there").

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use memcastle::server::lifecycle::{RuntimeInfo, read_if_live};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

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

        wait_for_all_jobs_completed(&client, &base).await;
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
        .env("MEMCASTLE_BIND", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // If the test panics before explicitly stopping the daemon, don't
        // leak a process holding the palace's RocksDB lock forever.
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

async fn wait_for_all_jobs_completed(client: &reqwest::Client, base: &str) {
    for _ in 0..100 {
        let jobs: Vec<serde_json::Value> = client
            .get(format!("{base}/api/jobs"))
            .send()
            .await
            .expect("list jobs")
            .json()
            .await
            .expect("jobs response is json");
        if !jobs.is_empty() && jobs.iter().all(|job| job["status"] == "completed") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("mining job did not complete in time");
}

async fn stop_daemon(bin: &Path, palace: &Path, child: &mut Child) {
    let status = Command::new(bin)
        .arg("stop")
        .env("MEMCASTLE_PALACE_PATH", palace)
        .status()
        .await
        .expect("run `memcastle stop`");
    assert!(status.success(), "`memcastle stop` should succeed");

    tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("daemon process exits within 15s of being asked to stop")
        .expect("daemon process can be waited on");
}
