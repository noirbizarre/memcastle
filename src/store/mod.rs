//! The SurrealDB storage abstraction.
//!
//! One connection type (`Surreal<Any>`, via `engine::any`) for both embedded
//! and remote deployments — the rest of the codebase never branches on which
//! backend is active. Repository methods live in the sibling modules
//! (`wings`, `drawers`, `jobs`, `entities`) as `impl SurrealStore` blocks;
//! this file only owns connecting and schema sync.
//!
//! Every write and read goes through hand-written SurrealQL with explicit
//! `<datetime>`/`<string>` casts rather than the SDK's typed `create`/
//! `select` helpers or its `Datetime`/`RecordId` wrapper types. That costs
//! some verbosity, but it means the only surface this module depends on is
//! `Surreal::query`/`bind`/`take` — the smallest, most stable part of the
//! API — instead of type-coercion behaviour between chrono and the driver's
//! own serde bridge that would otherwise have to be discovered by trial and
//! error.
//!
//! Schema management (`DEFINE TABLE`/`FIELD`/`INDEX`) is delegated entirely
//! to SurrealKit's library API, not reimplemented here — see
//! `docs/adr/004-versioned-database-migrations.md`. `embed_schema!()` below
//! compiles every `.surql` file under `database/schema/` (relative to
//! `Cargo.toml`) into the binary and generates `embedded_schema::{SCHEMA,
//! sync}`; SurrealKit tracks each file's content hash in its own `__entity`
//! metadata table and only reapplies what actually changed. `connect()`
//! itself does **not** sync schema — that's an explicit step of
//! `crate::migrate::run`/`status` (schema sync must happen alongside, and
//! in the same order as, MemCastle's own data migrations — see that
//! module's doc), not an implicit side effect of opening a connection.
// Wrapped in its own module so `#![allow(missing_docs)]` (an inner
// attribute, since the crate's `#![warn(missing_docs)]` would otherwise
// flag the macro's generated, undocumented items) only scopes to
// generated code, not this file's own hand-written items.
mod embedded_schema_gen {
    #![allow(missing_docs)]
    surrealkit::embed_schema!();
}
use embedded_schema_gen::embedded_schema;

mod drawers;
mod entities;
mod jobs;
mod migration_state;
mod wings;

use std::path::PathBuf;

use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;

use crate::error::{Error, Result};

pub use drawers::SearchHit;

/// Convert a struct or data-carrying enum to a bindable value.
///
/// Binding one of these types directly (`.bind(("field", value))`) silently
/// produces an empty object: the SDK's parameter binder supports `serde`'s
/// map/seq/primitive serialization, but not `serialize_struct`/
/// `serialize_struct_variant`, and drops every field without an error.
/// Going through `serde_json::Value` first serializes via `serialize_map`
/// instead, which the binder does handle correctly. Fieldless enums
/// (`JobStatus`) and plain collections (`Vec`, `Option`) are unaffected and
/// don't need this.
pub(crate) fn bindable<T: serde::Serialize>(value: &T) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|source| Error::store_malformed(source.to_string()))
}

/// Deserialize the query results at `index` into `Vec<T>`.
///
/// The mirror image of `bindable`: the 3.x driver's `take` only accepts
/// its own `SurrealValue` trait now, which arbitrary domain structs don't
/// implement (and shouldn't have to — that would put a SurrealDB-specific
/// trait on `domain`'s pure types). `serde_json::Value` does implement
/// `SurrealValue`, so results are taken as JSON first and decoded with
/// `serde_json`, keeping the SDK's type surface confined to this module.
pub(crate) fn take_rows<T: serde::de::DeserializeOwned>(
    response: &mut surrealdb::IndexedResults,
    index: usize,
) -> Result<Vec<T>> {
    let rows: Vec<serde_json::Value> = response.take(index)?;
    rows.into_iter()
        .map(|row| {
            serde_json::from_value(row).map_err(|source| Error::store_malformed(source.to_string()))
        })
        .collect()
}

