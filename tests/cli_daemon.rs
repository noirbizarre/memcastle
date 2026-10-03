//! The CLI against a running daemon: flags whose whole job is to change what
//! the daemon is asked, which no parse-only test can prove.
//!
//! The daemon is `TestDaemon`'s in-process task, so each CLI invocation is a
//! real subprocess (`tokio::process`, never a blocking `assert_cmd` call: the
//! daemon shares this test's runtime and a blocked thread would stall it).

mod common;

use std::process::Stdio;

use assert_cmd::cargo::cargo_bin;
use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::{Job, JobKind, JobStatus, MiningSource};
use tokio::process::Command;

/// The `memcastle` binary pointed at `daemon`'s palace, so it discovers the
/// daemon through the registry file exactly as a user's shell would.
fn memcastle(daemon: &TestDaemon) -> Command {
    let mut command = Command::new(cargo_bin("memcastle"));
    command
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .env_remove("MEMCASTLE_MODE")
        // A developer's real token must not leak into a test daemon that has none.
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdin(Stdio::null());
    command
}

fn checkpoint_payload_file(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("payload.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "items": [{
                "destination": "general",
                "content": "a cli checkpoint",
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "test" },
                "fact": null,
            }],
        })
        .to_string(),
    )
    .expect("write payload");
    path
}

#[tokio::test]
async fn a_read_only_cli_is_refused_a_write_with_the_mode_error() {
    let daemon = TestDaemon::start().await;
    let scratch = tempfile::tempdir().unwrap();
    let payload = checkpoint_payload_file(scratch.path());

    let output = memcastle(&daemon)
        .args(["--mode", "read_only", "checkpoint", "--payload"])
        .arg(&payload)
        .output()
        .await
        .expect("run memcastle");

    assert!(
        !output.status.success(),
        "a read-only session may not checkpoint"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("memcastle::app::mode_forbidden"),
        "{stderr}"
    );

    // The same command without the flag is a normal full-mode call.
    let output = memcastle(&daemon)
        .args(["checkpoint", "--payload"])
        .arg(&payload)
        .output()
        .await
        .expect("run memcastle");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn the_mode_can_also_come_from_the_environment() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .env("MEMCASTLE_MODE", "disabled")
        .args(["job", "list"])
        .output()
        .await
        .expect("run memcastle");

    assert!(
        !output.status.success(),
        "a disabled session may not read jobs"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("memcastle::app::mode_forbidden"));

    daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_mode_is_rejected_by_the_parser_and_lists_the_valid_ones() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["--mode", "readonly", "status"])
        .output()
        .await
        .expect("run memcastle");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("read_only"));

    daemon.shutdown().await;
}

#[tokio::test]
async fn mining_a_relative_path_works_when_the_daemon_runs_somewhere_else() {
    // The daemon is this test process, whose working directory is the crate
    // root; the shell that types `memcastle mine .` is standing in `project`.
    let daemon = TestDaemon::start().await;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "remember the milk").unwrap();

    let output = memcastle(&daemon)
        .current_dir(project.path())
        .args(["mine", "."])
        .output()
        .await
        .expect("run memcastle");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let submitted: Job = serde_json::from_slice(&output.stdout).expect("the job as JSON");
    let JobKind::Mine {
        source: MiningSource::Directory { path },
        ..
    } = &submitted.kind
    else {
        panic!("expected a mine job, got {:?}", submitted.kind);
    };
    assert!(
        path.is_absolute(),
        "the daemon must be sent an absolute path: {path:?}"
    );

    let client = reqwest::Client::new();
    let done = wait_for_job_status(
        &client,
        &daemon.base_url,
        submitted.id,
        JobStatus::Completed,
    )
    .await;
    assert_eq!(done.status, JobStatus::Completed);

    daemon.shutdown().await;
}

