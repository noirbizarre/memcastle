//! The one migration runner, used identically by `server::run` (daemon
//! startup) and `memcastle migrate` (an explicit CLI entry point that
//! connects to storage directly — see `main.rs`'s `cmd_migrate`, a second,
//! narrow exception to "the CLI only calls `client::DaemonClient`" for the
//! same reason `serve` already is one: migration must work without, and
//! before, a daemon exists).
//!
//! See `docs/adr/004-versioned-database-migrations.md` for the full design.
//! Two migration shapes, kept separate:
//!
//! - Schema: declarative `.surql` files under `database/schema/`, embedded
//!   into the binary and applied through SurrealKit's `Sync` (see
//!   `store::mod`'s `SurrealStore::sync_schema`) — synced first (so the
//!   bookkeeping table exists before the lock is taken) and again in [`run`]
//!   after any data migration that ran, never gated by the version watermark below, since
//!   SurrealKit's own content-hash tracking already makes re-applying an
//!   unchanged file a no-op. MemCastle does not reimplement that diffing.
//! - Data migrations ([`DataMigration`]): versioned Rust steps for a
//!   change that can't be expressed that way (a rename, reshape,
//!   split/merge, or backfill). Ordered, immutable once released, and
//!   gated by the watermark `store::migration_state` tracks, so each one
//!   runs exactly once.

mod dedup_keys;
mod diary_provenance;
mod timestamps;

use std::future::Future;
use std::pin::Pin;

use chrono::Duration;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::store::SurrealStore;

/// How long a migration run's lock lease lasts before a later run may
/// reclaim it. Generous relative to any migration this codebase ships
/// today: the embedded backend's own SurrealKV file lock is the primary
/// single-writer guarantee (a second process can't even open the same
/// palace path — see `store::mod`'s module doc); this lease is a defensive
/// net for a future remote backend where multiple processes genuinely can
/// connect concurrently, not a mechanism a normal run should ever race
/// against its own lease. A migration that legitimately needs longer than
/// this is a signal to add heartbeat renewal, not to raise this
/// unboundedly.
const LOCK_LEASE: Duration = Duration::minutes(5);

/// A future returned by a [`DataMigration::apply`] step.
type MigrationFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// The function shape every data migration step implements.
pub type MigrationFn = for<'a> fn(&'a SurrealStore) -> MigrationFuture<'a>;

/// One versioned, immutable data migration step — a change that can't be
/// expressed as an additive `DEFINE ... IF NOT EXISTS`. Ordered by
/// `version`; [`run`] applies every step whose `version` is greater than
/// the store's current recorded version, in ascending order, recording the
/// new watermark after each one succeeds so a later run resumes from the
/// right place instead of re-applying already-completed work.
///
/// `version` and `apply` are immutable once released — see the module doc.
/// Fixing a mistake in a shipped migration means writing a new migration
/// that corrects it, never editing this one.
pub struct DataMigration {
    /// Sequential and immutable once released.
    pub version: u32,
    /// A short, human-readable name for logs and [`MigrationStatus::pending`]
    /// — purely diagnostic, never parsed.
    pub name: &'static str,
    /// The transformation itself. Must be deterministic and, where
    /// practical, safe to re-run (idempotent) — see the module doc.
    pub apply: MigrationFn,
}

/// Every data migration shipped so far, in release order. This is where the
/// next one gets appended, never inserted before an existing entry and never
/// edited in place once released. Each step lives in its own submodule.
const DATA_MIGRATIONS: &[DataMigration] = &[
    DataMigration {
        version: 1,
        name: "diary-provenance",
        apply: diary_provenance::apply,
    },
    DataMigration {
        version: 2,
        name: "canonical-timestamps",
        apply: timestamps::apply,
    },
    DataMigration {
        version: 3,
        name: "dedup-keys",
        apply: dedup_keys::apply,
    },
];

/// What [`run`] actually did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    /// The version this palace was at before this run.
    pub from_version: u32,
    /// The version it's at now (equal to `from_version` if nothing was pending).
    pub to_version: u32,
    /// The names of the steps this run actually applied, in order.
    pub applied: Vec<String>,
}

/// What [`status`] reports — a read-only snapshot, never a mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStatus {
    /// The palace's current recorded version.
    pub current_version: u32,
    /// The highest version this binary knows about.
    pub latest_version: u32,
    /// The names of the steps that would run if [`run`] were called now.
    pub pending: Vec<String>,
}

