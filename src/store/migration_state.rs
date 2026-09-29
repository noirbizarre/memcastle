//! The migration bookkeeping singleton (`migration_state:state`): the data-
//! migration version watermark, plus the exclusive lease-based lock
//! `crate::migrate::run` holds while it's applying pending migrations.
//!
//! Kept as its own file, separate from `wings`/`drawers`/`jobs`/`entities`:
//! this isn't palace content, it's infrastructure the migration runner
//! needs, and mixing the two would blur what `crate::migrate` actually
//! orchestrates versus what `store` merely persists.
//!
//! Locking is a compare-and-swap over one row rather than SurrealDB's own
//! transaction/locking primitives, because it needs to work identically
//! against the embedded and a future remote backend, and a CAS over a plain
//! row is the smallest surface that does. (`claim_next_job` in `store::jobs`
//! takes no database-level lock either, but for a different reason: its
//! safety comes from the single sequential dispatcher.) Two distinct `WHERE` shapes are
//! used depending on whether the lock currently looks free or held-but-
//! stale: comparing a *bound* parameter against an absent (`NONE`) field
//! silently never matches (binding `Option::None` produces SurrealDB's
//! `NULL`, not its `NONE` — see `store::jobs::list_jobs`'s regression
//! comment), so "currently free" has to be spelled as a literal `= NONE` in
//! the query text; reclaiming an already-held-but-expired lock instead
//! binds the exact previous owner string we observed, which is a normal,
//! safe equality bind and gives a real compare-and-swap (a racing second
//! reclaimer's `WHERE` no longer matches once the first one's `UPDATE` has
//! applied).

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::error::{Error, Result};

use super::SurrealStore;

#[derive(Debug, Deserialize)]
struct VersionRow {
    version: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct LockRow {
    lock_owner: Option<String>,
    lock_expires_at: Option<String>,
}

/// True if `error` is SurrealDB's "table not found" — i.e. `migration_state`
/// itself has never been synced (a genuinely fresh, never-migrated palace:
/// `SurrealStore::connect` no longer syncs schema itself, only
/// `crate::migrate::run` does — see `store::mod`'s doc). A read that must
/// never mutate (`migration_version`, backing `crate::migrate::status`)
/// treats this as "nothing recorded yet", not a real failure.
fn is_table_not_found(error: &surrealdb::Error) -> bool {
    matches!(
        error.not_found_details(),
        Some(surrealdb::types::NotFoundError::Table { .. })
    )
}

/// `response.take(index)`, without converting a failure to `crate::Error` —
/// `migration_version` needs to inspect the raw `surrealdb::Error` first
/// (via `is_table_not_found`) to decide whether it's a real failure or just
/// "nothing recorded yet". A per-statement query failure (as opposed to a
/// transport-level one) only ever surfaces here, at `.take()`, not at the
/// `.await` that ran the query itself — the response comes back `Ok` with
/// the failure embedded per-index until something actually reads that
/// index.
fn take_json_raw(
    response: &mut surrealdb::IndexedResults,
    index: usize,
) -> std::result::Result<Vec<serde_json::Value>, surrealdb::Error> {
    response.take(index)
}

impl SurrealStore {
    /// Create the singleton row if this is the first time any migration
    /// machinery has touched this palace (version starts at `0`, no lock
    /// held). A no-op otherwise.
    ///
    /// Read-only callers (`crate::migrate::status`) must never call this —
    /// see that function's "performs no mutation" contract.
    pub(crate) async fn ensure_migration_state(&self) -> Result<()> {
        let mut response = self
            .db
            .query("SELECT version FROM migration_state:state")
            .await?;
        let existing: Vec<VersionRow> = super::take_rows(&mut response, 0)?;
        if !existing.is_empty() {
            return Ok(());
        }

        self.db
            .query(
                "CREATE migration_state:state SET \
                 version = 0, lock_owner = NONE, lock_expires_at = NONE, updated_at = <datetime>$now",
            )
            .bind(("now", super::stored(Utc::now())))
            .await?
            .check()?;
        Ok(())
    }

