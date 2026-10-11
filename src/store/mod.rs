//! The SurrealDB storage abstraction.
//!
//! One connection type (`Surreal<Any>`, via `engine::any`) for both embedded
//! and remote deployments — the rest of the codebase never branches on which
//! backend is active. Repository methods live in the sibling modules
//! (`wings`, `drawers`, `palace`, `jobs`, `entities`, `graph`, `retrieval`, `duplicates`, `resolution`, `extraction`,
//! `sources`, `source_packages`, `auth`, `timestamps`, `migration_state`) as `impl SurrealStore` blocks;
//! this file only owns connecting and schema sync.
//!
//! Method names say what they do: `get_*` reads one record, `list_*` reads
//! many, `create_*` inserts (replay-safe `_once` forms skip an existing id),
//! `save_*` upserts a whole record, and `get_or_create_*` reads and inserts
//! when absent. Callers own ids and timestamps, except inside
//! `get_or_create_*` — see `domain::ids` for why.
//!
//! Timestamps are written only through [`stored`], in one canonical string
//! form; `docs/adr/005-timestamp-representation.md` records why some columns
//! are `datetime` and the optional ones `option<string>`.
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

mod auth;
mod drawers;
mod duplicates;
mod entities;
mod extraction;
mod graph;
mod jobs;
mod lifecycle;
mod migration_state;
mod palace;
mod plugins;
mod renames;
mod resolution;
mod retrieval;
mod source_packages;
mod sources;
#[cfg(test)]
mod temporal_tests;
mod timestamps;
mod triggers;
mod wings;

use std::path::PathBuf;

use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;

use crate::domain::Secret;
use crate::error::{Error, Result};

pub use duplicates::{DrawerCandidate, SimilarDrawer, SimilarSide};
pub use resolution::{PossibleEntity, PossibleSide};
pub use retrieval::MatchMode;

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
    serde_json::to_value(value)
        .map_err(|source| Error::serialization("a value bound for storage", source))
}