/// Bring `store` up to the latest known data-migration version: sync the
/// declarative schema (so the lock table exists), acquire the exclusive
/// migration lock, apply every pending step in order, re-synchronize the
/// schema if any step ran, then release the lock. The same
/// function `server::run` calls on every daemon startup and `memcastle
/// migrate` calls directly — there is exactly one runner.
///
/// # Errors
///
/// Returns [`Error::MigrationLocked`] if another run already holds the
/// lock, or [`Error::MigrationFailed`] if a step's own transformation
/// fails — the version watermark is left at the last step that succeeded,
/// so a later, corrected run resumes rather than replays.
pub async fn run(store: &SurrealStore) -> Result<MigrationReport> {
    run_with(store, DATA_MIGRATIONS).await
}

/// Report the current/pending version without applying anything. Never
/// creates the `migration_state` row, never acquires the lock, never
/// writes anything — backs both `memcastle migrate --check` and
/// `--status`.
///
/// # Errors
///
/// Returns an error if the store cannot be read.
pub async fn status(store: &SurrealStore) -> Result<MigrationStatus> {
    status_with(store, DATA_MIGRATIONS).await
}

/// A fresh, unique identifier for whichever process/call is currently
/// trying to hold the migration lock — not persisted or parsed, only
/// compared for equality by `store::migration_state`.
fn lock_owner() -> String {
    format!("pid:{}:{}", std::process::id(), Uuid::new_v4())
}

/// [`run`], parameterized over an explicit migration list — this module's
/// own tests exercise the runner's behaviour (ordering, resumability,
/// idempotence, lock contention) with disposable fixture steps instead of
/// waiting for a real one to exist.
async fn run_with(store: &SurrealStore, migrations: &[DataMigration]) -> Result<MigrationReport> {
    // A real (non-dry-run) schema sync up front, before anything else: on a
    // genuinely fresh palace `migration_state` itself doesn't exist yet
    // (`SurrealStore::connect` no longer syncs schema — see its doc), and
    // this bookkeeping table has to exist before `ensure_migration_state`/
    // the lock/the watermark can be read or written at all. Harmless to
    // repeat here even on an already-migrated palace (idempotent), and
    // distinct in purpose from the call inside `apply_pending` (which
    // re-syncs *after* data migrations, per the ADR's ordering, to pick up
    // any schema growth this release also shipped — only if one ran).
    store.sync_schema().await?;
    store.ensure_migration_state().await?;

    let owner = lock_owner();
    if !store.try_acquire_migration_lock(&owner, LOCK_LEASE).await? {
        return Err(Error::migration_locked("another migration run"));
    }

    let result = apply_pending(store, migrations).await;
    // Best-effort, always attempted (including on the error path): a stuck
    // lock would otherwise defeat the whole point of the lease, and this
    // call is itself a no-op if `owner` no longer holds it (e.g. the lease
    // already expired and was reclaimed by someone else).
    let _ = store.release_migration_lock(&owner).await;

    result
}

/// The locked section of [`run_with`]: read the watermark, apply every
/// pending step in ascending version order, recording progress after each
/// one succeeds, then re-sync the declarative schema.
async fn apply_pending(
    store: &SurrealStore,
    migrations: &[DataMigration],
) -> Result<MigrationReport> {
    let from_version = store.migration_version().await?;
    let mut applied = Vec::new();

    let mut pending: Vec<&DataMigration> = migrations
        .iter()
        .filter(|step| step.version > from_version)
        .collect();
    pending.sort_by_key(|step| step.version);

    for step in pending {
        (step.apply)(store).await.map_err(|source| {
            Error::migration_failed(step.version, step.name, source.to_string())
        })?;
        store.record_migration_version(step.version).await?;
        applied.push(step.name.to_string());
    }

    // Only re-synced after a step ran: `run_with` synced just before taking the
    // lock, so with nothing applied the schema is already current. Repeating it
    // is not free — SurrealKit rewrites its setup and entity catalogue on every
    // call, tens of separately committed (and, on the embedded backend, fsynced)
    // statements that dominated a no-op boot on Windows.
    if !applied.is_empty() {
        store.sync_schema().await?;
    }

    let to_version = store.migration_version().await?;
    Ok(MigrationReport {
        from_version,
        to_version,
        applied,
    })
}

