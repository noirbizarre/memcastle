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
    // The process exits normally after each run, which flushes the database,
    // so relaxed syncing cannot lose what these tests read back; it only
    // spares each run a flush per commit (seconds on a slow disk).
    cmd.env("MEMCASTLE_PALACE_PATH", palace)
        .env("MEMCASTLE_STORE_SYNC", "never");
    cmd
}

/// Parse a successful invocation's stdout as the JSON `print_json` prints.
fn json_stdout(assert: Assert) -> Value {
    let output = assert.get_output().stdout.clone();
    serde_json::from_slice(&output).expect("stdout should be a JSON report/status")
}

#[test]
fn migrate_against_a_fresh_palace_applies_every_shipped_data_migration_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let assert = migrate_cmd(dir.path()).arg("migrate").assert().success();
    let report = json_stdout(assert);
    assert_eq!(report["from_version"], 0);
    let applied = report["applied"].as_array().expect("applied is a list");
    assert!(
        !applied.is_empty(),
        "a fresh palace is behind every shipped migration, got {report:?}"
    );
    // Versions are sequential from 1, so the watermark is the count applied.
    assert_eq!(report["to_version"], applied.len());
}

#[test]
fn migrate_status_against_a_never_migrated_palace_is_read_only_and_lists_what_is_pending() {
    // No prior `memcastle migrate` call: `--status` must still work on a
    // genuinely fresh palace (`migration_version` tolerates the
    // `migration_state` table not existing yet — see
    // `store::migration_state::is_table_not_found`), not error or require
    // migrating first just to ask a question.
    let dir = tempfile::tempdir().expect("tempdir");
    let first = json_stdout(
        migrate_cmd(dir.path())
            .args(["migrate", "--status"])
            .assert()
            .success(),
    );
    assert_eq!(first["current_version"], 0);
    assert_eq!(
        first["pending"]
            .as_array()
            .expect("pending is a list")
            .len(),
        first["latest_version"]
            .as_u64()
            .expect("latest is a number") as usize,
        "every shipped migration is pending on a fresh palace, got {first:?}"
    );

    // Asking must not have migrated anything.
    let second = json_stdout(
        migrate_cmd(dir.path())
            .args(["migrate", "--status"])
            .assert()
            .success(),
    );
    assert_eq!(second, first);
}

#[test]
fn migrate_check_fails_while_migrations_are_pending_and_passes_once_they_are_applied() {
    let dir = tempfile::tempdir().expect("tempdir");
    migrate_cmd(dir.path())
        .args(["migrate", "--check"])
        .assert()
        .failure();

    migrate_cmd(dir.path()).arg("migrate").assert().success();

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