/// The one form a timestamp is written to the database in: UTC, nine
/// fractional digits, a `Z` suffix — `2026-09-29T14:22:47.123456789Z`.
///
/// Fixed width and fixed offset, so comparing two stored strings
/// lexicographically is the same as comparing the instants, which is what
/// makes the `option<string>` timestamp columns safe to `ORDER BY` or range
/// over; `DateTime::to_rfc3339`'s variable precision and `+00:00` suffix are
/// not (`...47.5+00:00` sorts after `...47.25+00:00` lexically only by
/// accident of digit count). Nanoseconds because that is what `datetime`
/// columns hold, so nothing is truncated on the way in. Every timestamp write
/// in this module goes through here — see
/// `docs/adr/005-timestamp-representation.md`.
pub(crate) fn stored(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

/// Whether `error` is SurrealDB reporting a write conflict — two transactions
/// touching the same record at once — which it documents as safe to retry.
fn is_write_conflict(error: &Error) -> bool {
    matches!(error, Error::Store { source } if is_conflict(source))
}

/// [`is_write_conflict`] for the driver's own error, before it is wrapped.
fn is_conflict(error: &surrealdb::Error) -> bool {
    matches!(
        error.query_details(),
        Some(surrealdb::types::QueryError::TransactionConflict)
    )
}

/// The error that explains why a query failed, out of every statement's.
///
/// When a `BEGIN`/`COMMIT` transaction fails, *every* statement in it reports
/// an error, but only one names the cause; the rest are `NotExecuted`
/// ("The query was not executed due to a failed transaction"). The driver's
/// own `check()` returns the first in statement order, which is usually one of
/// the useless ones, so a lost write conflict reached callers (and
/// [`retrying_on_conflict`]) as `NotExecuted` and was neither retried nor
/// diagnosable. A conflict wins, then any real cause, then whatever is left.
pub(crate) fn root_cause(
    errors: impl IntoIterator<Item = (usize, surrealdb::Error)>,
) -> Option<surrealdb::Error> {
    let mut errors: Vec<_> = errors.into_iter().collect();
    // The driver hands them back in a hash map; statement order is what makes
    // "the first real cause" mean something.
    errors.sort_by_key(|(index, _)| *index);
    let rank = |error: &surrealdb::Error| {
        if is_conflict(error) {
            0
        } else if matches!(
            error.query_details(),
            Some(surrealdb::types::QueryError::NotExecuted)
        ) {
            2
        } else {
            1
        }
    };
    errors
        .into_iter()
        .min_by_key(|(_, error)| rank(error))
        .map(|(_, error)| error)
}

/// `Response::check`, but reporting [`root_cause`] instead of the first
/// statement's error. Use it for every query that contains a transaction.
pub(crate) fn checked(
    mut response: surrealdb::IndexedResults,
) -> Result<surrealdb::IndexedResults> {
    // Empty when every statement succeeded, in which case nothing was removed
    // from `response` and it is returned intact.
    match root_cause(response.take_errors()) {
        Some(error) => Err(error.into()),
        None => Ok(response),
    }
}

/// Run `operation`, retrying a few times if it loses a write conflict.
///
/// Two things write the same keys at once. A job record has two concurrent
/// writers by design: the worker checkpointing its progress, and the API
/// recording a user's pause or cancel. And SurrealDB rewrites the full-text and
/// vector index keys of `drawer` from a background compaction task that wakes
/// shortly after every commit, so any drawer write can overlap it. Either way
/// SurrealDB resolves the race by failing one transaction with a retryable
/// conflict; without a retry the loser surfaced as a failed checkpoint
/// (killing the job), a 500 on the user's request or a flaky test. The failed
/// transaction wrote nothing, so `operation` must be a single statement or
/// transaction and is simply run again.
pub(crate) async fn retrying_on_conflict<T, F, Fut>(mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    const MAX_RETRIES: u32 = 8;
    let mut retries = 0;
    loop {
        match operation().await {
            Err(error) if is_write_conflict(&error) && retries < MAX_RETRIES => {
                retries += 1;
                // A short, growing pause so the winner can commit.
                tokio::time::sleep(std::time::Duration::from_millis(u64::from(retries) * 5)).await;
            }
            other => return other,
        }
    }
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

/// How often an embedded palace forces its writes to disk.
///
/// Every commit is its own fsync by default, which is what makes a crash lose
/// nothing acknowledged, and also what makes a fresh boot (a few hundred
/// schema statements) take seconds on a disk with a slow flush, such as the
/// one on a Windows CI runner. The relaxed modes trade that guarantee for
/// speed and exist for tests and throwaway palaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
// A single string in the config file (`sync = "never"`), validated by `FromStr`.
#[serde(try_from = "String", into = "String")]
pub enum StoreSync {
    /// Flush on every commit: nothing acknowledged is lost on a crash.
    #[default]
    Every,
    /// Leave flushing to the operating system: fastest, and the last
    /// commits can be lost if the machine (not just the daemon) dies.
    Never,
    /// Flush in the background on this interval. SurrealKV refuses anything
    /// at or below 100 ms, so [`FromStr`](std::str::FromStr) does too.
    Interval(std::time::Duration),
}

impl std::str::FromStr for StoreSync {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, String> {
        let raw = raw.trim().to_ascii_lowercase();
        match raw.as_str() {
            "every" => return Ok(Self::Every),
            "never" => return Ok(Self::Never),
            _ => {}
        }
        // Longest suffix first: `ms` ends in `s`, so `s` would otherwise
        // swallow `250ms` and fail to parse `250m` as a number.
        let (digits, unit_ms) = if let Some(n) = raw.strip_suffix("ms") {
            (n, 1)
        } else if let Some(n) = raw.strip_suffix('s') {
            (n, 1_000)
        } else if let Some(n) = raw.strip_suffix('m') {
            (n, 60_000)
        } else {
            // A bare number has no unit to scale by: a zero multiplier makes
            // it fail the `> 100` check below rather than guess seconds.
            (raw.as_str(), 0)
        };
        let invalid =
            || "expected `every`, `never`, or an interval over 100ms such as `500ms`, `5s` or `1m`";
        let millis = digits
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(unit_ms))
            .filter(|millis| *millis > 100)
            .ok_or_else(invalid)?;
        Ok(Self::Interval(std::time::Duration::from_millis(millis)))
    }
}

impl std::fmt::Display for StoreSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Every => f.write_str("every"),
            Self::Never => f.write_str("never"),
            // Milliseconds always: the one unit that round-trips through
            // `from_str` (and SurrealDB's own parser) without loss.
            Self::Interval(interval) => write!(f, "{}ms", interval.as_millis()),
        }
    }
}

