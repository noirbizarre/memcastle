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
        .args(["jobs", "list"])
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
async fn db_serve_opens_the_endpoint_tells_where_to_connect_and_db_stop_closes_it() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["db", "serve", "--port", "0"])
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
async fn db_serve_beyond_loopback_is_refused_with_a_diagnostic_that_says_what_to_do() {
    let daemon = TestDaemon::start().await;

    let output = memcastle(&daemon)
        .args(["db", "serve", "--bind", "0.0.0.0", "--port", "0"])
        .output()
        .await
        .expect("run memcastle");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("memcastle::db::unsafe_bind"), "{stderr}");
    assert!(stderr.contains("--allow-remote"), "{stderr}");
    daemon.shutdown().await;
}