#[tokio::test]
async fn status_against_a_healthy_daemon_prints_the_report_and_exits_zero() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .arg("status")
        .output()
        .await
        .expect("run memcastle");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Exit 0 is the "healthy" half of the scripting contract.
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("MemCastle is running"), "{stdout}");
    assert!(stdout.contains(&daemon.base_url), "{stdout}");
    assert!(
        stdout.contains("from the daemon's registry file"),
        "{stdout}"
    );
    assert!(stdout.contains("/mcp"), "{stdout}");
    assert!(
        stdout.contains(&daemon.palace_path.display().to_string()),
        "{stdout}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn status_json_reports_the_endpoint_palace_and_datastore_for_scripts() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["status", "--json"])
        .output()
        .await
        .expect("run memcastle");

    assert_eq!(output.status.code(), Some(0));
    let view: serde_json::Value = serde_json::from_slice(&output.stdout).expect("status as JSON");
    assert_eq!(view["running"], true);
    // The configured address is 8420; the daemon is on an OS-assigned port,
    // so finding it proves the registry is what `status` follows.
    assert_eq!(view["endpoint"], daemon.base_url.as_str());
    assert_eq!(view["endpoint_source"], "registry");
    assert_eq!(view["mcp_url"], format!("{}/mcp", daemon.base_url));
    assert_eq!(view["registry"]["state"], "live");
    assert_eq!(
        view["palace_path"],
        daemon.palace_path.display().to_string()
    );
    assert_eq!(view["daemon"]["datastore"]["ok"], true);
    assert_eq!(view["daemon"]["datastore"]["backend"], "embedded");
    assert_eq!(
        view["daemon"]["datastore"]["pending"],
        serde_json::json!([])
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn db_start_opens_the_endpoint_tells_where_to_connect_and_db_stop_closes_it() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["db", "start", "--port", "0"])
        .output()
        .await
        .expect("run memcastle");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ws://127.0.0.1:"), "{stdout}");
    // What Studio needs to select, so the user is not left guessing.
    assert!(
        stdout.contains("memcastle") && stdout.contains("palace"),
        "{stdout}"
    );

    let status = memcastle(&daemon)
        .args(["db", "status", "--json"])
        .output()
        .await
        .expect("run memcastle");
    let report: serde_json::Value = serde_json::from_slice(&status.stdout).expect("json");
    assert_eq!(report["running"], true, "{report}");

    let stopped = memcastle(&daemon)
        .args(["db", "stop"])
        .output()
        .await
        .expect("run memcastle");
    assert!(stopped.status.success());
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("not running"));
    let status = memcastle(&daemon)
        .args(["db", "status", "--json"])
        .output()
        .await
        .expect("run memcastle");
    let report: serde_json::Value = serde_json::from_slice(&status.stdout).expect("json");
    assert_eq!(report["running"], false, "{report}");

    daemon.shutdown().await;
}

#[tokio::test]
async fn db_start_again_says_it_is_already_running_and_shows_the_same_details() {
    let daemon = TestDaemon::start().await;
    let first = memcastle(&daemon)
        .args(["db", "start", "--port", "0"])
        .output()
        .await
        .expect("run memcastle");
    assert!(first.status.success());
    let first = String::from_utf8_lossy(&first.stdout).into_owned();
    assert!(first.contains("listening on ws://127.0.0.1:"), "{first}");

    let again = memcastle(&daemon)
        .args(["db", "start"])
        .output()
        .await
        .expect("run memcastle");

    // A repeat is not a failure, or a script running it twice would break.
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let again = String::from_utf8_lossy(&again.stdout);
    assert!(
        again.contains("already running on ws://127.0.0.1:"),
        "{again}"
    );
    // Everything after the headline is what the first start printed.
    assert_eq!(
        first.lines().skip(1).collect::<Vec<_>>(),
        again.lines().skip(1).collect::<Vec<_>>()
    );
    // The URL is the same one, not a second endpoint.
    let url = |text: &str| {
        text.lines()
            .next()
            .and_then(|line| line.rsplit(' ').next())
            .map(str::to_string)
    };
    assert_eq!(url(&first), url(&again));

    daemon.shutdown().await;
}

