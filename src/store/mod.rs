//! The SurrealDB storage abstraction.
//!
//! One connection type (`Surreal<Any>`, via `engine::any`) for both embedded
//! and remote deployments — the rest of the codebase never branches on which
//! backend is active. Repository methods live in the sibling modules
//! (`wings`, `drawers`, `jobs`) as `impl SurrealStore` blocks; this file only
//! owns connecting and migrating.
//!
//! Every write and read goes through hand-written SurrealQL with explicit
//! `<datetime>`/`<string>` casts rather than the SDK's typed `create`/
//! `select` helpers or its `Datetime`/`RecordId` wrapper types. That costs
//! some verbosity, but it means the only surface this module depends on is
//! `Surreal::query`/`bind`/`take` — the smallest, most stable part of the
//! API — instead of type-coercion behaviour between chrono and the driver's
//! own serde bridge that would otherwise have to be discovered by trial and
//! error.

mod drawers;
mod jobs;
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

/// One migration file, applied in order and idempotently (every `DEFINE` in
/// it is `IF NOT EXISTS`), so `Store::connect` can just re-run all of them
/// on every startup instead of tracking an applied-version watermark.
const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_init.surql")];

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
    /// Connect to `backend`, select its namespace/database, and apply every
    /// pending migration.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] if the connection, sign-in, or a
    /// migration statement fails.
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

        let store = Self { db };
        store.migrate().await?;
        Ok(store)
    }

    /// Apply every migration file. Safe to call repeatedly.
    async fn migrate(&self) -> Result<()> {
        for migration in MIGRATIONS {
            // `.await` alone only reports transport-level failures; a
            // malformed `DEFINE` inside the migration would otherwise fail
            // silently. `.check()` promotes the first per-statement error,
            // if any, to a real `Err` — see the module doc.
            self.db.query(*migration).await?.check()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_store() -> SurrealStore {
        // `engine::any` dispatches the literal string `"memory"` to the
        // `kv-mem` engine — no scheme prefix, unlike `surrealkv:`.
        let db = any::connect("memory").await.expect("connect");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        let store = SurrealStore { db };
        store.migrate().await.expect("migrate");
        store
    }

    #[tokio::test]
    async fn migrations_apply_cleanly_and_are_idempotent() {
        let store = memory_store().await;
        // Re-running must not error — every DEFINE is IF NOT EXISTS.
        store.migrate().await.expect("second migrate");
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
}