    /// The last successfully recorded data-migration version, or `0` if the
    /// row (or even the table itself, on a genuinely fresh palace — see
    /// `is_table_not_found`) doesn't exist yet. A pure read: never creates
    /// the row, never touches the lock, never syncs schema.
    pub(crate) async fn migration_version(&self) -> Result<u32> {
        let mut response = self
            .db
            .query("SELECT version FROM migration_state:state")
            .await?;
        // A per-statement failure (as opposed to a transport-level one)
        // only surfaces here, at `.take()` — not at the `.await` above, see
        // `take_json_raw`'s doc comment.
        let raw = match take_json_raw(&mut response, 0) {
            Ok(rows) => rows,
            Err(source) if is_table_not_found(&source) => return Ok(0),
            Err(source) => return Err(source.into()),
        };
        let rows: Vec<VersionRow> = raw
            .into_iter()
            .map(|row| {
                serde_json::from_value(row)
                    .map_err(|source| Error::store_malformed(source.to_string()))
            })
            .collect::<Result<_>>()?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|row| u32::try_from(row.version).ok())
            .unwrap_or(0))
    }

    /// Record the watermark after one data migration step succeeds.
    pub(crate) async fn record_migration_version(&self, version: u32) -> Result<()> {
        self.db
            .query(
                "UPDATE migration_state:state SET version = $version, updated_at = <datetime>$now",
            )
            .bind(("version", version))
            .bind(("now", super::stored(Utc::now())))
            .await?
            .check()?;
        Ok(())
    }

    /// Try to acquire the exclusive migration lock for `owner`, with a
    /// `lease` after which — if `owner` never released it (e.g. it
    /// crashed mid-migration) — a later caller may reclaim it. Returns
    /// `Ok(false)` if another, still-live owner holds it; never blocks.
    ///
    /// Call `ensure_migration_state` first — this assumes the row exists.
    pub(crate) async fn try_acquire_migration_lock(
        &self,
        owner: &str,
        lease: Duration,
    ) -> Result<bool> {
        let now = Utc::now();
        let current = self.read_lock_row().await?;

        let previous_owner = current.as_ref().and_then(|row| row.lock_owner.clone());
        let is_free = match &current {
            None => true, // `ensure_migration_state` should have run; treat defensively as free.
            Some(row) => match &row.lock_owner {
                None => true,
                Some(_) => row
                    .lock_expires_at
                    .as_deref()
                    .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                    .is_some_and(|expires_at| expires_at.with_timezone(&Utc) < now),
            },
        };
        if !is_free {
            return Ok(false);
        }

        let expires_at = super::stored(now + lease);
        let response = match previous_owner {
            // Currently unheld: `= NONE` must be a literal in the query
            // text, not a bound parameter — see this module's doc comment.
            None => {
                self.db
                    .query(
                        "UPDATE migration_state:state SET lock_owner = $owner, lock_expires_at = $expires_at \
                         WHERE lock_owner = NONE",
                    )
                    .bind(("owner", owner.to_string()))
                    .bind(("expires_at", expires_at))
                    .await?
            }
            // Held but expired: reclaim only if the owner we just observed
            // is still the one on the row — a genuine compare-and-swap
            // against a racing second reclaimer.
            Some(previous_owner) => {
                self.db
                    .query(
                        "UPDATE migration_state:state SET lock_owner = $owner, lock_expires_at = $expires_at \
                         WHERE lock_owner = $previous_owner",
                    )
                    .bind(("owner", owner.to_string()))
                    .bind(("expires_at", expires_at))
                    .bind(("previous_owner", previous_owner))
                    .await?
            }
        };
        let mut response = response.check()?;
        let updated: Vec<LockRow> = super::take_rows(&mut response, 0)?;
        Ok(updated
            .into_iter()
            .next()
            .is_some_and(|row| row.lock_owner.as_deref() == Some(owner)))
    }

    /// Release the lock, but only if `owner` still holds it — a stale or
    /// already-superseded caller must never clear someone else's lock.
    pub(crate) async fn release_migration_lock(&self, owner: &str) -> Result<()> {
        self.db
            .query(
                "UPDATE migration_state:state SET lock_owner = NONE, lock_expires_at = NONE \
                 WHERE lock_owner = $owner",
            )
            .bind(("owner", owner.to_string()))
            .await?
            .check()?;
        Ok(())
    }

    async fn read_lock_row(&self) -> Result<Option<LockRow>> {
        let mut response = self
            .db
            .query("SELECT lock_owner, lock_expires_at FROM migration_state:state")
            .await?;
        let rows: Vec<LockRow> = super::take_rows(&mut response, 0)?;
        Ok(rows.into_iter().next())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    #[tokio::test]
    async fn version_defaults_to_zero_on_a_fresh_store() {
        let store = memory_store().await;
        assert_eq!(store.migration_version().await.expect("version"), 0);
    }

    /// Regression test for the exact bug `is_table_not_found`/`take_json_raw`
    /// exist to fix: a per-statement SurrealDB query failure (as opposed to
    /// a transport-level one) comes back as `Ok(IndexedResults)` with the
    /// failure embedded at that index — it only surfaces once something
    /// reads that index (`.take()`), not at the `.await` that ran the
    /// query. `connect_memory_for_tests` always syncs schema first, so it
    /// can't exercise this; this test deliberately bypasses it to connect a
    /// store whose `migration_state` table has genuinely never been
    /// synced, exactly like `SurrealStore::connect` (which no longer syncs
    /// schema itself) before `crate::migrate::run`/`status` ever touch it.
    #[tokio::test]
    async fn migration_version_tolerates_a_genuinely_unsynced_store() {
        let db = surrealdb::engine::any::connect("memory")
            .await
            .expect("connect");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        // `SurrealStore`'s only field is private to this crate, but this
        // module is inside `store`, so it can still be constructed
        // directly — the point is deliberately skipping `sync_schema`.
        let store = SurrealStore { db };
        assert_eq!(
            store.migration_version().await.expect("version"),
            0,
            "a never-synced store's version must read as 0, not error"
        );
    }

    #[tokio::test]
    async fn migration_version_never_creates_the_row() {
        let store = memory_store().await;
        // Read twice, without ever calling `ensure_migration_state` —
        // still just `0`, no row created as a side effect.
        assert_eq!(store.migration_version().await.expect("first read"), 0);
        assert_eq!(store.migration_version().await.expect("second read"), 0);
    }

    #[tokio::test]
    async fn recording_a_version_round_trips() {
        let store = memory_store().await;
        store.ensure_migration_state().await.expect("ensure");
        store
            .record_migration_version(3)
            .await
            .expect("record version");
        assert_eq!(store.migration_version().await.expect("version"), 3);
    }

    #[tokio::test]
    async fn ensure_migration_state_is_idempotent() {
        let store = memory_store().await;
        store.ensure_migration_state().await.expect("first ensure");
        store
            .record_migration_version(5)
            .await
            .expect("record version");
        // A second `ensure` must not reset the watermark it just wrote.
        store.ensure_migration_state().await.expect("second ensure");
        assert_eq!(store.migration_version().await.expect("version"), 5);
    }

    #[tokio::test]
    async fn a_lock_can_be_acquired_once_and_rejects_a_second_owner() {
        let store = memory_store().await;
        store.ensure_migration_state().await.expect("ensure");

        let acquired = store
            .try_acquire_migration_lock("owner-a", Duration::minutes(5))
            .await
            .expect("acquire attempt");
        assert!(acquired, "the first owner should acquire a free lock");

        let rejected = store
            .try_acquire_migration_lock("owner-b", Duration::minutes(5))
            .await
            .expect("acquire attempt");
        assert!(
            !rejected,
            "a second owner must not acquire a lock still held and not yet expired"
        );
    }

    #[tokio::test]
    async fn an_expired_lease_can_be_reclaimed_by_a_new_owner() {
        let store = memory_store().await;
        store.ensure_migration_state().await.expect("ensure");
        // A lease already in the past: acquired, then immediately stale.
        store
            .try_acquire_migration_lock("owner-a", Duration::seconds(-1))
            .await
            .expect("acquire attempt");

        let reclaimed = store
            .try_acquire_migration_lock("owner-b", Duration::minutes(5))
            .await
            .expect("reclaim attempt");
        assert!(
            reclaimed,
            "an expired lease must be reclaimable by a new owner"
        );
    }

    #[tokio::test]
    async fn release_only_clears_the_lock_for_its_own_owner() {
        let store = memory_store().await;
        store.ensure_migration_state().await.expect("ensure");
        store
            .try_acquire_migration_lock("owner-a", Duration::minutes(5))
            .await
            .expect("acquire attempt");

        // A caller that never held the lock must not be able to clear it.
        store
            .release_migration_lock("owner-b")
            .await
            .expect("release attempt");
        let still_rejected = store
            .try_acquire_migration_lock("owner-c", Duration::minutes(5))
            .await
            .expect("acquire attempt");
        assert!(
            !still_rejected,
            "owner-b's release must not have cleared owner-a's lock"
        );

        store
            .release_migration_lock("owner-a")
            .await
            .expect("release attempt");
        let now_free = store
            .try_acquire_migration_lock("owner-c", Duration::minutes(5))
            .await
            .expect("acquire attempt");
        assert!(now_free, "owner-a's own release must free the lock");
    }
}
