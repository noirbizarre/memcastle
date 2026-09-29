//! CLI smoke tests: the binary parses its documented commands and behaves
//! sensibly with no daemon around.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_the_top_level_commands() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("serve"))
        .stdout(contains("mine"))
        .stdout(contains("audit"))
        .stdout(contains("repair"))
        .stdout(contains("jobs"))
        .stdout(contains("recall"))
        .stdout(contains("wake-up"));
}

#[test]
fn serve_help_documents_the_daemon_alias() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(contains("daemon"));
}

/// A `memcastle status` with no daemon, pointed at an empty palace and a
/// state directory of its own so the developer's real registry is never read.
fn status_without_a_daemon(state: &tempfile::TempDir, palace: &tempfile::TempDir) -> Command {
    let mut command = Command::cargo_bin("memcastle").unwrap();
    command
        .env("MEMCASTLE_PALACE_PATH", palace.path())
        .env("XDG_STATE_HOME", state.path())
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "1")
        .arg("status");
    command
}

#[test]
fn status_without_a_reachable_daemon_reports_not_running_and_exits_3() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    status_without_a_daemon(&state, &palace)
        .assert()
        // 3, not 1: a script must be able to tell "stopped" from "broken".
        .code(3)
        .stdout(contains("MemCastle is not running"))
        .stdout(contains("127.0.0.1:1"))
        .stdout(contains("memcastle serve"));
}

#[test]
fn status_json_without_a_daemon_says_running_false_and_still_names_the_palace() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let output = status_without_a_daemon(&state, &palace)
        .arg("--json")
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();

    let view: serde_json::Value = serde_json::from_slice(&output).expect("status as JSON");
    assert_eq!(view["running"], false);
    assert_eq!(view["endpoint"], "http://127.0.0.1:1");
    assert_eq!(view["endpoint_source"], "config");
    assert_eq!(view["registry"]["state"], "absent");
    assert_eq!(view["palace_path"], palace.path().display().to_string());
    assert!(view["daemon"].is_null());
}

// Liveness is only probed on Unix; elsewhere a registry file is trusted as live.
#[cfg(unix)]
#[test]
fn status_diagnoses_a_registry_file_left_behind_by_a_killed_daemon() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // A PID that cannot be alive: above any Linux `pid_max` (2^22).
    let dead_pid = 4_194_305_u32;
    let registry = memcastle::server::lifecycle::RuntimeInfo {
        pid: dead_pid,
        bind_addr: "127.0.0.1:1".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        version: "0.0.0".to_string(),
    };
    // Written through the same path logic the binary uses, under the
    // subprocess's `XDG_STATE_HOME`, so it lands where `status` will look.
    let file = registry_file_under(state.path(), palace.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, serde_json::to_string(&registry).unwrap()).unwrap();

    status_without_a_daemon(&state, &palace)
        .assert()
        .code(3)
        .stdout(contains("stale"))
        .stdout(contains(dead_pid.to_string()));
}

#[cfg(unix)]
/// Where `memcastle` will look for `palace`'s registry when its
/// `XDG_STATE_HOME` is `state` — see `server::lifecycle::registry_path`.
fn registry_file_under(state: &std::path::Path, palace: &std::path::Path) -> std::path::PathBuf {
    let canonical = std::fs::canonicalize(palace).unwrap();
    let digest = memcastle::domain::sha256_hex(canonical.display().to_string().as_bytes());
    state
        .join("memcastle")
        .join("run")
        .join(&digest[..16])
        .join("daemon.json")
}

#[test]
fn jobs_show_rejects_a_malformed_job_id_before_ever_reaching_the_network() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "1")
        .args(["jobs", "show", "not-a-uuid"])
        .assert()
        .failure()
        // The diagnostic code, not just a non-zero exit: a malformed id must
        // be reported as such, not as a job that "was not found".
        .stderr(contains("memcastle::jobs::invalid_id"));
}

#[test]
fn a_reserved_but_unimplemented_command_says_so_instead_of_blaming_the_config() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .arg("wings")
        .assert()
        .failure()
        .stderr(contains("memcastle::cli::not_implemented"));
}
