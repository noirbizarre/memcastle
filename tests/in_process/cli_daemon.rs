//! The CLI against a running daemon: flags whose whole job is to change what
//! the daemon is asked, which no parse-only test can prove.
//!
//! The daemon is `TestDaemon`'s in-process task, so each CLI invocation is a
//! real subprocess (`tokio::process`, never a blocking `assert_cmd` call: the
//! daemon shares this test's runtime and a blocked thread would stall it).

use crate::common;

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
        // A developer's own project scope must not decide where a test's notes are filed.
        .env_remove("MEMCASTLE_WING")
        .env_remove("MEMCASTLE_ROOM")
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
    assert!(stderr.contains("memcastle::mode::forbidden"), "{stderr}");

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
    assert!(String::from_utf8_lossy(&output.stderr).contains("memcastle::mode::forbidden"));

    daemon.shutdown().await;
}

#[tokio::test]
async fn miner_and_trigger_commands_send_the_session_mode() {
    let daemon = TestDaemon::start().await;

    for command in [
        ["--mode", "disabled", "miner", "list", ""],
        ["--mode", "disabled", "trigger", "list", ""],
        ["--mode", "read_only", "miner", "run", "missing"],
        ["--mode", "read_only", "trigger", "fire", "missing"],
    ] {
        let output = memcastle(&daemon)
            .args(command.into_iter().filter(|arg| !arg.is_empty()))
            .output()
            .await
            .expect("run memcastle");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{command:?} should be refused");
        assert!(stderr.contains("memcastle::mode::forbidden"), "{command:?}: {stderr}");
    }

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

/// `memcastle mine <args>` from `cwd` against `daemon`, as the job it submitted.
async fn mine_job(daemon: &TestDaemon, cwd: &std::path::Path, args: &[&str]) -> Job {
    let output = memcastle(daemon)
        .current_dir(cwd)
        .arg("mine")
        .args(args)
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
async fn mining_the_directory_source_by_name_is_the_same_job_as_the_path_shorthand() {
    let daemon = TestDaemon::start().await;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "remember the milk").unwrap();

    for args in [&["directory", "."][..], &["."][..]] {
        let job = mine_job(&daemon, project.path(), args).await;
        let JobKind::Mine {
            source: MiningSource::Directory { path },
            options,
            ..
        } = &job.kind
        else {
            panic!("expected a directory mine job, got {:?}", job.kind);
        };
        assert!(path.is_absolute(), "{args:?}: {path:?}");
        assert!(options.is_empty(), "{args:?}");
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn key_value_arguments_reach_the_job_as_options() {
    let daemon = TestDaemon::start().await;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "remember the milk").unwrap();

    for args in [
        &["directory", ".", "since=2026-09"][..],
        &[".", "since=2026-09"][..],
        // Order is free: the place may come after the options.
        &["directory", "since=2026-09", "."][..],
    ] {
        let job = mine_job(&daemon, project.path(), args).await;
        let JobKind::Mine { options, .. } = &job.kind else {
            panic!("expected a mine job, got {:?}", job.kind);
        };
        assert_eq!(options["since"], "2026-09", "{args:?}");
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_option_is_refused_by_the_daemon_and_names_what_is_accepted() {
    let daemon = TestDaemon::start().await;
    let output = memcastle(&daemon)
        .args(["mine", "directory", ".", "dates=2026-09"])
        .output()
        .await
        .expect("run memcastle");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown option") && stderr.contains("since"),
        "{stderr}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_word_that_is_neither_a_source_nor_a_directory_lists_the_sources() {
    let daemon = TestDaemon::start().await;
    let output = memcastle(&daemon)
        .args(["mine", "no-such-thing"])
        .output()
        .await
        .expect("run memcastle");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("neither a source nor a directory") && stderr.contains("directory"),
        "{stderr}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_old_source_and_locator_flags_are_gone() {
    let daemon = TestDaemon::start().await;
    let output = memcastle(&daemon)
        .args(["mine", "--source", "pi"])
        .output()
        .await
        .expect("run memcastle");
    assert!(
        !output.status.success(),
        "`--source` was removed: the source is the first word"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_piped_status_against_a_healthy_daemon_is_json_without_asking_and_exits_zero() {
    let daemon = TestDaemon::start().await;

    let piped = memcastle(&daemon)
        .arg("status")
        .output()
        .await
        .expect("run memcastle");
    let forced = memcastle(&daemon)
        .args(["status", "--json"])
        .output()
        .await
        .expect("run memcastle");

    // Exit 0 is the "healthy" half of the scripting contract, whichever form is printed.
    assert_eq!(piped.status.code(), Some(0));
    let mut view: serde_json::Value =
        serde_json::from_slice(&piped.stdout).expect("status as JSON");
    assert_eq!(view["running"], true);
    assert_eq!(view["endpoint"], daemon.base_url.as_str());
    // Asking for JSON on a pipe changes nothing: the stream already decided. The uptime is the one field that
    // moves between two calls.
    let mut asked: serde_json::Value =
        serde_json::from_slice(&forced.stdout).expect("status as JSON");
    view["daemon"]["uptime_secs"] = serde_json::Value::Null;
    asked["daemon"]["uptime_secs"] = serde_json::Value::Null;
    assert_eq!(view, asked);

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
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert!(
        started["url"]
            .as_str()
            .is_some_and(|url| url.starts_with("ws://127.0.0.1:")),
        "{started}"
    );
    // What Studio needs to select, so the user is not left guessing.
    assert_eq!(started["namespace"], "memcastle", "{started}");
    assert_eq!(started["database"], "palace", "{started}");

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
    let stopped: serde_json::Value = serde_json::from_slice(&stopped.stdout).expect("json");
    assert_eq!(stopped["running"], false, "{stopped}");
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
    let mut first: serde_json::Value = serde_json::from_slice(&first.stdout).expect("json");
    assert_eq!(first["running"], true, "{first}");
    assert_eq!(first["already_running"], false, "{first}");

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
    let mut again: serde_json::Value = serde_json::from_slice(&again.stdout).expect("json");
    assert_eq!(again["already_running"], true, "{again}");
    // The same endpoint, not a second one: only the repeat marker differs.
    first["already_running"] = serde_json::Value::Null;
    again["already_running"] = serde_json::Value::Null;
    assert_eq!(first, again);

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
async fn every_command_with_a_data_answer_prints_json_with_no_escape_codes_in_a_pipe() {
    let daemon = TestDaemon::start().await;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "remember the milk").unwrap();
    let job = submit_demo(&daemon, "1").await;
    let job_id = job.id.to_string();

    // Colour forced on: it must still never reach a pipe, because the pretty form is the only one that has any.
    for args in [
        vec!["status"],
        vec!["db", "status"],
        vec!["job", "list"],
        vec!["job", "show", &job_id],
        vec!["audit"],
        vec!["search", "milk"],
        vec!["recall", "milk"],
        vec!["wake-up", "--agent-identity", "tester"],
        vec!["diary", "read", "--agent-identity", "tester", "--wing", "w"],
        vec!["sources"],
        vec!["wing", "list"],
    ] {
        let output = memcastle(&daemon)
            .env("FORCE_COLOR", "1")
            .args(&args)
            .output()
            .await
            .expect("run memcastle");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains('\u{1b}'), "{args:?}: {stdout:?}");
        serde_json::from_str::<serde_json::Value>(&stdout)
            .unwrap_or_else(|error| panic!("{args:?} is not JSON ({error}): {stdout}"));
    }

    // `mine` is the command that started this: it used to be the one that printed JSON in a terminal.
    let mined = memcastle(&daemon)
        .env("FORCE_COLOR", "1")
        .current_dir(project.path())
        .args(["mine", "."])
        .output()
        .await
        .expect("run memcastle");
    serde_json::from_slice::<Job>(&mined.stdout).expect("mine prints the job as JSON in a pipe");

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
    assert!(String::from_utf8_lossy(&output.stderr).contains("memcastle::palace::path_invalid"));
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

/// A directory named `name` under a fresh temp dir, which `memcastle note` stands in: its name is the fallback wing.
fn project_named(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    // The CLI canonicalises its working directory, so the expected locator is the canonical one.
    let dir = dir.canonicalize().unwrap();
    (root, dir)
}

/// Run `memcastle note <args>` standing in `dir`, with `input` on standard input when given.
async fn note_in(
    daemon: &TestDaemon,
    dir: &std::path::Path,
    args: &[&str],
    input: Option<&str>,
    configure: impl FnOnce(&mut Command),
) -> std::process::Output {
    use tokio::io::AsyncWriteExt;
    let mut command = memcastle(daemon);
    command
        .current_dir(dir)
        .arg("note")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    if input.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().expect("spawn memcastle");
    if let Some(input) = input {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).await.unwrap();
        // Dropping closes the pipe, which is what ends `read_to_string`.
        drop(stdin);
    }
    child.wait_with_output().await.unwrap()
}

/// The confirmation JSON of a note that must have succeeded.
fn confirmed(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the confirmation as JSON")
}

/// The drawer `memcastle drawer show <wing>/<room>/<id>` finds, which proves where a note was filed.
async fn shown(
    daemon: &TestDaemon,
    wing: &str,
    room: &str,
    note: &serde_json::Value,
) -> serde_json::Value {
    let path = format!("{wing}/{room}/{}", note["id"].as_str().unwrap());
    serde_json::from_str(&run_ok(daemon, &["drawer", "show", &path]).await).unwrap()
}

#[tokio::test]
async fn a_short_note_is_filed_under_the_directory_name_with_its_provenance() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("keep");

    let output = note_in(&daemon, &dir, &["buy", "oat milk"], None, |_| {}).await;
    let note = confirmed(&output);

    assert_eq!(note["created"], true);
    assert!(
        uuid::Uuid::parse_str(note["id"].as_str().unwrap()).is_ok(),
        "the confirmation carries a stable id: {note}"
    );
    let stored = shown(&daemon, "keep", "notes", &note).await;
    assert_eq!(
        stored["content"], "buy oat milk",
        "words are joined as typed"
    );
    assert_eq!(stored["source"]["kind"], "note");
    assert_eq!(stored["source"]["uri"], dir.display().to_string());
    assert_eq!(stored["provenance"]["requested_by"], "cli");
    assert!(stored["created_at"].is_string());
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_multiline_note_on_standard_input_is_kept_verbatim_without_its_final_newline() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("keep");
    let text = "Meeting with Ada\n\n  - ship the CLI\n  - write docs\n";

    let note = confirmed(&note_in(&daemon, &dir, &[], Some(text), |_| {}).await);

    let stored = shown(&daemon, "keep", "notes", &note).await;
    assert_eq!(
        stored["content"],
        "Meeting with Ada\n\n  - ship the CLI\n  - write docs"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_note_can_be_read_from_a_file_or_from_standard_input_by_name() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("keep");
    std::fs::write(dir.join("draft.txt"), "from a file\nsecond line\n").unwrap();

    let from_file =
        confirmed(&note_in(&daemon, &dir, &["--file", "draft.txt"], None, |_| {}).await);
    let from_dash = confirmed(
        &note_in(
            &daemon,
            &dir,
            &["--file", "-"],
            Some("from a pipe\n"),
            |_| {},
        )
        .await,
    );

    assert_eq!(
        shown(&daemon, "keep", "notes", &from_file).await["content"],
        "from a file\nsecond line"
    );
    assert_eq!(
        shown(&daemon, "keep", "notes", &from_dash).await["content"],
        "from a pipe"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_empty_note_is_refused_before_the_daemon_is_asked() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("keep");

    let output = note_in(&daemon, &dir, &[], Some("  \n\n"), |_| {}).await;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("nothing to save"));
    let wings = run_ok(&daemon, &["wing", "list"]).await;
    assert_eq!(wings.trim(), "[]", "nothing was stored: {wings}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_project_file_decides_the_wing_and_room_even_from_a_nested_directory() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("checkout");
    std::fs::create_dir_all(dir.join(".config")).unwrap();
    std::fs::write(
        dir.join(".config/memcastle.toml"),
        "[memcastle]\nwing = \"castle\"\nroom = \"design\"\n",
    )
    .unwrap();
    let nested = dir.join("src/deep");
    std::fs::create_dir_all(&nested).unwrap();

    let note = confirmed(&note_in(&daemon, &nested, &["use a queue"], None, |_| {}).await);

    // Filed under the project's scope, not under the nested directory's name.
    let stored = shown(&daemon, "castle", "design", &note).await;
    assert_eq!(stored["content"], "use a queue");
    assert_eq!(stored["source"]["uri"], nested.display().to_string());
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_environment_beats_the_project_file_and_flags_beat_both() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("checkout");
    std::fs::create_dir_all(dir.join(".config")).unwrap();
    std::fs::write(
        dir.join(".config/memcastle.toml"),
        "[memcastle]\nwing = \"castle\"\nroom = \"design\"\n",
    )
    .unwrap();

    let from_env = confirmed(
        &note_in(&daemon, &dir, &["env note"], None, |command| {
            command
                .env("MEMCASTLE_WING", "ci")
                .env("MEMCASTLE_ROOM", "scratch");
        })
        .await,
    );
    assert_eq!(
        shown(&daemon, "ci", "scratch", &from_env).await["content"],
        "env note"
    );

    let from_flags = confirmed(
        &note_in(
            &daemon,
            &dir,
            &["--wing", "flagged", "--room", "inbox", "flag note"],
            None,
            |command| {
                command.env("MEMCASTLE_WING", "ci");
            },
        )
        .await,
    );
    assert_eq!(
        shown(&daemon, "flagged", "inbox", &from_flags).await["content"],
        "flag note"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_broken_project_file_is_an_error_naming_it_unless_both_flags_state_the_destination() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("checkout");
    std::fs::create_dir_all(dir.join(".config")).unwrap();
    std::fs::write(
        dir.join(".config/memcastle.toml"),
        "[memcastle]\nwng = \"typo\"\n",
    )
    .unwrap();

    let refused = note_in(&daemon, &dir, &["lost?"], None, |_| {}).await;
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("memcastle::project::invalid"), "{stderr}");
    assert!(stderr.contains("memcastle.toml"), "{stderr}");

    let stated = note_in(
        &daemon,
        &dir,
        &["--wing", "w", "--room", "r", "stated outright"],
        None,
        |_| {},
    )
    .await;
    shown(&daemon, "w", "r", &confirmed(&stated)).await;
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_same_note_twice_reports_the_one_already_captured() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("keep");

    let first = confirmed(&note_in(&daemon, &dir, &["call the plumber"], None, |_| {}).await);
    let second = confirmed(&note_in(&daemon, &dir, &["call the plumber"], None, |_| {}).await);

    assert_eq!(first["created"], true);
    assert_eq!(second["created"], false);
    assert_eq!(second["id"], first["id"]);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_cli_cannot_capture_a_note() {
    let daemon = TestDaemon::start().await;
    let (_root, dir) = project_named("keep");

    let mut command = memcastle(&daemon);
    let output = command
        .current_dir(&dir)
        .args(["--mode", "read_only", "note", "never stored"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("memcastle::mode::forbidden"));
    daemon.shutdown().await;
}

#[cfg(unix)]
mod editor {
    use super::*;

    /// A command line for `$EDITOR` that overwrites the file it is handed with `body`: run through `sh`, so the
    /// script file itself is never executed (a freshly written executable can be "text file busy" under load).
    fn editor_writing(dir: &std::path::Path, body: &str) -> String {
        let script = dir.join("editor.sh");
        std::fs::write(&script, format!("printf '%s' '{body}' > \"$1\"\n")).unwrap();
        format!("sh {}", script.display())
    }

    #[tokio::test]
    async fn a_note_written_in_the_editor_is_saved() {
        let daemon = TestDaemon::start().await;
        let (root, dir) = project_named("keep");
        let editor = editor_writing(root.path(), "from the editor\nsecond line\n");

        let output = note_in(&daemon, &dir, &["--edit"], None, |command| {
            command.env("EDITOR", &editor).env_remove("VISUAL");
        })
        .await;

        let note = confirmed(&output);
        assert_eq!(
            shown(&daemon, "keep", "notes", &note).await["content"],
            "from the editor\nsecond line"
        );
        daemon.shutdown().await;
    }

    #[tokio::test]
    async fn the_visual_editor_wins_over_the_editor_and_an_empty_result_saves_nothing() {
        let daemon = TestDaemon::start().await;
        let (root, dir) = project_named("keep");
        let blank = editor_writing(root.path(), "\n");

        let output = note_in(&daemon, &dir, &["--edit"], None, |command| {
            command.env("VISUAL", &blank).env("EDITOR", "false");
        })
        .await;

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("nothing to save"));
        assert_eq!(run_ok(&daemon, &["wing", "list"]).await.trim(), "[]");
        daemon.shutdown().await;
    }

    #[tokio::test]
    async fn without_an_editor_the_error_says_how_to_configure_one() {
        let daemon = TestDaemon::start().await;
        let (_root, dir) = project_named("keep");

        let output = note_in(&daemon, &dir, &["--edit"], None, |command| {
            command.env_remove("VISUAL").env_remove("EDITOR");
        })
        .await;

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("$VISUAL"));
        daemon.shutdown().await;
    }
}

/// Run `memcastle <args>` and return its standard output, asserting it succeeded.
async fn stdout_of(daemon: &TestDaemon, args: &[&str]) -> String {
    let output = memcastle(daemon)
        .args(args)
        .output()
        .await
        .expect("run memcastle");
    assert!(
        output.status.success(),
        "memcastle {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn hit_contents(stdout: &str) -> Vec<String> {
    let hits: serde_json::Value = serde_json::from_str(stdout).expect("search prints JSON");
    let mut found: Vec<String> = hits
        .as_array()
        .expect("hits")
        .iter()
        .map(|hit| hit["content"].as_str().expect("content").to_string())
        .collect();
    found.sort();
    found
}

#[tokio::test]
async fn search_takes_a_date_an_interval_and_drawer_history_follows_the_chain() {
    let daemon = TestDaemon::start().await;
    stdout_of(
        &daemon,
        &[
            "drawer",
            "create",
            "w/r/db",
            "--content",
            "we run the database on postgres",
        ],
    )
    .await;
    stdout_of(
        &daemon,
        &[
            "drawer",
            "supersede",
            "w/r/db",
            "--content",
            "we run the database on surrealdb",
        ],
    )
    .await;

    // The issue's own example: a date, read as midnight UTC. Long ago nothing was recorded; far ahead
    // the open-ended latest belief is the one that is true.
    let long_ago = stdout_of(&daemon, &["search", "database", "--as-of", "2000-01-01"]).await;
    assert!(hit_contents(&long_ago).is_empty(), "{long_ago}");
    let ahead = stdout_of(&daemon, &["search", "database", "--as-of", "2999-01-01"]).await;
    assert_eq!(hit_contents(&ahead), ["we run the database on surrealdb"]);

    // An interval spanning the correction sees both, through search and recall alike.
    for command in ["search", "recall"] {
        let across = stdout_of(
            &daemon,
            &[
                command,
                "database",
                "--from",
                "2000-01-01",
                "--until",
                "2999-01-01",
            ],
        )
        .await;
        assert_eq!(
            hit_contents(&across),
            [
                "we run the database on postgres",
                "we run the database on surrealdb"
            ],
            "{command}"
        );
    }

    // History starts from the name, which now belongs to the replacement, and returns both versions oldest first.
    let history = stdout_of(&daemon, &["drawer", "history", "w/r/db"]).await;
    let history: serde_json::Value = serde_json::from_str(&history).expect("history prints JSON");
    let versions = history["versions"].as_array().expect("versions");
    assert_eq!(versions[0]["content"], "we run the database on postgres");
    assert_eq!(versions[1]["content"], "we run the database on surrealdb");
    assert_eq!(versions[0]["valid_to"], versions[1]["valid_from"]);

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_half_given_or_contradictory_interval_is_refused_before_asking_the_daemon() {
    let daemon = TestDaemon::start().await;

    for args in [
        vec!["search", "x", "--from", "2026-01-01"],
        vec!["search", "x", "--until", "2026-01-01"],
        vec![
            "search",
            "x",
            "--from",
            "2026-01-01",
            "--until",
            "2026-02-01",
            "--as-of",
            "2026-01-15",
        ],
        vec![
            "search",
            "x",
            "--from",
            "2026-01-01",
            "--until",
            "2026-02-01",
            "--include-historical",
        ],
    ] {
        let output = memcastle(&daemon)
            .args(&args)
            .output()
            .await
            .expect("run memcastle");
        assert!(!output.status.success(), "{args:?} should be refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--"),
            "{args:?} should name a flag: {stderr}"
        );
    }

    // A reversed interval gets past the flag rules and is refused with the diagnostic code.
    let output = memcastle(&daemon)
        .args([
            "search",
            "x",
            "--from",
            "2026-02-01",
            "--until",
            "2026-01-01",
        ])
        .output()
        .await
        .expect("run memcastle");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("memcastle::input::invalid"), "{stderr}");

    daemon.shutdown().await;
}
