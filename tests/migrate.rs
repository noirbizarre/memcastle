//! `memcastle migrate` integration tests.
//!
//! `migrate` is the CLI's second entry point into `crate::migrate::run`/
//! `status`, alongside `server::run` — see `memcastle::migrate`'s module
//! doc. Like `serve`, it connects to storage directly, so every test here
//! runs with no daemon involved at all: a fresh tempdir palace, the
//! `memcastle` binary invoked as a genuine subprocess (proving the CLI
//! wiring end to end, not just the library function).

use assert_cmd::Command;
use assert_cmd::assert::Assert;
use serde_json::Value;

/// A `memcastle` invocation scoped to `palace` — no `MEMCASTLE_BIND` needed,
/// since `migrate` never binds an HTTP listener.
fn migrate_cmd(palace: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("memcastle").expect("find the memcastle binary");
    cmd.env("MEMCASTLE_PALACE_PATH", palace);
    cmd
}

/// Parse a successful invocation's stdout as the JSON `print_json` prints.
fn json_stdout(assert: Assert) -> Value {
    let output = assert.get_output().stdout.clone();
    serde_json::from_slice(&output).expect("stdout should be a JSON report/status")
}

#[test]
fn migrate_against_a_fresh_palace_reports_no_pending_data_migrations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let assert = migrate_cmd(dir.path()).arg("migrate").assert().success();
    let report = json_stdout(assert);
    assert_eq!(report["from_version"], 0);
    assert_eq!(report["to_version"], 0);
    assert_eq!(
        report["applied"],
        serde_json::json!([]),
        "no MemCastle data migrations are shipped yet, got {report:?}"
    );
}

#[test]
fn migrate_status_against_a_never_migrated_palace_is_read_only_and_reports_up_to_date() {
    // No prior `memcastle migrate` call: `--status` must still work on a
    // genuinely fresh palace (`migration_version` tolerates the
    // `migration_state` table not existing yet — see
    // `store::migration_state::is_table_not_found`), not error or require
    // migrating first just to ask a question.
    let dir = tempfile::tempdir().expect("tempdir");
    let assert = migrate_cmd(dir.path())
        .args(["migrate", "--status"])
        .assert()
        .success();
    let status = json_stdout(assert);
    assert_eq!(
        status["current_version"], status["latest_version"],
        "an up-to-date palace should report equal current/latest versions, got {status:?}"
    );
    assert_eq!(status["pending"], serde_json::json!([]));
}

#[test]
fn migrate_check_on_an_up_to_date_palace_exits_zero_without_mutating() {
    let dir = tempfile::tempdir().expect("tempdir");
    migrate_cmd(dir.path())
        .args(["migrate", "--check"])
        .assert()
        .success();
}

#[test]
fn running_migrate_twice_in_a_row_against_the_same_palace_both_succeed() {
    let dir = tempfile::tempdir().expect("tempdir");
    migrate_cmd(dir.path()).arg("migrate").assert().success();
    // The second run must be just as clean a no-op as the schema-sync side
    // already was before this feature existed — nothing pending, nothing
    // to break.
    let assert = migrate_cmd(dir.path()).arg("migrate").assert().success();
    let report = json_stdout(assert);
    assert_eq!(report["applied"], serde_json::json!([]));
}

#[test]
fn migration_state_persists_across_separate_process_invocations_against_the_same_palace() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Round 1: a fresh process runs the real migration.
    let assert = migrate_cmd(dir.path()).arg("migrate").assert().success();
    let first = json_stdout(assert);

    // Round 2: a brand-new process, same palace directory — must read back
    // the version the first process recorded, not start over at a
    // different value. This is the "persistence across close/reopen"
    // acceptance criterion: SurrealDB's embedded engine doesn't release its
    // on-disk lock within one process (see `store::mod`'s test comment),
    // so proving this genuinely requires two separate processes, exactly
    // like `tests/persistence.rs` does for drawer content.
    let assert = migrate_cmd(dir.path())
        .args(["migrate", "--status"])
        .assert()
        .success();
    let status = json_stdout(assert);
    assert_eq!(status["current_version"], first["to_version"]);
}

#[test]
fn check_and_status_are_mutually_exclusive() {
    let dir = tempfile::tempdir().expect("tempdir");
    migrate_cmd(dir.path())
        .args(["migrate", "--check", "--status"])
        .assert()
        .failure();
}