impl From<StoreSync> for String {
    fn from(sync: StoreSync) -> Self {
        sync.to_string()
    }
}

impl TryFrom<String> for StoreSync {
    type Error = String;

    fn try_from(raw: String) -> std::result::Result<Self, String> {
        raw.parse()
    }
}

/// Where the palace's data actually lives.
#[derive(Debug, Clone)]
pub enum Backend {
    /// A local SurrealKV directory — the default developer experience.
    Embedded {
        /// The directory SurrealDB should own. Created if missing.
        path: PathBuf,
        /// How often writes are forced to disk.
        sync: StoreSync,
    },
    /// A remotely hosted SurrealDB instance.
    Remote {
        /// e.g. `ws://localhost:8000` or `wss://db.example.com`.
        url: String,
        /// The namespace to select after connecting.
        namespace: String,
        /// The database to select after connecting.
        database: String,
        /// Root username (`connect` always signs in as root).
        username: String,
        /// Root password. A [`Secret`] so the derived `Debug` redacts it.
        password: Secret,
    },
}

impl Backend {
    /// Whether other daemons can connect to the same palace at the same time.
    /// An embedded palace cannot: SurrealKV's file lock admits one process,
    /// which is what lets startup recovery treat every `Running` job as a
    /// dead predecessor's. A remote one can, so recovery must go by lease.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    /// A description of this backend that is safe to print or serve over the
    /// API: the kind and where it lives, with no credentials. `Remote` carries
    /// a root password, and a `status` response is read by anyone who can
    /// reach the port, so the URL's userinfo (`user:pass@`) is dropped too.
    #[must_use]
    pub fn describe(&self) -> BackendInfo {
        match self {
            Self::Embedded { path, .. } => BackendInfo {
                kind: "embedded",
                location: path.display().to_string(),
            },
            Self::Remote { url, .. } => BackendInfo {
                kind: "remote",
                location: strip_userinfo(url),
            },
        }
    }

    /// The endpoint string `engine::any::connect` dispatches on.
    fn endpoint(&self) -> String {
        match self {
            // Single colon, no slashes: `surrealkv:` is the scheme, what
            // follows is the path verbatim (`surrealkv://` would make the
            // first path segment look like a host).
            Self::Embedded { path, sync } => {
                let endpoint = format!("surrealkv:{}", path.display());
                match sync {
                    // The default is left implicit so the common endpoint
                    // stays exactly what it was before this setting existed.
                    StoreSync::Every => endpoint,
                    // Query parameters become `datastore_*` options in
                    // SurrealDB's `any` engine, which strips them from the path.
                    _ => format!("{endpoint}?sync={sync}"),
                }
            }
            Self::Remote { url, .. } => url.clone(),
        }
    }
}

/// What [`Backend::describe`] reports: enough to answer "which datastore is
/// this?" without carrying a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// `"embedded"` or `"remote"`.
    pub kind: &'static str,
    /// The directory (embedded) or credential-free URL (remote).
    pub location: String,
}

/// `url` without any `user[:password]@` part of its authority. A URL with no
/// scheme separator is returned untouched: it cannot carry userinfo the way
/// `scheme://user:pass@host` does, and guessing would mangle it.
fn strip_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    // The authority ends at the first `/`, `?` or `#`; an `@` after that
    // belongs to the path or query, not to userinfo.
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    // `rsplit` because a password may itself contain an unescaped `@`.
    let host = authority.rsplit('@').next().unwrap_or(authority);
    format!("{scheme}://{host}{tail}")
}