#[tokio::test]
async fn db_start_with_settings_that_differ_from_the_running_endpoint_points_at_db_stop() {
    let daemon = TestDaemon::start().await;
    let first = memcastle(&daemon)
        .args(["db", "start", "--port", "0", "--json"])
        .output()
        .await
        .expect("run memcastle");
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).expect("json");
    let running: u16 = report["addr"]
        .as_str()
        .and_then(|addr| addr.rsplit(':').next())
        .and_then(|port| port.parse().ok())
        .expect("port");
    let other = if running == 65000 { 65001 } else { 65000 };

    let output = memcastle(&daemon)
        .args(["db", "start", "--port", &other.to_string()])
        .output()
        .await
        .expect("run memcastle");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("memcastle::db::already_running"),
        "{stderr}"
    );
    assert!(stderr.contains("memcastle db stop"), "{stderr}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn db_start_beyond_loopback_is_refused_with_a_diagnostic_that_says_what_to_do() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["db", "start", "--bind", "0.0.0.0", "--port", "0"])
        .output()
        .await
        .expect("run memcastle");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("memcastle::db::unsafe_bind"), "{stderr}");
    assert!(stderr.contains("--allow-remote"), "{stderr}");
    daemon.shutdown().await;
}

/// Submit a demo job through the CLI and return it.
async fn submit_demo(daemon: &TestDaemon, steps: &str) -> Job {
    let output = memcastle(daemon)
        .args(["job", "demo", "--steps", steps])
        .output()
        .await
        .expect("run memcastle");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the job as JSON")
}

#[tokio::test]
async fn jobs_list_prints_json_when_stdout_is_a_pipe_with_no_flag_needed() {
    let daemon = TestDaemon::start().await;
    let submitted = submit_demo(&daemon, "1").await;

    let output = memcastle(&daemon)
        .args(["job", "list"])
        .output()
        .await
        .expect("run memcastle");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains('\u{1b}'),
        "no escape codes in a pipe: {stdout:?}"
    );
    let jobs: Vec<Job> = serde_json::from_slice(&output.stdout).expect("a JSON array of jobs");
    assert!(jobs.iter().any(|job| job.id == submitted.id), "{stdout}");

    daemon.shutdown().await;
}

#[tokio::test]
async fn jobs_list_filtered_by_status_stays_valid_json_when_nothing_matches() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["job", "list", "--status", "failed"])
        .output()
        .await
        .expect("run memcastle");

    assert!(output.status.success());
    let jobs: Vec<Job> = serde_json::from_slice(&output.stdout).expect("a JSON array of jobs");
    assert!(jobs.is_empty());

    daemon.shutdown().await;
}

#[tokio::test]
async fn confirming_commands_never_prompt_without_a_terminal() {
    // A script that already decided must not block on a question nobody can
    // answer: stdin is /dev/null here, as it is in CI.
    let daemon = TestDaemon::start().await;
    let job = submit_demo(&daemon, "500").await;

    for args in [
        vec!["repair".to_string(), "--apply".to_string()],
        vec!["job".to_string(), "cancel".to_string(), job.id.to_string()],
        vec!["auth".to_string(), "revoke".to_string()],
    ] {
        let output = memcastle(&daemon)
            .args(&args)
            .output()
            .await
            .expect("run memcastle");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}: {stderr}");
        assert!(!stderr.contains("(y/n)"), "{args:?} prompted: {stderr}");
    }

    daemon.shutdown().await;
}