/// [`status`], parameterized over an explicit migration list — see
/// `run_with`'s doc comment for why.
///
/// Deliberately reports only MemCastle's own watermark, not the schema
/// state: a SurrealKit dry-run diff on a genuinely fresh palace, whose tables
/// have never been synced for real even once, tries to introspect a
/// `SCHEMAFULL` target that doesn't exist yet and errors (confirmed
/// empirically against `surrealkit` 1.0.0-beta.2) — a `--status` call must
/// never fail on exactly the palace state it exists to describe. The
/// watermark is a plain read with no such caveat; the schema side stays
/// SurrealKit's alone to report, via its own tooling.
async fn status_with(
    store: &SurrealStore,
    migrations: &[DataMigration],
) -> Result<MigrationStatus> {
    let current_version = store.migration_version().await?;
    let latest_version = migrations
        .iter()
        .map(|step| step.version)
        .max()
        .unwrap_or(current_version);
    let pending = migrations
        .iter()
        .filter(|step| step.version > current_version)
        .map(|step| step.name.to_string())
        .collect();
    Ok(MigrationStatus {
        current_version,
        latest_version,
        pending,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    async fn memory_store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    /// A fixture step that increments a static counter every time it's
    /// actually applied — the only way these tests can distinguish "ran
    /// again" from "correctly skipped".
    static STEP_ONE_RUNS: AtomicUsize = AtomicUsize::new(0);
    static STEP_TWO_RUNS: AtomicUsize = AtomicUsize::new(0);
    static STEP_THREE_RUNS: AtomicUsize = AtomicUsize::new(0);

    /// The counters above are process-wide statics, but `cargo test` runs
    /// `#[tokio::test]`s concurrently by default — every test that resets
    /// or asserts on them must hold this for its whole body, or one test's
    /// `reset_counters()` races another's in-flight assertions. Local to
    /// this test module only; unrelated tests elsewhere in the crate are
    /// unaffected and still run in parallel. `tokio::sync::Mutex`, not
    /// `std::sync::Mutex`: the guard is held across `.await` points (every
    /// test in this module does real store I/O while holding it), which
    /// clippy's `await_holding_lock` correctly forbids for the std lock —
    /// tokio's is async-aware and has no such restriction, at the cost of
    /// no poisoning (acceptable here: a panicking test still unlocks it on
    /// drop, it just can't tell a later test "the data might be tainted").
    static COUNTER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Acquire [`COUNTER_LOCK`] for the calling test's whole body.
    async fn lock_counters() -> tokio::sync::MutexGuard<'static, ()> {
        COUNTER_LOCK.lock().await
    }

    fn reset_counters() {
        STEP_ONE_RUNS.store(0, Ordering::SeqCst);
        STEP_TWO_RUNS.store(0, Ordering::SeqCst);
        STEP_THREE_RUNS.store(0, Ordering::SeqCst);
    }

    fn step_one(_store: &SurrealStore) -> MigrationFuture<'_> {
        Box::pin(async move {
            STEP_ONE_RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn step_two(_store: &SurrealStore) -> MigrationFuture<'_> {
        Box::pin(async move {
            STEP_TWO_RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn step_two_fails(_store: &SurrealStore) -> MigrationFuture<'_> {
        Box::pin(async move {
            STEP_TWO_RUNS.fetch_add(1, Ordering::SeqCst);
            Err(Error::store_malformed("simulated failure"))
        })
    }

    fn step_three(_store: &SurrealStore) -> MigrationFuture<'_> {
        Box::pin(async move {
            STEP_THREE_RUNS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    #[tokio::test]
    async fn a_fresh_store_is_already_at_its_current_version_with_no_migrations() {
        let store = memory_store().await;
        let status = status_with(&store, &[]).await.expect("status");
        assert_eq!(status.current_version, 0);
        assert_eq!(status.latest_version, 0);
        assert!(status.pending.is_empty());

        let report = run_with(&store, &[]).await.expect("run");
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, 0);
        assert!(report.applied.is_empty());
    }

    #[tokio::test]
    async fn n_to_n_plus_one_applies_the_single_pending_step() {
        let _guard = lock_counters().await;
        reset_counters();
        let store = memory_store().await;
        let migrations = [DataMigration {
            version: 1,
            name: "step-one",
            apply: step_one,
        }];

        let report = run_with(&store, &migrations).await.expect("run");
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, 1);
        assert_eq!(report.applied, vec!["step-one".to_string()]);
        assert_eq!(store.migration_version().await.expect("version"), 1);
        assert_eq!(STEP_ONE_RUNS.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn n_to_n_plus_m_applies_every_intermediate_step_in_order() {
        let _guard = lock_counters().await;
        reset_counters();
        let store = memory_store().await;
        let migrations = [
            DataMigration {
                version: 1,
                name: "step-one",
                apply: step_one,
            },
            DataMigration {
                version: 2,
                name: "step-two",
                apply: step_two,
            },
            DataMigration {
                version: 3,
                name: "step-three",
                apply: step_three,
            },
        ];

        let report = run_with(&store, &migrations).await.expect("run");
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, 3);
        assert_eq!(
            report.applied,
            vec![
                "step-one".to_string(),
                "step-two".to_string(),
                "step-three".to_string()
            ]
        );
        assert_eq!(STEP_ONE_RUNS.load(Ordering::SeqCst), 1);
        assert_eq!(STEP_TWO_RUNS.load(Ordering::SeqCst), 1);
        assert_eq!(STEP_THREE_RUNS.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn running_twice_only_applies_each_step_once() {
        let _guard = lock_counters().await;
        reset_counters();
        let store = memory_store().await;
        let migrations = [DataMigration {
            version: 1,
            name: "step-one",
            apply: step_one,
        }];

        let first = run_with(&store, &migrations).await.expect("first run");
        assert_eq!(first.applied, vec!["step-one".to_string()]);
        let second = run_with(&store, &migrations).await.expect("second run");
        assert!(
            second.applied.is_empty(),
            "an already-applied step must not run again, got {:?}",
            second.applied
        );
        assert_eq!(
            STEP_ONE_RUNS.load(Ordering::SeqCst),
            1,
            "the step's own side effect must have happened exactly once"
        );
    }

    #[tokio::test]
    async fn a_failed_step_stops_the_run_and_leaves_the_watermark_at_the_last_success() {
        let _guard = lock_counters().await;
        reset_counters();
        let store = memory_store().await;
        let migrations = [
            DataMigration {
                version: 1,
                name: "step-one",
                apply: step_one,
            },
            DataMigration {
                version: 2,
                name: "step-two-fails",
                apply: step_two_fails,
            },
            DataMigration {
                version: 3,
                name: "step-three",
                apply: step_three,
            },
        ];

        let error = run_with(&store, &migrations)
            .await
            .expect_err("step two must fail the run");
        assert!(matches!(error, Error::MigrationFailed { version: 2, .. }));
        assert_eq!(
            store.migration_version().await.expect("version"),
            1,
            "the watermark must stay at the last step that actually succeeded"
        );
        assert_eq!(STEP_ONE_RUNS.load(Ordering::SeqCst), 1);
        assert_eq!(STEP_THREE_RUNS.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_run_can_be_resumed_after_fixing_the_step_that_failed() {
        let _guard = lock_counters().await;
        reset_counters();
        let store = memory_store().await;
        let failing = [
            DataMigration {
                version: 1,
                name: "step-one",
                apply: step_one,
            },
            DataMigration {
                version: 2,
                name: "step-two-fails",
                apply: step_two_fails,
            },
            DataMigration {
                version: 3,
                name: "step-three",
                apply: step_three,
            },
        ];
        run_with(&store, &failing)
            .await
            .expect_err("first attempt must fail at step two");
        assert_eq!(STEP_ONE_RUNS.load(Ordering::SeqCst), 1);

        // The corrected list: same versions/names, but step two now
        // succeeds — simulating "the underlying bug got fixed and this is
        // a follow-up release", not an edit of the released step.
        let fixed = [
            DataMigration {
                version: 1,
                name: "step-one",
                apply: step_one,
            },
            DataMigration {
                version: 2,
                name: "step-two",
                apply: step_two,
            },
            DataMigration {
                version: 3,
                name: "step-three",
                apply: step_three,
            },
        ];
        let report = run_with(&store, &fixed).await.expect("resumed run");
        assert_eq!(
            report.applied,
            vec!["step-two".to_string(), "step-three".to_string()],
            "step one must not be replayed, got {:?}",
            report.applied
        );
        assert_eq!(
            STEP_ONE_RUNS.load(Ordering::SeqCst),
            1,
            "step one's own side effect must still only have happened once"
        );
        assert_eq!(store.migration_version().await.expect("version"), 3);
    }

    #[tokio::test]
    async fn a_held_lock_rejects_a_concurrent_run() {
        let store = memory_store().await;
        store.ensure_migration_state().await.expect("ensure");
        let held = store
            .try_acquire_migration_lock("someone-else", Duration::minutes(5))
            .await
            .expect("acquire");
        assert!(held);

        let error = run_with(&store, &[])
            .await
            .expect_err("a run must not proceed while the lock is held");
        assert!(matches!(error, Error::MigrationLocked { .. }));
    }

    #[tokio::test]
    async fn status_never_mutates_the_store() {
        let _guard = lock_counters().await;
        reset_counters();
        let store = memory_store().await;
        let migrations = [DataMigration {
            version: 1,
            name: "step-one",
            apply: step_one,
        }];

        let before = status_with(&store, &migrations).await.expect("status");
        assert_eq!(before.current_version, 0);
        assert_eq!(before.pending, vec!["step-one".to_string()]);

        let after = status_with(&store, &migrations)
            .await
            .expect("status again");
        assert_eq!(after.current_version, 0);
        assert_eq!(after.pending, vec!["step-one".to_string()]);
        assert_eq!(
            STEP_ONE_RUNS.load(Ordering::SeqCst),
            0,
            "status must never actually apply a pending step"
        );
    }
}