/// The namespace an embedded palace lives in. Named so the admin endpoint can
/// tell SurrealDB Studio which one to select, instead of repeating the literal.
pub const EMBEDDED_NAMESPACE: &str = "memcastle";
/// The database an embedded palace lives in.
pub const EMBEDDED_DATABASE: &str = "palace";

/// A connected handle to one palace's storage. Connecting does not migrate —
/// see [`SurrealStore::connect`].
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
        if let Backend::Embedded { path, .. } = backend {
            std::fs::create_dir_all(path)
                .map_err(|source| crate::Error::io(path.display().to_string(), source))?;
        }

        let db = any::connect(backend.endpoint()).await?;

        let (namespace, database) = match backend {
            Backend::Embedded { .. } => (EMBEDDED_NAMESPACE, EMBEDDED_DATABASE),
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
                    password: password.expose().to_string(),
                })
                .await?;
                (namespace.as_str(), database.as_str())
            }
        };
        db.use_ns(namespace).use_db(database).await?;

        Ok(Self { db })
    }

    /// A new session over this store's own database, for the admin endpoint
    /// (`crate::dbadmin`).
    ///
    /// Cloning a `Surreal` handle does not open anything: it creates another
    /// session over the same connection, starting from this one's namespace,
    /// database and variables. That is what lets Studio query the *live* embedded
    /// database without a second process ever opening the SurrealKV directory,
    /// and what keeps a Studio `USE` from moving the daemon's own queries.
    pub(crate) fn session(&self) -> Surreal<Any> {
        self.db.clone()
    }

    /// Prove the datastore answers a query right now, touching no table.
    /// `status` uses this to tell "the daemon is up" from "the daemon is up
    /// but its database is not", which the static `/api/health` cannot.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Store`] if the connection is down or the
    /// query fails.
    pub async fn ping(&self) -> Result<()> {
        // `.check()` because a failed statement arrives as a per-statement
        // error inside an `Ok` response, which would read as healthy.
        self.db.query("RETURN 1").await?.check()?;
        Ok(())
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

    fn query_error(details: surrealdb::types::QueryError) -> surrealdb::Error {
        surrealdb::Error::query("boom".to_string(), details)
    }

    #[test]
    fn a_failed_transaction_reports_its_conflict_not_its_not_executed_statements() {
        use surrealdb::types::QueryError::{NotExecuted, TransactionConflict};
        // Statement order as a conflicted `BEGIN`/`COMMIT` reports it: the
        // useless errors first, the conflict later.
        let cause = root_cause([
            (0, query_error(NotExecuted)),
            (1, query_error(NotExecuted)),
            (2, query_error(TransactionConflict)),
        ])
        .expect("a cause");
        assert!(is_conflict(&cause), "{cause:?}");
    }

    #[test]
    fn a_real_failure_is_reported_over_the_statements_it_prevented() {
        use surrealdb::types::QueryError::NotExecuted;
        let cause = root_cause([
            (0, query_error(NotExecuted)),
            (1, surrealdb::Error::internal("disk full".to_string())),
            (2, query_error(NotExecuted)),
        ])
        .expect("a cause");
        assert_eq!(cause.message(), "disk full");
        assert!(root_cause([]).is_none(), "no errors means no cause");
    }

    #[tokio::test]
    async fn a_conflict_is_retried_until_it_clears_and_other_errors_are_not() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let conflict = || {
            Error::from(query_error(
                surrealdb::types::QueryError::TransactionConflict,
            ))
        };

        let attempts = AtomicU32::new(0);
        let result = retrying_on_conflict(|| async {
            match attempts.fetch_add(1, Ordering::SeqCst) {
                0..=2 => Err(conflict()),
                _ => Ok("written"),
            }
        })
        .await;
        assert_eq!(result.expect("retried to success"), "written");
        assert_eq!(attempts.load(Ordering::SeqCst), 4);

        let attempts = AtomicU32::new(0);
        let result: Result<()> = retrying_on_conflict(|| async {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(Error::store_malformed("not a conflict"))
        })
        .await;
        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "only conflicts retry");
    }

    #[tokio::test]
    async fn a_live_store_answers_a_ping() {
        // `status` reports the datastore as healthy on the strength of this.
        memory_store().await.ping().await.expect("ping");
    }

    fn remote(url: &str) -> Backend {
        Backend::Remote {
            url: url.to_string(),
            namespace: "n".into(),
            database: "d".into(),
            username: "root".into(),
            password: Secret::new("hunter2"),
        }
    }

    #[test]
    fn a_remote_backend_description_never_carries_credentials() {
        // The description is served on an unauthenticated endpoint.
        for url in [
            "ws://root:hunter2@db.example.com:8000",
            "wss://root:hunter2@db.example.com/rpc",
            "ws://root:p@ss@db.example.com:8000",
        ] {
            let info = remote(url).describe();
            assert_eq!(info.kind, "remote");
            assert!(!info.location.contains("hunter2"), "{}", info.location);
            assert!(!info.location.contains("p@ss"), "{}", info.location);
            assert!(!info.location.contains('@'), "{}", info.location);
        }
        assert_eq!(
            remote("wss://root:x@db.example.com/rpc")
                .describe()
                .location,
            "wss://db.example.com/rpc"
        );
    }

    #[test]
    fn an_at_sign_in_the_path_is_not_mistaken_for_userinfo() {
        // Stripping up to the last `@` of the whole URL would eat the host.
        assert_eq!(
            remote("ws://db.example.com/a@b").describe().location,
            "ws://db.example.com/a@b"
        );
    }

    #[test]
    fn an_embedded_backend_describes_its_directory() {
        let info = Backend::Embedded {
            path: PathBuf::from("/data/palace/db"),
            sync: StoreSync::default(),
        }
        .describe();
        assert_eq!(info.kind, "embedded");
        assert_eq!(info.location, "/data/palace/db");
    }

    fn embedded(sync: StoreSync) -> Backend {
        Backend::Embedded {
            path: PathBuf::from("/data/palace/db"),
            sync,
        }
    }

    #[test]
    fn the_default_sync_leaves_the_endpoint_exactly_as_it_was() {
        // A palace that never set `sync` must keep opening the way it always did.
        assert_eq!(
            embedded(StoreSync::Every).endpoint(),
            "surrealkv:/data/palace/db"
        );
    }

    #[test]
    fn a_relaxed_sync_reaches_the_endpoint_as_a_query_parameter() {
        assert_eq!(
            embedded(StoreSync::Never).endpoint(),
            "surrealkv:/data/palace/db?sync=never"
        );
        assert_eq!(
            embedded("2s".parse().unwrap()).endpoint(),
            "surrealkv:/data/palace/db?sync=2000ms"
        );
    }

    #[test]
    fn sync_modes_parse_and_print_the_same_way() {
        assert_eq!("every".parse(), Ok(StoreSync::Every));
        assert_eq!(" NEVER ".parse(), Ok(StoreSync::Never));
        assert_eq!(
            "250ms".parse(),
            Ok(StoreSync::Interval(std::time::Duration::from_millis(250)))
        );
        assert_eq!(
            "1m".parse(),
            Ok(StoreSync::Interval(std::time::Duration::from_secs(60)))
        );
        for sync in [StoreSync::Every, StoreSync::Never, "5s".parse().unwrap()] {
            assert_eq!(sync.to_string().parse(), Ok(sync));
        }
    }

    #[test]
    fn a_sync_value_nothing_can_honour_is_refused() {
        // 100ms and below is SurrealKV's own floor; a bare number has no unit.
        for bad in ["", "sometimes", "100ms", "0s", "30", "-5s", "1h"] {
            assert!(bad.parse::<StoreSync>().is_err(), "{bad:?} must be refused");
        }
    }

    #[tokio::test]
    async fn a_store_opens_with_every_sync_mode_the_endpoint_can_carry() {
        // The query string is SurrealDB's to interpret; this proves it accepts
        // what `endpoint()` builds instead of rejecting it as part of the path.
        for sync in [StoreSync::Never, "1s".parse().unwrap()] {
            let dir = tempfile::tempdir().expect("tempdir");
            let backend = Backend::Embedded {
                path: dir.path().join("palace"),
                sync,
            };
            let store = SurrealStore::connect(&backend)
                .await
                .unwrap_or_else(|e| panic!("connect with sync={sync}: {e}"));
            store.sync_schema().await.expect("sync schema");
            store.ping().await.expect("ping");
            // Dropping the handle does not release the file lock (see the note
            // below), but the tempdir is private to this iteration.
        }
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
        // The default sync, deliberately: this is the one test that opens a
        // real durable palace the way production does.
        let backend = Backend::Embedded {
            path: dir.path().join("palace"),
            sync: StoreSync::default(),
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
            name: None,
            content: "hello palace".into(),
            content_hash: "abc".into(),
            source: crate::domain::Source {
                kind: crate::domain::SourceKind::Manual,
                uri: None,
                agent: Some("test".into()),
                origin: None,
            },
            tags: vec![],
            embedding: None,
            provenance: crate::domain::Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
            valid_from: chrono::Utc::now(),
            valid_to: None,
            supersedes: None,
            superseded_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        store.create_drawer(&drawer).await.expect("create drawer");

        let drawers = store.list_drawers(Some(room.id)).await.expect("list");
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
            .claim_next_job("worker-1", chrono::Duration::seconds(30))
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

    #[tokio::test]
    async fn a_full_background_pool_skips_a_mine_and_claims_short_work() {
        use crate::domain::{Job, JobKind, JobStatus, MiningSource, Priority};

        let store = memory_store().await;
        let mine = Job::new(
            JobKind::Mine {
                source: MiningSource::Directory {
                    path: "/tmp/load".into(),
                },
                wing: None,
                full: false,
                options: Default::default(),
            },
            Priority::Critical,
            "test",
        );
        let short = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        store.save_job(&mine).await.unwrap();
        store.save_job(&short).await.unwrap();

        let claimed = store
            .claim_next_job_eligible("worker", chrono::Duration::seconds(30), false)
            .await
            .unwrap()
            .expect("short job is still eligible");
        assert_eq!(claimed.id, short.id);
        assert_eq!(
            store.get_job(mine.id).await.unwrap().unwrap().status,
            JobStatus::Queued
        );

        let claimed = store
            .claim_next_job_eligible("worker", chrono::Duration::seconds(30), true)
            .await
            .unwrap()
            .expect("mine becomes eligible when a slot opens");
        assert_eq!(claimed.id, mine.id);
    }

    #[tokio::test]
    async fn a_status_guarded_save_refuses_to_overwrite_a_job_that_moved_on() {
        use crate::domain::{JobEvent, JobStatus};

        let store = memory_store().await;
        let job = crate::domain::Job::new(
            crate::domain::JobKind::Demo { steps: 1 },
            crate::domain::Priority::Normal,
            "test",
        );
        store.save_job(&job).await.expect("save queued");

        // A stale copy, read while the job was still queued, is turned into a
        // cancellation...
        let mut stale = store.get_job(job.id).await.expect("get").expect("present");
        stale.apply(JobEvent::Cancel).expect("cancel from queued");

        // ...but a worker claims the job before that copy is saved.
        store
            .claim_next_job("worker-1", chrono::Duration::seconds(30))
            .await
            .expect("claim")
            .expect("claimed");

        let saved = store
            .save_job_if_status(&stale, JobStatus::Queued)
            .await
            .expect("guarded save");

        assert!(!saved, "the guard must report that nothing was written");
        let stored = store.get_job(job.id).await.expect("get").expect("present");
        assert_eq!(
            stored.status,
            JobStatus::Running,
            "the claim must not be clobbered by the stale cancellation"
        );
        assert_eq!(stored.lease_owner.as_deref(), Some("worker-1"));

        // With the status still as expected, the same save goes through.
        let mut fresh = stored.clone();
        fresh.apply(JobEvent::Complete).expect("complete");
        assert!(
            store
                .save_job_if_status(&fresh, JobStatus::Running)
                .await
                .expect("guarded save")
        );
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

    /// A filter scoping by wing and/or room name, everything else default.
    fn scope(wing: Option<&str>, room: Option<&str>) -> crate::domain::SearchFilter {
        crate::domain::SearchFilter {
            wing: wing.map(str::to_string),
            room: room.map(str::to_string),
            ..Default::default()
        }
    }

    /// A minimal drawer fixture for search tests — only `room` and
    /// `content` vary between callers; everything else is filler a
    /// full-text search test doesn't care about.
    fn test_drawer(room: crate::domain::RoomId, content: &str) -> crate::domain::Drawer {
        crate::domain::Drawer {
            id: crate::domain::DrawerId::new(),
            room,
            name: None,
            content: content.to_string(),
            content_hash: "hash".into(),
            source: crate::domain::Source {
                kind: crate::domain::SourceKind::Manual,
                uri: None,
                agent: Some("test".into()),
                origin: None,
            },
            tags: vec![],
            embedding: None,
            provenance: crate::domain::Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
            valid_from: chrono::Utc::now(),
            valid_to: None,
            supersedes: None,
            superseded_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// A store holding one drawer, for the match-mode tests below.
    async fn store_with_one_drawer(content: &str) -> SurrealStore {
        let store = memory_store().await;
        let wing = store.get_or_create_wing("alpha", None).await.expect("wing");
        let room = store
            .get_or_create_room(wing.id, "notes", None)
            .await
            .expect("room");
        store
            .create_drawer(&test_drawer(room.id, content))
            .await
            .expect("create drawer");
        store
    }

    #[tokio::test]
    async fn all_mode_requires_every_query_term() {
        let store =
            store_with_one_drawer("my main programming languages are Rust and Python").await;

        let hits = store
            .search_lexical(
                "programming languages I use preferences",
                10,
                &scope(None, None),
                MatchMode::All,
            )
            .await
            .expect("search");
        assert!(
            hits.is_empty(),
            "terms absent from the drawer must defeat a strict match, got {hits:?}"
        );
    }

    #[tokio::test]
    async fn any_mode_matches_on_a_single_shared_term() {
        let store =
            store_with_one_drawer("my main programming languages are Rust and Python").await;

        let hits = store
            .search_lexical(
                "programming languages I use preferences",
                10,
                &scope(None, None),
                MatchMode::Any,
            )
            .await
            .expect("search");
        assert_eq!(hits.len(), 1, "a shared term should be enough: {hits:?}");
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
            .search_lexical("castle", 10, &scope(None, None), MatchMode::All)
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
            .search_lexical("castle", 10, &scope(Some("alpha"), None), MatchMode::All)
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
            .search_lexical("castle", 10, &scope(None, Some("notes")), MatchMode::All)
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

        // Drawers that never mention the term. BM25's inverse document
        // frequency is zero when *every* drawer matches, which would make
        // every score 0 and leave the order to insertion luck; with fillers
        // the score gap below is real.
        for i in 0..6 {
            store
                .create_drawer(&test_drawer(loud_room.id, &format!("unrelated filler {i}")))
                .await
                .expect("create filler drawer");
        }
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
            .search_lexical("castle", 2, &scope(None, None), MatchMode::All)
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
            .search_lexical("castle", 2, &scope(Some("quiet"), None), MatchMode::All)
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
                full: false,
                options: Default::default(),
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

    #[test]
    fn stored_timestamps_are_fixed_width_utc_so_lexical_order_is_chronological() {
        use chrono::{TimeZone, Utc};
        let earlier = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        // Half a second later: `to_rfc3339` would print `05.5+00:00`, which
        // sorts *before* a `05+00:00` neighbour by digit count alone.
        let later = earlier + chrono::Duration::milliseconds(500);

        assert_eq!(stored(earlier), "2026-01-02T03:04:05.000000000Z");
        assert_eq!(stored(later), "2026-01-02T03:04:05.500000000Z");
        assert!(stored(earlier) < stored(later));
        assert_eq!(stored(earlier).len(), stored(later).len());
    }
}