#[tokio::test]
async fn yes_is_accepted_everywhere_a_confirmation_would_be_asked() {
    let daemon = TestDaemon::start().await;
    let job = submit_demo(&daemon, "500").await;

    for args in [
        vec![
            "repair".to_string(),
            "--apply".to_string(),
            "--yes".to_string(),
        ],
        vec![
            "job".to_string(),
            "cancel".to_string(),
            job.id.to_string(),
            "-y".to_string(),
        ],
        vec![
            "auth".to_string(),
            "revoke".to_string(),
            "--yes".to_string(),
        ],
    ] {
        let output = memcastle(&daemon)
            .args(&args)
            .output()
            .await
            .expect("run memcastle");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_piped_status_report_has_no_escape_codes() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .arg("status")
        .output()
        .await
        .expect("run memcastle");

    assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{1b}'));

    daemon.shutdown().await;
}

/// Run `memcastle` with `args` against `daemon` and return its stdout, failing
/// the test with stderr if it did not succeed.
async fn run_ok(daemon: &TestDaemon, args: &[&str]) -> String {
    let output = memcastle(daemon)
        .args(args)
        .output()
        .await
        .expect("run memcastle");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

#[tokio::test]
async fn the_wing_room_and_drawer_commands_run_a_whole_lifecycle_and_print_json_when_piped() {
    let daemon = TestDaemon::start().await;

    let wing: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["wing", "create", "work"]).await).unwrap();
    assert_eq!(wing["created"], true);
    run_ok(&daemon, &["room", "create", "work/project-x"]).await;
    run_ok(
        &daemon,
        &[
            "drawer",
            "create",
            "work/project-x/context",
            "--content",
            "the plan",
        ],
    )
    .await;

    let wings: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["wing", "list"]).await).unwrap();
    assert_eq!(wings[0]["name"], "work");
    assert_eq!(wings[0]["drawers"], 1);

    // `wings`/`rooms`/`drawers` are the same commands.
    let rooms: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["rooms", "list", "--wing", "work"]).await).unwrap();
    assert_eq!(rooms[0]["name"], "project-x");
    let everywhere: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["room", "list"]).await).unwrap();
    assert_eq!(everywhere.as_array().unwrap().len(), 1);

    let drawers: serde_json::Value = serde_json::from_str(
        &run_ok(&daemon, &["drawer", "list", "--room", "work/project-x"]).await,
    )
    .unwrap();
    assert_eq!(drawers[0]["name"], "context");

    let shown: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["drawer", "show", "work/project-x/context"]).await)
            .unwrap();
    assert_eq!(shown["content"], "the plan");

    run_ok(
        &daemon,
        &["drawer", "delete", "work/project-x/context", "--yes"],
    )
    .await;
    run_ok(&daemon, &["room", "delete", "work/project-x", "--yes"]).await;
    let deleted: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["wing", "delete", "work", "--yes"]).await).unwrap();
    assert_eq!(deleted["wings"], 1);

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_drawer_can_be_written_from_standard_input() {
    use tokio::io::AsyncWriteExt;
    let daemon = TestDaemon::start().await;

    let mut child = memcastle(&daemon)
        .args(["drawer", "create", "w/r/piped", "--file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn memcastle");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"from a pipe")
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let shown: serde_json::Value =
        serde_json::from_str(&run_ok(&daemon, &["drawer", "show", "w/r/piped"]).await).unwrap();
    assert_eq!(shown["content"], "from a pipe");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_missing_wing_fails_with_the_daemons_own_diagnostic_and_a_bad_path_fails_locally() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["wing", "show", "nope"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("memcastle::palace::wing_not_found"));

    // Rejected before any request: `wing show` takes a bare name, not a path.
    let output = memcastle(&daemon)
        .args(["wing", "show", "a/b"])
        .output()
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("memcastle::palace::invalid_path"));
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_delete_commands_never_prompt_without_a_terminal_and_accept_yes() {
    let daemon = TestDaemon::start().await;
    for name in ["a", "b"] {
        run_ok(
            &daemon,
            &["drawer", "create", &format!("{name}/r/d"), "--content", "x"],
        )
        .await;
    }

    // No terminal: deleting proceeds without asking (as `job cancel` does).
    for args in [["drawer", "delete", "a/r/d"], ["room", "delete", "a/r"]] {
        let output = memcastle(&daemon).args(args).output().await.unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{args:?}: {stderr}");
        assert!(!stderr.contains("(y/n)"), "{args:?} prompted: {stderr}");
    }
    run_ok(&daemon, &["wing", "delete", "b", "-y"]).await;
    daemon.shutdown().await;
}
