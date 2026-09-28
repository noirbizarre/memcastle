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
        .success();
}

#[test]
fn status_without_a_reachable_daemon_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("memcastle")
        .unwrap()
        .env("MEMCASTLE_PALACE_PATH", dir.path())
        .env("MEMCASTLE_BIND", "127.0.0.1:1")
        .arg("status")
        .assert()
        .failure();
}

#[test]
fn jobs_show_rejects_a_malformed_job_id_before_ever_reaching_the_network() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .env("MEMCASTLE_BIND", "127.0.0.1:1")
        .args(["jobs", "show", "not-a-uuid"])
        .assert()
        .failure();
}