/// Where the palace's data actually lives.
#[derive(Debug, Clone)]
pub enum Backend {
    /// A local SurrealKV directory — the default developer experience.
    Embedded {
        /// The directory SurrealDB should own. Created if missing.
        path: PathBuf,
    },
    /// A remotely hosted SurrealDB instance.
    Remote {
        /// e.g. `ws://localhost:8000` or `wss://db.example.com`.
        url: String,
        /// The namespace to select after connecting.
        namespace: String,
        /// The database to select after connecting.
        database: String,
        /// Root (or namespace/database) username.
        username: String,
        /// Root (or namespace/database) password.
        password: String,
    },
}

impl Backend {
    /// The endpoint string `engine::any::connect` dispatches on.
    fn endpoint(&self) -> String {
        match self {
            // Single colon, no slashes: `surrealkv:` is the scheme, what
            // follows is the path verbatim (`surrealkv://` would make the
            // first path segment look like a host).
            Self::Embedded { path } => format!("surrealkv:{}", path.display()),
            Self::Remote { url, .. } => url.clone(),
        }
    }
}

/// A connected, migrated handle to one palace's storage.
#[derive(Clone)]
pub struct SurrealStore {
    db: Surreal<Any>,
}

impl SurrealStore {
    /// Connect to `backend` and select its namespace/database. Does **not**
    /// sync schema or run migrations — see this module's doc comment on why
    /// that's a separate, explicit step (`crate::migrate::run`/`status`),
    /// not an implicit side effect of connecting.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] if the connection or sign-in fails.
    pub async fn connect(backend: &Backend) -> Result<Self> {
        if let Backend::Embedded { path } = backend {
            std::fs::create_dir_all(path)
                .map_err(|source| crate::Error::io(path.display().to_string(), source))?;
        }

        let db = any::connect(backend.endpoint()).await?;

        let (namespace, database) = match backend {
            Backend::Embedded { .. } => ("memcastle", "palace"),
            Backend::Remote {
                namespace,
                database,
                username,
                password,
                ..
            } => {
                // `Root`'s fields are owned `String`s as of 3.x (previously
                // borrowed) — `backend` is `&Backend`, so these are `&String`.
                db.signin(Root {
                    username: username.clone(),
                    password: password.clone(),
                })
                .await?;
                (namespace.as_str(), database.as_str())
            }
        };
        db.use_ns(namespace).use_db(database).await?;

        Ok(Self { db })
    }

    /// Apply the embedded schema via SurrealKit's `Sync`. Idempotent:
    /// SurrealKit tracks each file's content hash in its own metadata and
    /// only reapplies what changed. Called by `crate::migrate::run`, not by
    /// `connect()` — see this module's doc.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::SchemaSync`] if SurrealKit fails to apply
    /// the embedded schema (e.g. a malformed `.surql` statement).
    pub(crate) async fn sync_schema(&self) -> Result<()> {
        embedded_schema::sync(&self.db)
            .await
            .map_err(|source| Error::schema_sync(source.to_string()))
    }
}

