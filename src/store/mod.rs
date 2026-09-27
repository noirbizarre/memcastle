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
//! (expensive, RocksDB-relinking) error.

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
    /// A local RocksDB directory — the default developer experience.
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
            // Single slash: `rocksdb:` is the scheme, what follows is the
            // path verbatim (`rocksdb://` would make the first path segment
            // look like a host).
            Self::Embedded { path } => format!("rocksdb:{}", path.display()),
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
        // `kv-mem` engine — no scheme prefix, unlike `rocksdb:`.
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

    // "Reopen the same RocksDB path in the same process" is deliberately
    // NOT exercised here: SurrealDB's embedded engine does not release the
    // OS-level RocksDB lock when a `Surreal` handle drops within the same
    // process (confirmed empirically — it does not clear even after several
    // seconds of retrying), so a unit test doing that would be testing a
    // driver quirk, not `SurrealStore`. The real "does data survive a
    // restart" guarantee is proven at the process boundary instead, in
    // `tests/persistence.rs`, which spawns two genuinely separate
    // `memcastle serve` processes against the same palace directory.
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
}
