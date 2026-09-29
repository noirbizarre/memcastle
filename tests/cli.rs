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

#[test]
fn status_without_a_reachable_daemon_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("memcastle")
        .unwrap()
        .env("MEMCASTLE_PALACE_PATH", dir.path())
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "1")
        .arg("status")
        .assert()
        .failure()
        // The diagnostic code, not just a non-zero exit: "no daemon" must be
        // reported as such, not as a generic connection error.
        .stderr(contains("memcastle::client::not_running"));
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