#[cfg(test)]
impl SurrealStore {
    /// A migrated, in-memory (`kv-mem`) store for other modules' unit
    /// tests — e.g. `checkpoint::tests` — that need a real `SurrealStore`
    /// without a tempdir-backed `SurrealKV` path. `pub(crate)` and
    /// `cfg(test)`-gated: only test code anywhere in this crate should ever
    /// construct a bare in-memory store this way, never a real interface.
    /// Syncs schema itself (unlike the real `connect()`) — this helper's
    /// whole point is "hand me a ready-to-use store", so tests that don't
    /// care about migration orchestration don't have to think about it.
    pub(crate) async fn connect_memory_for_tests() -> Self {
        let db = any::connect("memory").await.expect("connect");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        let store = Self { db };
        store.sync_schema().await.expect("sync_schema");
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_store() -> SurrealStore {
        // `engine::any` dispatches the literal string `"memory"` to the
        // `kv-mem` engine — no scheme prefix, unlike `surrealkv:`. Delegates
        // to `connect_memory_for_tests` so this module and every other
        // module's unit tests share one implementation.
        SurrealStore::connect_memory_for_tests().await
    }

    #[tokio::test]
    async fn migrations_apply_cleanly_and_are_idempotent() {
        let store = memory_store().await;
        // Re-running must not error — every DEFINE is IF NOT EXISTS.
        store.sync_schema().await.expect("second sync_schema");
    }

    // "Reopen the same SurrealKV path in the same process" is deliberately
    // NOT exercised here: SurrealDB's embedded engine does not release its
    // on-disk lock file when a `Surreal` handle drops within the same
    // process (confirmed empirically — a second `connect` against the same
    // path fails immediately with "Database at <path>/LOCK is already
    // locked by another process"; unlike the prior RocksDB backend, which
    // hung/silently retried instead of erroring, SurrealKV at least fails
    // fast), so a unit test doing that would be testing a driver quirk, not
    // `SurrealStore`. The real "does data survive a restart" guarantee is
    // proven at the process boundary instead, in `tests/persistence.rs`,
    // which spawns two genuinely separate `memcastle serve` processes
    // against the same palace directory.
    #[tokio::test]
    async fn a_drawer_can_be_created_and_listed_under_its_room() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = Backend::Embedded {
            path: dir.path().join("palace"),
        };
        let store = SurrealStore::connect(&backend).await.expect("connect");
        // `connect()` no longer syncs schema itself (see this module's doc)
        // — mirror what `server::run`/`cmd_migrate` do via `crate::migrate::run`.
        store.sync_schema().await.expect("sync schema");

        let wing = store.get_or_create_wing("demo", None).await.expect("wing");
        let room = store
            .get_or_create_room(wing.id, "general", None)
            .await
            .expect("room");
        let drawer = crate::domain::Drawer {
            id: crate::domain::DrawerId::new(),
            room: room.id,
            content: "hello palace".into(),
            content_hash: "abc".into(),
            source: crate::domain::Source {
                kind: crate::domain::SourceKind::Manual,
                uri: None,
                agent: Some("test".into()),
            },
            tags: vec![],
            embedding: None,
            provenance: crate::domain::Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
            valid_from: chrono::Utc::now(),
            valid_to: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        store.create_drawer(&drawer).await.expect("create drawer");

        let drawers = store.list_drawers(room.id).await.expect("list");
        assert_eq!(drawers.len(), 1);
        assert_eq!(drawers[0].id, drawer.id);
        assert_eq!(drawers[0].content, "hello palace");
    }

    #[tokio::test]
    async fn a_job_can_be_saved_claimed_and_completed() {
        let store = memory_store().await;
        let mut job = crate::domain::Job::new(
            crate::domain::JobKind::Demo { steps: 1 },
            crate::domain::Priority::Normal,
            "test",
        );
        store.save_job(&job).await.expect("save queued");

        let fetched = store.get_job(job.id).await.expect("get").expect("present");
        assert_eq!(fetched.status, crate::domain::JobStatus::Queued);

        let claimed = store
            .claim_next_job("worker-1")
            .await
            .expect("claim")
            .expect("a job was claimed");
        assert_eq!(claimed.id, job.id);
        assert_eq!(claimed.status, crate::domain::JobStatus::Running);

        job = claimed;
        job.apply(crate::domain::JobEvent::Complete)
            .expect("apply complete");
        store.save_job(&job).await.expect("save completed");

        let fetched = store.get_job(job.id).await.expect("get").expect("present");
        assert_eq!(fetched.status, crate::domain::JobStatus::Completed);
    }

    // Regression test for a 3.x driver behaviour change: binding
    // `Option::None` through `serde_json::Value` produces SurrealDB's
    // `NULL` (a real value), not its `NONE` (absence) -- so the "no filter"
    // branch of a query must compare against `NULL`, not `NONE`, or it
    // silently matches nothing and every unfiltered list reads empty.
    #[tokio::test]
    async fn listing_jobs_with_no_status_filter_returns_every_job() {
        let store = memory_store().await;
        let job = crate::domain::Job::new(
            crate::domain::JobKind::Demo { steps: 1 },
            crate::domain::Priority::Normal,
            "test",
        );
        store.save_job(&job).await.expect("save");

        let all = store.list_jobs(None).await.expect("list all");
        assert_eq!(
            all.len(),
            1,
            "list_jobs(None) should return every job, got {all:?}"
        );
    }

    /// A minimal drawer fixture for search tests — only `room` and
    /// `content` vary between callers; everything else is filler a
    /// full-text search test doesn't care about.
    fn test_drawer(room: crate::domain::RoomId, content: &str) -> crate::domain::Drawer {
        crate::domain::Drawer {
            id: crate::domain::DrawerId::new(),
            room,
            content: content.to_string(),
            content_hash: "hash".into(),
            source: crate::domain::Source {
                kind: crate::domain::SourceKind::Manual,
                uri: None,
                agent: Some("test".into()),
            },
            tags: vec![],
            embedding: None,
            provenance: crate::domain::Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
            valid_from: chrono::Utc::now(),
            valid_to: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn an_unscoped_lexical_search_still_matches_every_wing() {
        let store = memory_store().await;
        let alpha = store
            .get_or_create_wing("alpha", None)
            .await
            .expect("wing alpha");
        let alpha_room = store
            .get_or_create_room(alpha.id, "notes", None)
            .await
            .expect("room alpha");
        let beta = store
            .get_or_create_wing("beta", None)
            .await
            .expect("wing beta");
        let beta_room = store
            .get_or_create_room(beta.id, "notes", None)
            .await
            .expect("room beta");

        store
            .create_drawer(&test_drawer(
                alpha_room.id,
                "the castle remembers everything",
            ))
            .await
            .expect("create alpha drawer");
        store
            .create_drawer(&test_drawer(
                beta_room.id,
                "the castle remembers everything too",
            ))
            .await
            .expect("create beta drawer");

        let hits = store
            .lexical_search("castle", 10, None, None)
            .await
            .expect("search");
        assert_eq!(
            hits.len(),
            2,
            "unscoped search should still return every matching drawer, got {hits:?}"
        );
    }

    #[tokio::test]
    async fn lexical_search_scoped_to_a_wing_only_returns_that_wings_drawers() {
        let store = memory_store().await;
        let alpha = store
            .get_or_create_wing("alpha", None)
            .await
            .expect("wing alpha");
        let alpha_room = store
            .get_or_create_room(alpha.id, "notes", None)
            .await
            .expect("room alpha");
        let beta = store
            .get_or_create_wing("beta", None)
            .await
            .expect("wing beta");
        let beta_room = store
            .get_or_create_room(beta.id, "notes", None)
            .await
            .expect("room beta");

        let alpha_drawer = test_drawer(alpha_room.id, "the castle remembers everything");
        store
            .create_drawer(&alpha_drawer)
            .await
            .expect("create alpha drawer");
        store
            .create_drawer(&test_drawer(
                beta_room.id,
                "the castle remembers everything too",
            ))
            .await
            .expect("create beta drawer");

        let hits = store
            .lexical_search("castle", 10, Some("alpha"), None)
            .await
            .expect("wing-scoped search");
        assert_eq!(
            hits.len(),
            1,
            "wing-scoped search should only return alpha's drawer, got {hits:?}"
        );
        assert_eq!(hits[0].drawer.id, alpha_drawer.id);
    }

    #[tokio::test]
    async fn lexical_search_scoped_to_a_room_only_returns_that_rooms_drawers() {
        let store = memory_store().await;
        let wing = store.get_or_create_wing("alpha", None).await.expect("wing");
        let general = store
            .get_or_create_room(wing.id, "general", None)
            .await
            .expect("room general");
        let notes = store
            .get_or_create_room(wing.id, "notes", None)
            .await
            .expect("room notes");

        store
            .create_drawer(&test_drawer(general.id, "the castle remembers everything"))
            .await
            .expect("create general drawer");
        let notes_drawer = test_drawer(notes.id, "the castle remembers everything too");
        store
            .create_drawer(&notes_drawer)
            .await
            .expect("create notes drawer");

        let hits = store
            .lexical_search("castle", 10, None, Some("notes"))
            .await
            .expect("room-scoped search");
        assert_eq!(
            hits.len(),
            1,
            "room-scoped search should only return the notes drawer, got {hits:?}"
        );
        assert_eq!(hits[0].drawer.id, notes_drawer.id);
    }

    // The issue's explicit regression-test ask: prove the scope is a
    // SurrealQL predicate the database applies before `LIMIT`, not a
    // filter this method applies in Rust to an already-limited page. A
    // post-filter implementation would pass every other test above (small
    // fixtures, no `LIMIT` pressure) but fail this one.
    #[tokio::test]
    async fn wing_scope_is_applied_by_surrealdb_before_the_result_limit() {
        let store = memory_store().await;
        let loud = store
            .get_or_create_wing("loud", None)
            .await
            .expect("wing loud");
        let loud_room = store
            .get_or_create_room(loud.id, "notes", None)
            .await
            .expect("room loud");
        let quiet = store
            .get_or_create_wing("quiet", None)
            .await
            .expect("wing quiet");
        let quiet_room = store
            .get_or_create_room(quiet.id, "notes", None)
            .await
            .expect("room quiet");

        // Repeated term -> a much higher BM25 score than a single mention,
        // so an unscoped, capped query is dominated by "loud"'s drawers.
        for i in 0..3 {
            store
                .create_drawer(&test_drawer(
                    loud_room.id,
                    &format!("castle castle castle castle castle #{i}"),
                ))
                .await
                .expect("create loud drawer");
        }
        let mut quiet_ids = Vec::new();
        for i in 0..2 {
            let drawer = test_drawer(quiet_room.id, &format!("castle #{i}"));
            store
                .create_drawer(&drawer)
                .await
                .expect("create quiet drawer");
            quiet_ids.push(drawer.id);
        }

        // Sanity check: with no scope and a limit smaller than the total
        // match count, the top hits are "loud"'s -- proving the score gap
        // is real, not an artifact of insertion order.
        let unscoped = store
            .lexical_search("castle", 2, None, None)
            .await
            .expect("unscoped search");
        assert_eq!(unscoped.len(), 2);
        assert!(
            unscoped.iter().all(|hit| hit.drawer.room == loud_room.id),
            "expected the top 2 unscoped hits to be \"loud\"'s higher-scoring drawers, got {unscoped:?}"
        );

        // The actual regression check: scoping to "quiet" with the same
        // small limit must still return "quiet"'s drawers. If the scope
        // were a Rust-side post-filter over an already-limited,
        // already-fetched unscoped page, this would come back empty -- the
        // top 2 rows fetched would already be "loud"'s.
        let scoped = store
            .lexical_search("castle", 2, Some("quiet"), None)
            .await
            .expect("wing-scoped search");
        assert_eq!(
            scoped.len(),
            2,
            "expected both \"quiet\" drawers despite the limit, got {scoped:?}"
        );
        assert!(
            scoped.iter().all(|hit| quiet_ids.contains(&hit.drawer.id)),
            "expected only \"quiet\"'s drawers, got {scoped:?}"
        );
    }

    #[tokio::test]
    async fn list_diary_drawers_only_returns_the_matching_agents_entries() {
        let store = memory_store().await;
        let wing = store.get_or_create_wing("diary", None).await.expect("wing");
        let room = store
            .get_or_create_room(wing.id, "diary", None)
            .await
            .expect("room");

        let mut alice_drawer = test_drawer(room.id, "alice's entry");
        alice_drawer.source.agent = Some("alice".to_string());
        store
            .create_drawer(&alice_drawer)
            .await
            .expect("create alice drawer");

        let mut bob_drawer = test_drawer(room.id, "bob's entry");
        bob_drawer.source.agent = Some("bob".to_string());
        store
            .create_drawer(&bob_drawer)
            .await
            .expect("create bob drawer");

        let alice_entries = store
            .list_diary_drawers(room.id, "alice", 10)
            .await
            .expect("list alice's entries");
        assert_eq!(
            alice_entries.len(),
            1,
            "bob's entry must not leak into alice's diary read, got {alice_entries:?}"
        );
        assert_eq!(alice_entries[0].id, alice_drawer.id);

        let bob_entries = store
            .list_diary_drawers(room.id, "bob", 10)
            .await
            .expect("list bob's entries");
        assert_eq!(bob_entries.len(), 1);
        assert_eq!(bob_entries[0].id, bob_drawer.id);
    }

    #[tokio::test]
    async fn list_diary_drawers_orders_newest_first_and_respects_the_limit() {
        let store = memory_store().await;
        let wing = store.get_or_create_wing("diary", None).await.expect("wing");
        let room = store
            .get_or_create_room(wing.id, "diary", None)
            .await
            .expect("room");

        // Explicit, strictly increasing `created_at` values rather than
        // `Utc::now()` in a loop: this proves the `ORDER BY created_at
        // DESC` behaviour deterministically instead of racing wall-clock
        // resolution across fast successive writes.
        let base = chrono::Utc::now();
        let mut ids = Vec::new();
        for i in 0..3i64 {
            let mut drawer = test_drawer(room.id, &format!("entry {i}"));
            drawer.source.agent = Some("agent".to_string());
            drawer.created_at = base + chrono::Duration::seconds(i);
            ids.push(drawer.id);
            store.create_drawer(&drawer).await.expect("create drawer");
        }

        let entries = store
            .list_diary_drawers(room.id, "agent", 2)
            .await
            .expect("list");
        assert_eq!(
            entries.len(),
            2,
            "limit must cap the result, got {entries:?}"
        );
        assert_eq!(
            entries[0].id, ids[2],
            "the newest entry must come first, got {entries:?}"
        );
        assert_eq!(
            entries[1].id, ids[1],
            "the second-newest entry must come second, got {entries:?}"
        );
    }

    /// A drawer fixture whose `provenance.job_id` points at `job_id` —
    /// simulates what a checkpoint (or mining) job leaves behind, without
    /// running the real handler (see `checkpoint::mod`'s own tests, and
    /// `app::mod`'s `seed_checkpoint_drawer`, for the end-to-end version).
    fn test_drawer_from_job(
        room: crate::domain::RoomId,
        content: &str,
        job_id: crate::domain::JobId,
    ) -> crate::domain::Drawer {
        let mut drawer = test_drawer(room, content);
        drawer.provenance.job_id = Some(job_id);
        drawer
    }

    #[tokio::test]
    async fn list_checkpoint_originated_drawers_only_returns_drawers_from_checkpoint_jobs() {
        let store = memory_store().await;
        let wing = store
            .get_or_create_wing("project-x", None)
            .await
            .expect("wing");
        let room = store
            .get_or_create_room(wing.id, "notes", None)
            .await
            .expect("room");

        let checkpoint_job = crate::domain::Job::new(
            crate::domain::JobKind::Checkpoint {
                payload: crate::domain::CheckpointPayload { items: vec![] },
            },
            crate::domain::Priority::High,
            "test",
        );
        store
            .save_job(&checkpoint_job)
            .await
            .expect("save checkpoint job");

        let mine_job = crate::domain::Job::new(
            crate::domain::JobKind::Mine {
                source: crate::domain::MiningSource::Directory {
                    path: "/tmp".into(),
                },
                wing: None,
            },
            crate::domain::Priority::Background,
            "test",
        );
        store.save_job(&mine_job).await.expect("save mine job");

        let checkpoint_drawer =
            test_drawer_from_job(room.id, "a checkpointed highlight", checkpoint_job.id);
        store
            .create_drawer(&checkpoint_drawer)
            .await
            .expect("create checkpoint drawer");

        let mined_drawer = test_drawer_from_job(room.id, "a mined note", mine_job.id);
        store
            .create_drawer(&mined_drawer)
            .await
            .expect("create mined drawer");

        let manual_drawer = test_drawer(room.id, "a manual note, no job at all");
        store
            .create_drawer(&manual_drawer)
            .await
            .expect("create manual drawer");

        let highlights = store
            .list_checkpoint_originated_drawers(None, 10)
            .await
            .expect("list checkpoint-originated");
        assert_eq!(
            highlights.len(),
            1,
            "only the checkpoint job's drawer should come back, got {highlights:?}"
        );
        assert_eq!(highlights[0].id, checkpoint_drawer.id);
    }

    // Same regression class as `wing_scope_is_applied_by_surrealdb_before_
    // the_result_limit`, applied to `list_checkpoint_originated_drawers`:
    // the "quiet" wing's drawers are older than "loud"'s here, so a
    // Rust-side post-filter (fetch the newest `limit` rows unscoped, then
    // drop the ones outside the requested wing) would fetch "loud"'s two
    // newest drawers and filter them all away, returning nothing — despite
    // "quiet" genuinely having two matching drawers.
    #[tokio::test]
    async fn checkpoint_originated_wing_scope_is_applied_by_surrealdb_before_the_result_limit() {
        let store = memory_store().await;
        let loud = store
            .get_or_create_wing("loud", None)
            .await
            .expect("wing loud");
        let loud_room = store
            .get_or_create_room(loud.id, "notes", None)
            .await
            .expect("room loud");
        let quiet = store
            .get_or_create_wing("quiet", None)
            .await
            .expect("wing quiet");
        let quiet_room = store
            .get_or_create_room(quiet.id, "notes", None)
            .await
            .expect("room quiet");

        let job = crate::domain::Job::new(
            crate::domain::JobKind::Checkpoint {
                payload: crate::domain::CheckpointPayload { items: vec![] },
            },
            crate::domain::Priority::High,
            "test",
        );
        store.save_job(&job).await.expect("save job");

        // Older, fewer: "quiet"'s two drawers, created first.
        let mut quiet_ids = Vec::new();
        for i in 0..2 {
            let drawer =
                test_drawer_from_job(quiet_room.id, &format!("quiet highlight #{i}"), job.id);
            store
                .create_drawer(&drawer)
                .await
                .expect("create quiet drawer");
            quiet_ids.push(drawer.id);
        }
        // Newer, more numerous: "loud"'s three drawers, created after.
        for i in 0..3 {
            store
                .create_drawer(&test_drawer_from_job(
                    loud_room.id,
                    &format!("loud highlight #{i}"),
                    job.id,
                ))
                .await
                .expect("create loud drawer");
        }

        let scoped = store
            .list_checkpoint_originated_drawers(Some("quiet"), 2)
            .await
            .expect("scoped list");
        assert_eq!(
            scoped.len(),
            2,
            "expected both \"quiet\" drawers despite the limit, got {scoped:?}"
        );
        assert!(
            scoped.iter().all(|d| quiet_ids.contains(&d.id)),
            "expected only \"quiet\"'s drawers, got {scoped:?}"
        );
    }

    #[tokio::test]
    async fn creating_an_entity_twice_with_the_same_name_and_kind_is_idempotent() {
        let store = memory_store().await;
        let first = store
            .create_entity("Ada Lovelace", "person", serde_json::json!({}))
            .await
            .expect("create");
        let second = store
            .create_entity("Ada Lovelace", "person", serde_json::json!({}))
            .await
            .expect("get-or-create");
        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn entity_kind_is_normalized_so_casing_does_not_fragment_the_graph() {
        let store = memory_store().await;
        let first = store
            .create_entity("Ada Lovelace", "Person", serde_json::json!({}))
            .await
            .expect("create");
        let second = store
            .create_entity("Ada Lovelace", "person", serde_json::json!({}))
            .await
            .expect("get-or-create despite different casing");
        assert_eq!(first.id, second.id);
        assert_eq!(second.kind, "person");
    }

    #[tokio::test]
    async fn an_empty_kind_or_predicate_is_rejected() {
        let store = memory_store().await;
        let entity_err = store
            .create_entity("Ada Lovelace", "   ", serde_json::json!({}))
            .await
            .expect_err("blank kind must be rejected");
        assert!(matches!(entity_err, crate::error::Error::EmptyLabel { .. }));

        let alice = store
            .create_entity("Alice", "person", serde_json::json!({}))
            .await
            .expect("alice");
        let bob = store
            .create_entity("Bob", "person", serde_json::json!({}))
            .await
            .expect("bob");
        let relationship_err = store
            .create_relationship(alice.id, bob.id, "", 1.0)
            .await
            .expect_err("blank predicate must be rejected");
        assert!(matches!(
            relationship_err,
            crate::error::Error::EmptyLabel { .. }
        ));
    }

    #[tokio::test]
    async fn superseding_a_relationship_closes_the_old_edge_and_leaves_history_queryable() {
        let store = memory_store().await;
        let alice = store
            .create_entity("Alice", "person", serde_json::json!({}))
            .await
            .expect("alice");
        let acme = store
            .create_entity("Acme", "organization", serde_json::json!({}))
            .await
            .expect("acme");

        let original = store
            .create_relationship(alice.id, acme.id, "employee_of", 0.9)
            .await
            .expect("create relationship");

        let replacement = store
            .supersede_relationship(
                original.id,
                crate::domain::NewRelationship {
                    from: alice.id,
                    to: acme.id,
                    predicate: "former_employee_of".to_string(),
                    confidence: 0.95,
                },
            )
            .await
            .expect("supersede");
        assert_ne!(replacement.id, original.id);

        let current = store
            .list_relationships(alice.id, false)
            .await
            .expect("list current");
        assert_eq!(
            current.len(),
            1,
            "only the replacement should be current, got {current:?}"
        );
        assert_eq!(current[0].id, replacement.id);
        assert_eq!(current[0].predicate, "former_employee_of");

        let history = store
            .list_relationships(alice.id, true)
            .await
            .expect("list including expired");
        assert_eq!(
            history.len(),
            2,
            "both the old and new edge should stay queryable, got {history:?}"
        );
        let old = history
            .iter()
            .find(|r| r.id == original.id)
            .expect("original edge still present");
        assert!(
            old.valid_to.is_some(),
            "the superseded edge must be closed, got {old:?}"
        );
    }

    #[tokio::test]
    async fn invalidating_a_relationship_sets_valid_to_without_creating_a_replacement() {
        let store = memory_store().await;
        let alice = store
            .create_entity("Alice", "person", serde_json::json!({}))
            .await
            .expect("alice");
        let acme = store
            .create_entity("Acme", "organization", serde_json::json!({}))
            .await
            .expect("acme");
        let relationship = store
            .create_relationship(alice.id, acme.id, "employee_of", 0.9)
            .await
            .expect("create relationship");

        store
            .invalidate_relationship(relationship.id)
            .await
            .expect("invalidate");

        let current = store
            .list_relationships(alice.id, false)
            .await
            .expect("list current");
        assert!(
            current.is_empty(),
            "an invalidated relationship must not read back as current, got {current:?}"
        );

        let history = store
            .list_relationships(alice.id, true)
            .await
            .expect("list including expired");
        assert_eq!(
            history.len(),
            1,
            "invalidating must not create a replacement edge, got {history:?}"
        );
        assert!(history[0].valid_to.is_some());
    }
}
