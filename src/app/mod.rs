//! Application services: the one layer the CLI (for `serve`), the HTTP API,
//! and the MCP tools all call into.
//!
//! This is the layer the architecture constraint is actually about: nothing
//! under `api`/`mcp`/`cli` may reach past this into `store` or `jobs`
//! directly (enforced by a `prek` grep hook — see `AGENTS.md`), so a web
//! dashboard or a new transport can only ever do what this struct exposes.

mod auth;
mod db_endpoint;
mod palace;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{
    CheckpointDestination, CheckpointPayload, Drawer, DrawerId, FactMutation, Job, JobId, JobKind,
    JobStatus, MemoryMode, MiningSource, NameKind, Priority, Provenance, Source, SourceKind,
    validate_name,
};
use crate::error::{Error, Result};
use crate::jobs::Scheduler;
use crate::store::{SearchHit, SurrealStore};

pub use auth::{AuthPolicy, GeneratedToken, RevokeResult};
pub use db_endpoint::{DbEndpoint, DbEndpointRequest, DbEndpointStatus};
pub use palace::{Created, DEFAULT_LIST_LIMIT, WingDetail};

/// A point-in-time summary of daemon health, for `GET /api/status`,
/// `memcastle status`, and the `memcastle_status` MCP tool alike.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReport {
    /// The running binary's version.
    pub version: String,
    /// Seconds since the daemon started.
    pub uptime_secs: i64,
    /// The palace's display name.
    pub palace_name: String,
    /// Total drawers currently stored.
    pub drawer_count: u64,
    /// Jobs currently `Queued`.
    pub jobs_queued: u64,
    /// Jobs currently `Running`.
    pub jobs_running: u64,
    /// Jobs currently `Paused`.
    pub jobs_paused: u64,
    /// The effective [`MemoryMode`] for the request/session that asked for
    /// this report — purely observational, `status` itself is never gated
    /// (see `MemoryMode`'s doc comment on daemon vs memory operations): a
    /// disabled session can still see "MemCastle: disabled" here.
    pub mode: MemoryMode,
    // Everything below arrived after the fields above. `#[serde(default)]` so
    // a newer CLI can still read an older daemon's report (and the reverse).
    /// The daemon process's PID.
    #[serde(default)]
    pub pid: u32,
    /// When the daemon started.
    #[serde(default)]
    pub started_at: DateTime<Utc>,
    /// The address the daemon's listener is actually bound to (the real port
    /// even when `0` was requested). Empty when the daemon did not say.
    #[serde(default)]
    pub bind_addr: String,
    /// The palace directory being served. Empty when the daemon did not say.
    #[serde(default)]
    pub palace_path: String,
    /// The datastore's health and migration state.
    #[serde(default)]
    pub datastore: DatastoreStatus,
    /// Whether the daemon requires a bearer token. Only the fact, never any
    /// credential: this report is served to whoever can call `status`.
    #[serde(default)]
    pub auth_enabled: bool,
}

/// The datastore section of a [`StatusReport`]: can the daemon reach its
/// database right now, and is the database at the version this binary expects.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatastoreStatus {
    /// Whether a ping succeeded, and (when it did) the migration state was read.
    pub ok: bool,
    /// `embedded` or `remote`; empty when unknown.
    pub backend: String,
    /// Directory or credential-free URL of the datastore.
    pub location: String,
    /// Why `ok` is false, when it is.
    pub error: Option<String>,
    /// The migration watermark stored in the database.
    pub migration_version: u32,
    /// The newest migration this binary knows.
    pub latest_version: u32,
    /// Names of migrations not yet applied. A running daemon migrates before
    /// serving, so this is normally empty; it is reported so a shared remote
    /// palace migrated by a newer daemon is not silently served wrongly.
    pub pending: Vec<String>,
}

impl DatastoreStatus {
    /// Healthy means reachable *and* fully migrated.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.ok && self.pending.is_empty()
    }
}

/// Facts about how the daemon was started that the store and scheduler do not
/// know. Set once by `server::run`, after the listener is bound.
#[derive(Debug, Clone, Default)]
pub struct RuntimeContext {
    /// The listener's actual bound address.
    pub bind_addr: String,
    /// The served palace directory.
    pub palace_path: String,
    /// `embedded` or `remote`.
    pub backend: String,
    /// Directory or credential-free URL of the datastore.
    pub location: String,
}

/// Bounds `AppServices::wake_up`'s output — deterministic and testable, no
/// LLM-based summarization (task brief §13: this is L0/L1 only). Both
/// limits apply to `recent_highlights` only: `max_items` caps how many
/// drawers are fetched at all (pushed into the store query's `LIMIT`, and
/// clamped to [`MAX_READ_LIMIT`] like every other read);
/// `max_bytes` then caps their cumulative content length: a whole drawer that
/// would push the running total over it is left out (never truncated
/// mid-string) and the older, possibly smaller, ones after it are still
/// considered — see `AppServices::wake_up`'s doc comment. The single `diary`
/// entry, already capped at exactly one, is always included regardless of
/// `max_bytes`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WakeUpBudget {
    /// Maximum number of `recent_highlights` drawers to include.
    pub max_items: usize,
    /// Maximum total content bytes across `recent_highlights` (not
    /// counting `diary`).
    pub max_bytes: usize,
}

/// How many hits `search`/`recall` return when the caller does not say. One
/// value for every interface: the CLI, REST and MCP each used to carry their
/// own copy of `10`, and a change to one would have made the same query
/// return different amounts depending on how it was asked.
pub const DEFAULT_SEARCH_LIMIT: u32 = 10;

/// How many entries `diary_read` returns when the caller does not say —
/// shared by every interface for the same reason as [`DEFAULT_SEARCH_LIMIT`].
pub const DEFAULT_DIARY_LIMIT: u32 = 20;

/// The most hits or entries any one read returns, whatever the caller asks
/// for. Without a ceiling `limit=4294967295` is accepted verbatim and the
/// query is asked to materialise (and the response to serialize) the whole
/// palace. Clamped rather than rejected so an over-eager integration still
/// gets a useful answer instead of a failure it must special-case.
pub const MAX_READ_LIMIT: u32 = 200;

impl WakeUpBudget {
    /// A budget from optional caller-supplied limits, each falling back to
    /// [`WakeUpBudget::default`]'s value when omitted. The one place that
    /// merge is written, instead of once per interface.
    #[must_use]
    pub fn from_options(max_items: Option<usize>, max_bytes: Option<usize>) -> Self {
        let default = Self::default();
        Self {
            max_items: max_items.unwrap_or(default.max_items),
            max_bytes: max_bytes.unwrap_or(default.max_bytes),
        }
    }
}

impl Default for WakeUpBudget {
    /// Small enough to stay cheap to read or embed in a prompt, large
    /// enough to hold several short highlights.
    fn default() -> Self {
        Self {
            max_items: 10,
            max_bytes: 8192,
        }
    }
}

/// `AppServices::wake_up`'s result: an agent's session-start context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeUpContext {
    /// The agent's most recent diary entry, if any (see `wake_up`'s doc
    /// comment for when this is `None` even though entries exist).
    pub diary: Option<Drawer>,
    /// Up to `budget.max_items` most recent checkpoint-originated drawers,
    /// newest first, bounded by `budget.max_bytes`.
    pub recent_highlights: Vec<Drawer>,
    /// When this context was assembled — the only field that varies across
    /// otherwise-identical calls (see the determinism test).
    pub generated_at: DateTime<Utc>,
}

/// What a job-control request answers: the state the request left the job
/// heading for. One shape, served identically by REST, MCP and the CLI.
///
/// `pause_requested` and `cancel_requested` are *requests*, not outcomes:
/// stopping is cooperative, so the job may still be running when this comes
/// back (and, if the daemon dies first, is stopped by recovery instead).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct JobControlResult {
    /// What the request left the job heading for.
    pub status: JobControlStatus,
}

/// The answers a job-control request can give, serialized as the
/// `snake_case` words callers already read (`pause_requested`, `resumed`,
/// `cancel_requested`, `retried`). A closed enum rather than a `&'static str`
/// so `client::DaemonClient` can deserialize the daemon's answer into the same
/// type instead of handing callers an untyped `serde_json::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobControlStatus {
    /// A pause was asked for; the job stops at its next check.
    PauseRequested,
    /// A paused job was put back in the queue.
    Resumed,
    /// A cancel was asked for; a running job stops at its next check.
    CancelRequested,
    /// A failed job was put back in the queue.
    Retried,
}

/// The application services shared by every interface. Cheap to clone
/// (everything inside is a handle: `SurrealStore` wraps a connection,
/// `Scheduler` is behind an `Arc`), so it can be axum/rmcp request state
/// directly.
#[derive(Clone)]
pub struct AppServices {
    store: SurrealStore,
    scheduler: Arc<Scheduler>,
    started_at: DateTime<Utc>,
    runtime: Arc<RuntimeContext>,
    auth: Arc<AuthPolicy>,
    db_endpoint: Arc<DbEndpoint>,
}

impl AppServices {
    /// Wrap a connected store and running scheduler.
    #[must_use]
    pub fn new(store: SurrealStore, scheduler: Arc<Scheduler>) -> Self {
        Self {
            store,
            scheduler,
            started_at: Utc::now(),
            runtime: Arc::new(RuntimeContext::default()),
            auth: Arc::new(AuthPolicy::default()),
            db_endpoint: Arc::new(DbEndpoint::default()),
        }
    }

    /// Record how the daemon was started, for `status` to report.
    #[must_use]
    pub fn with_runtime(mut self, runtime: RuntimeContext) -> Self {
        self.runtime = Arc::new(runtime);
        self
    }

    /// An `AppServices` over a fresh in-memory store and an idle scheduler,
    /// for the unit tests of the layers above (`mcp` cannot construct a store
    /// itself: the `store-isolation` hook forbids it).
    #[cfg(test)]
    pub(crate) async fn for_tests() -> Self {
        let store = SurrealStore::connect_memory_for_tests().await;
        let scheduler = Arc::new(Scheduler::new(store.clone(), 1));
        Self::new(store, scheduler)
    }

    /// Summarise current daemon health. `mode` is stamped into the report
    /// purely for observability (`status` is a daemon-level operation, not
    /// a memory operation — see `MemoryMode`'s doc comment) — never gated.
    ///
    /// Strictly read-only: on a brand-new palace nothing has created the
    /// palace record yet, and looking must not be what creates it, so the
    /// name falls back to [`crate::domain::DEFAULT_PALACE_NAME`] until a write does.
    ///
    /// # Errors
    ///
    /// Never returns an error for an unhealthy datastore: that is the very
    /// thing being reported, so it comes back as `datastore.ok == false` with
    /// zeroed counts. An error here would make the daemon look absent (a 500)
    /// exactly when a caller most needs to know it is up but degraded. The
    /// `Result` is kept so a future fallible field does not change every
    /// caller.
    pub async fn status(&self, mode: MemoryMode) -> Result<StatusReport> {
        let mut report = StatusReport {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: (Utc::now() - self.started_at).num_seconds(),
            palace_name: crate::domain::DEFAULT_PALACE_NAME.to_string(),
            drawer_count: 0,
            jobs_queued: 0,
            jobs_running: 0,
            jobs_paused: 0,
            mode,
            pid: std::process::id(),
            started_at: self.started_at,
            bind_addr: self.runtime.bind_addr.clone(),
            palace_path: self.runtime.palace_path.clone(),
            datastore: DatastoreStatus {
                backend: self.runtime.backend.clone(),
                location: self.runtime.location.clone(),
                ..DatastoreStatus::default()
            },
            auth_enabled: self.auth_enabled(),
        };
        match self.read_status_details(&mut report).await {
            Ok(()) => report.datastore.ok = true,
            Err(error) => report.datastore.error = Some(error.to_string()),
        }
        Ok(report)
    }

    /// Fill `report` from the store: ping, migration state, name and counts.
    /// Split out so `status` has one place that turns any failure into a
    /// degraded report.
    async fn read_status_details(&self, report: &mut StatusReport) -> Result<()> {
        self.store.ping().await?;
        let migrations = crate::migrate::status(&self.store).await?;
        report.datastore.migration_version = migrations.current_version;
        report.datastore.latest_version = migrations.latest_version;
        report.datastore.pending = migrations.pending;
        if let Some(palace) = self.store.get_palace().await? {
            report.palace_name = palace.name;
        }
        report.drawer_count = self.store.count_drawers().await?;
        report.jobs_queued = self.store.count_jobs(Some(JobStatus::Queued)).await?;
        report.jobs_running = self.store.count_jobs(Some(JobStatus::Running)).await?;
        report.jobs_paused = self.store.count_jobs(Some(JobStatus::Paused)).await?;
        Ok(())
    }

    /// Reject a read (`search`/`recall`/`wake_up`/`diary_read`) that
    /// `mode` doesn't permit, before any store contact — the single place
    /// all read-gated methods check `MemoryMode` (see that type's doc
    /// comment for the matrix).
    fn require_read(mode: MemoryMode, operation: &'static str) -> Result<()> {
        if mode.allows_read() {
            Ok(())
        } else {
            Err(Error::ModeForbidden {
                operation: operation.to_string(),
                mode,
            })
        }
    }

    /// Reject a write (`checkpoint`/`emergency_checkpoint`/`diary_write`)
    /// that `mode` doesn't permit, before any store contact — the write
    /// counterpart of [`Self::require_read`].
    fn require_write(mode: MemoryMode, operation: &'static str) -> Result<()> {
        if mode.allows_write() {
            Ok(())
        } else {
            Err(Error::ModeForbidden {
                operation: operation.to_string(),
                mode,
            })
        }
    }

    /// Lexical search over drawer content, optionally scoped to one wing
    /// and/or room by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails, or [`Error::ModeForbidden`]
    /// if `mode` doesn't permit reads (`Disabled`).
    pub async fn search(
        &self,
        query: &str,
        wing: Option<&str>,
        room: Option<&str>,
        limit: u32,
        mode: MemoryMode,
    ) -> Result<Vec<SearchHit>> {
        self.gated_search("search", query, wing, room, limit, mode)
            .await
    }

    /// The gated search both `search` and `recall` run, taking the caller's
    /// own name so a rejected `recall` says `recall`, not the `search` it
    /// never asked for (the same reason `enqueue_checkpoint` takes one).
    async fn gated_search(
        &self,
        operation: &'static str,
        query: &str,
        wing: Option<&str>,
        room: Option<&str>,
        limit: u32,
        mode: MemoryMode,
    ) -> Result<Vec<SearchHit>> {
        Self::require_read(mode, operation)?;
        let limit = limit.min(MAX_READ_LIMIT);
        crate::search::lexical_search(&self.store, query, limit, wing, room).await
    }

    /// Submit a mining job for `path`, returning immediately with the
    /// job's id — the CLI/MCP/HTTP caller never runs the mine itself.
    ///
    /// Mining is background work: it always runs at [`Priority::Background`]
    /// so it never delays checkpoint/audit/repair jobs.
    ///
    /// Gated as a **write**: the job's whole purpose is to file drawers, so
    /// letting a `ReadOnly` session submit it would mutate the palace
    /// through the back door, and a `Disabled` one must not touch it at all.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted,
    /// [`Error::InvalidInput`] if `path` is relative, or
    /// [`Error::ModeForbidden`] if `mode` doesn't permit writes.
    pub async fn submit_mine(
        &self,
        path: PathBuf,
        wing: Option<String>,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Job> {
        Self::require_write(mode, "mine")?;
        // Validated here, like `submit_repair`'s `based_on_job`: a relative path
        // would be resolved against the *daemon's* working directory, not the
        // caller's, and mine the wrong tree or fail minutes later as a job.
        // `has_root` as well as `is_absolute` so `/data` is accepted on Windows,
        // where it has a root but no drive.
        if !(path.is_absolute() || path.has_root()) {
            return Err(Error::invalid_input(
                "path",
                format!(
                    "`{}` is relative; give an absolute path, since the daemon resolves it \
                     against its own working directory",
                    path.display()
                ),
            ));
        }
        // Up front, like the path: a bad wing name is a 400 at submission,
        // not a job that fails once it starts. A wing derived from the
        // directory name is left alone, since the caller did not choose it.
        if let Some(wing) = wing.as_deref() {
            self.check_new_wing_name(wing).await?;
        }
        self.scheduler
            .submit(
                JobKind::Mine {
                    source: MiningSource::Directory { path },
                    wing,
                },
                Priority::Background,
                requested_by,
            )
            .await
    }

    /// Submit a synthetic demo job (see `domain::job::JobKind::Demo`) at
    /// [`Priority::Normal`].
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted.
    pub async fn submit_demo(&self, steps: u32, requested_by: &str) -> Result<Job> {
        self.scheduler
            .submit(JobKind::Demo { steps }, Priority::Normal, requested_by)
            .await
    }

    /// Refuse a wing name that could not be created through `wing create`.
    ///
    /// An existing wing is always accepted, whatever its name: it can only
    /// have an odd one from before names were checked, and refusing it here
    /// would lock its owner out of writing to it. Only a name that would
    /// *create* a wing is validated, so a UUID-shaped or `/`-bearing name can
    /// never become a wing no path can address.
    async fn check_new_wing_name(&self, wing: &str) -> Result<()> {
        if self.store.get_wing(wing).await?.is_some() {
            return Ok(());
        }
        validate_name(NameKind::Wing, wing)
    }

    /// Shared submission path for both checkpoint priorities — `checkpoint`
    /// and `emergency_checkpoint` differ only in which `Priority` they
    /// pass, since there is exactly one `JobKind::Checkpoint` variant (see
    /// its doc comment). Also the single place that gates both public
    /// checkpoint methods on `mode`, so neither duplicates the check.
    async fn enqueue_checkpoint(
        &self,
        payload: CheckpointPayload,
        priority: Priority,
        requested_by: &str,
        operation: &'static str,
        mode: MemoryMode,
    ) -> Result<Job> {
        // The caller's own name, so a rejected emergency checkpoint says
        // `emergency_checkpoint`, not the `checkpoint` it never asked for.
        Self::require_write(mode, operation)?;
        // An empty payload would complete as a "successful" job that wrote
        // nothing, and the agent would believe it had stored a memory.
        if payload.items.is_empty() {
            return Err(Error::invalid_input(
                "payload",
                "`items` must contain at least one item",
            ));
        }
        // The same rule `create_drawer` applies: a blank drawer is never
        // recallable, so storing one silently loses what the caller meant.
        if let Some(index) = payload
            .items
            .iter()
            .position(|item| item.content.trim().is_empty())
        {
            return Err(Error::invalid_input(
                "payload",
                format!("`items[{index}].content` must not be empty"),
            ));
        }
        // The range the domain types document and nothing else enforced: an
        // out-of-range (or NaN) confidence would be stored as given and
        // skew whatever later ranks facts by it. `contains` is false for NaN.
        for (index, item) in payload.items.iter().enumerate() {
            let confidence = match &item.fact {
                Some(
                    FactMutation::Add { confidence, .. }
                    | FactMutation::Supersede { confidence, .. },
                ) => Some(*confidence),
                Some(FactMutation::Invalidate { .. }) | None => None,
            };
            if confidence.is_some_and(|value| !(0.0..=1.0).contains(&value)) {
                return Err(Error::invalid_input(
                    "payload",
                    format!("`items[{index}].fact.confidence` must be between 0 and 1"),
                ));
            }
        }
        // Up front, so a bad name is a 400 at submission and not a job that
        // fails halfway through after writing the items before it.
        for name in payload.items.iter().filter_map(|item| item.name.as_deref()) {
            validate_name(NameKind::Drawer, name)?;
        }
        // The same reason for item wings, which create a wing on first use.
        for wing in payload.items.iter().filter_map(|item| item.wing.as_deref()) {
            self.check_new_wing_name(wing).await?;
        }
        self.scheduler
            .submit(JobKind::Checkpoint { payload }, priority, requested_by)
            .await
    }

    /// Submit a checkpoint job at [`Priority::High`] — above background
    /// mining, below an emergency checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted, or
    /// [`Error::ModeForbidden`] if `mode` doesn't permit writes
    /// (`ReadOnly`/`Disabled`).
    pub async fn submit_checkpoint(
        &self,
        payload: CheckpointPayload,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Job> {
        self.enqueue_checkpoint(payload, Priority::High, requested_by, "checkpoint", mode)
            .await
    }

    /// Submit a checkpoint at [`Priority::Critical`] when `emergency` is set
    /// and [`Priority::High`] otherwise. The one place that choice is made, so
    /// REST and MCP (which both take an `emergency` flag) do not each branch
    /// on it and drift apart.
    ///
    /// # Errors
    ///
    /// As [`Self::submit_checkpoint`] and [`Self::submit_emergency_checkpoint`].
    pub async fn submit_checkpoint_with_urgency(
        &self,
        payload: CheckpointPayload,
        emergency: bool,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Job> {
        if emergency {
            self.submit_emergency_checkpoint(payload, requested_by, mode)
                .await
        } else {
            self.submit_checkpoint(payload, requested_by, mode).await
        }
    }

    /// Submit an emergency checkpoint at [`Priority::Critical`] — preempts
    /// every other queued job, for save-before-crash situations.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted, or
    /// [`Error::ModeForbidden`] if `mode` doesn't permit writes
    /// (`ReadOnly`/`Disabled`).
    pub async fn submit_emergency_checkpoint(
        &self,
        payload: CheckpointPayload,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Job> {
        self.enqueue_checkpoint(
            payload,
            Priority::Critical,
            requested_by,
            "emergency_checkpoint",
            mode,
        )
        .await
    }

    /// Submit a read-only palace consistency audit, optionally narrowing its
    /// embedding-count fields to one wing by name (see `crate::audit`'s
    /// module doc for exactly what it checks).
    ///
    /// Runs at [`Priority::Normal`] — above background mining, below a
    /// checkpoint's `High`/`Critical` (see `submit_mine`'s doc comment,
    /// which already anticipates this). **Not** gated by [`MemoryMode`]:
    /// like `submit_mine`/`submit_demo`, this is an administrative/
    /// daemon-level operation, not a session-scoped memory read — see
    /// `domain::MemoryMode`'s module doc, and ADR-002 for why leaving
    /// `Audit` ungated is provisional rather than a settled boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted.
    pub async fn submit_audit(&self, scope: Option<String>, requested_by: &str) -> Result<Job> {
        self.scheduler
            .submit(JobKind::Audit { scope }, Priority::Normal, requested_by)
            .await
    }

    /// Submit a repair job — see `crate::repair`'s module doc for exactly
    /// what this does (only orphan-drawer removal) and why a second action
    /// named in that issue was dropped as redundant with
    /// `jobs::Scheduler::recover`.
    ///
    /// Runs at [`Priority::Normal`], same as `submit_audit`. A **dry run** is
    /// not gated by [`MemoryMode`] — it only reports, exactly like audit.
    /// An applied repair (`dry_run == false`) deletes drawers, so it is
    /// gated as a **write**.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted,
    /// [`Error::ModeForbidden`] if this is an applied repair and `mode`
    /// doesn't permit writes, or [`Error::InvalidBasedOnJob`] if
    /// `based_on_job` is not a completed audit job (no job is created).
    pub async fn submit_repair(
        &self,
        dry_run: bool,
        based_on_job: Option<JobId>,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Job> {
        if !dry_run {
            Self::require_write(mode, "repair")?;
        }
        // Validated here, not only when the handler runs: otherwise a typo'd
        // id is accepted with a 200 and surfaces as a failed job minutes
        // later. The handler re-checks (the audit may vanish in between).
        if let Some(audit_id) = based_on_job {
            crate::repair::load_audited_orphan_ids(&self.store, audit_id).await?;
        }
        self.scheduler
            .submit(
                JobKind::Repair {
                    dry_run,
                    based_on_job,
                },
                Priority::Normal,
                requested_by,
            )
            .await
    }

    /// List jobs, optionally filtered to one status.
    ///
    /// Gated as a **read**: a job record carries its whole input, and for a
    /// checkpoint job that is the memory being written. Leaving this open
    /// would let a `Disabled` session read palace content through the job
    /// list, defeating the guarantee that nothing MemCastle-derived reaches
    /// it.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails, or
    /// [`Error::ModeForbidden`] if `mode` doesn't permit reads.
    pub async fn list_jobs(&self, status: Option<JobStatus>, mode: MemoryMode) -> Result<Vec<Job>> {
        Self::require_read(mode, "job_list")?;
        self.store.list_jobs(status).await
    }

    /// Fetch one job by id. Gated as a **read** for the same reason as
    /// [`Self::list_jobs`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::JobNotFound`] if no job has that id — every
    /// interface wants that same error, so it is raised once here instead of
    /// being rebuilt from an `Option` by each of them — an error if the store
    /// query fails, or [`Error::ModeForbidden`] if `mode` doesn't permit
    /// reads.
    pub async fn get_job(&self, id: JobId, mode: MemoryMode) -> Result<Job> {
        Self::require_read(mode, "job_get")?;
        self.store
            .get_job(id)
            .await?
            .ok_or_else(|| Error::JobNotFound { id: id.to_string() })
    }

    /// Request that a running job pause.
    ///
    /// Job control is deliberately not mode-gated: it needs a job id, which a
    /// session that cannot list jobs never learns (ADR-007).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::JobNotFound`] if the job doesn't exist, or
    /// [`crate::Error::InvalidJobTransition`] if it isn't running.
    pub async fn pause_job(&self, id: JobId) -> Result<JobControlResult> {
        self.scheduler.request_pause(id).await?;
        Ok(JobControlResult {
            status: JobControlStatus::PauseRequested,
        })
    }

    /// Resume a paused job.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't paused.
    pub async fn resume_job(&self, id: JobId) -> Result<JobControlResult> {
        self.scheduler.resume(id).await?;
        Ok(JobControlResult {
            status: JobControlStatus::Resumed,
        })
    }

    /// Cancel a queued, paused, or running job.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or can't be cancelled from
    /// its current status.
    pub async fn cancel_job(&self, id: JobId) -> Result<JobControlResult> {
        self.scheduler.request_cancel(id).await?;
        Ok(JobControlResult {
            status: JobControlStatus::CancelRequested,
        })
    }

    /// Retry a failed job.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't failed.
    pub async fn retry_job(&self, id: JobId) -> Result<JobControlResult> {
        self.scheduler.retry(id).await?;
        Ok(JobControlResult {
            status: JobControlStatus::Retried,
        })
    }

    /// Persist a diary entry for `agent_identity`, filed as a drawer under
    /// `wing`'s fixed `"diary"` room (the same convention
    /// `CheckpointDestination::Diary` already reserves — see
    /// `domain::checkpoint`). Diary writes are small and synchronous, not
    /// job-queued: MemCastle already has one daemon/one writer, so nothing
    /// forces this through the scheduler the way checkpoint/mining are
    /// (see issue #13 / `PLAN.md`'s design note).
    ///
    /// `requested_by` is the channel the write came through (`"cli"`,
    /// `"http"`, `"mcp"`), recorded as `provenance.requested_by`; the agent
    /// identity goes in `source.agent`. See [`crate::domain::Provenance`].
    ///
    /// # Errors
    ///
    /// Returns an error if the store write fails, [`Error::InvalidInput`] if
    /// `content` is blank, [`Error::InvalidPalacePath`]
    /// if `wing` would be a new wing with an unusable name, or
    /// [`Error::ModeForbidden`] if `mode` doesn't permit writes
    /// (`ReadOnly`/`Disabled`).
    pub async fn diary_write(
        &self,
        agent_identity: &str,
        wing: &str,
        content: String,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Drawer> {
        Self::require_write(mode, "diary_write")?;
        // The rule `create_drawer` and checkpoint already apply: a blank
        // drawer is never recallable, so storing one silently loses the entry
        // the agent believes it wrote.
        if content.trim().is_empty() {
            return Err(Error::invalid_input("content", "must not be empty"));
        }
        self.check_new_wing_name(wing).await?;
        let wing_record = self.store.get_or_create_wing(wing, None).await?;
        let room = self
            .store
            .get_or_create_room(
                wing_record.id,
                CheckpointDestination::Diary.room_name(),
                None,
            )
            .await?;

        let drawer = Drawer::new(
            DrawerId::new(),
            room.id,
            content,
            Source {
                kind: SourceKind::Manual,
                uri: None,
                // Who wrote it: the agent identity, as every writer records it.
                agent: Some(agent_identity.to_string()),
            },
            vec![],
            Provenance {
                // Through which channel it arrived (`cli`, `http`, `mcp`) —
                // the same meaning mining and checkpoint give this field.
                // Writing the identity here too is what made "who asked"
                // unqueryable across writers.
                requested_by: requested_by.to_string(),
                job_id: None,
            },
        );
        self.store.create_drawer(&drawer).await?;
        Ok(drawer)
    }

    /// Read back `agent_identity`'s most recent diary entries in `wing`,
    /// newest first. The identity string is the caller's responsibility to
    /// keep consistent across writes/reads — MemCastle just stores/
    /// retrieves by it faithfully (see issue #13).
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails, or [`Error::ModeForbidden`]
    /// if `mode` doesn't permit reads (`Disabled`).
    pub async fn diary_read(
        &self,
        agent_identity: &str,
        wing: &str,
        limit: u32,
        mode: MemoryMode,
    ) -> Result<Vec<Drawer>> {
        Self::require_read(mode, "diary_read")?;
        let limit = limit.min(MAX_READ_LIMIT);
        // Read-only lookups: a `ReadOnly` session reading a wing nobody has
        // written to must not create that wing and its diary room. No wing or
        // room simply means no entries.
        let Some(wing_record) = self.store.get_wing(wing).await? else {
            return Ok(Vec::new());
        };
        let Some(room) = self
            .store
            .get_room(wing_record.id, CheckpointDestination::Diary.room_name())
            .await?
        else {
            return Ok(Vec::new());
        };
        self.store
            .list_diary_drawers(room.id, agent_identity, limit)
            .await
    }

    /// Retrieve palace content matching `query`, scoped to `wing` if given.
    ///
    /// Distinct from [`Self::search`] only in *intent*: this is the
    /// recall-oriented primitive the task brief's vocabulary calls for
    /// (issue #14 / §14 — "MemCastle should expose excellent primitives for
    /// `recall(...)`/`search(...)`"), wired to exactly the same scoped
    /// `list_drawers_matching` underneath — a future divergence (e.g.
    /// recall-specific reranking) has a name to hang off, not a reason to
    /// duplicate logic today. MemCastle does not itself force a
    /// search-before-answer protocol; enforcing that discipline is an
    /// integration/skill's job, not this primitive's. `recall` never
    /// paraphrases or truncates: every `SearchHit::drawer.content` returned
    /// is exactly what was stored.
    ///
    /// Deliberately does not match on `mode` itself — it delegates entirely
    /// to `gated_search`, which is the one place that read gate lives (and
    /// which [`Self::search`] shares), so there is exactly one `MemoryMode`
    /// match to audit for this path, not two copies that could drift apart.
    /// The operation is named `recall` there, so a refusal says which tool was
    /// forbidden.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails, or [`Error::ModeForbidden`]
    /// if `mode` doesn't permit reads (`Disabled`).
    pub async fn recall(
        &self,
        query: &str,
        wing: Option<&str>,
        limit: u32,
        mode: MemoryMode,
    ) -> Result<Vec<SearchHit>> {
        self.gated_search("recall", query, wing, None, limit, mode)
            .await
    }

    /// Build an agent's session-start context: its most recent diary entry
    /// (when `wing` is given — see the note below) plus up to
    /// `budget.max_items` of the most recent checkpoint-originated drawers
    /// in `wing` scope, bounded by `budget.max_bytes` (see
    /// [`WakeUpBudget`]'s doc comment for exactly what that bounds).
    ///
    /// Deliberately a simple, deterministic V1 (task brief §13: "do not
    /// prematurely implement an elaborate token optimizer") — this is only
    /// L0/L1 of the task brief's layered retrieval model; project-specific
    /// (L2) and deeper (L3) retrieval are future work, not attempted here.
    ///
    /// `wing: None` skips the diary lookup entirely (`diary` comes back
    /// `None`): [`Self::diary_read`] always resolves a per-wing `"diary"`
    /// room, so there is no defined "diary across every wing" query to run
    /// — only `recent_highlights` supports an unscoped ("every wing")
    /// lookup.
    ///
    /// Gates on `mode` up front (before any store contact), then passes it
    /// straight through to the internal [`Self::diary_read`] call — that
    /// call re-checking the same `mode` is redundant but harmless, not a
    /// second copy of the policy (see `recall`'s doc comment for the same
    /// reasoning).
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails, or [`Error::ModeForbidden`]
    /// if `mode` doesn't permit reads (`Disabled`).
    pub async fn wake_up(
        &self,
        agent_identity: &str,
        wing: Option<&str>,
        budget: WakeUpBudget,
        mode: MemoryMode,
    ) -> Result<WakeUpContext> {
        Self::require_read(mode, "wake_up")?;
        let diary = match wing {
            Some(wing) => self
                .diary_read(agent_identity, wing, 1, mode)
                .await?
                .into_iter()
                .next(),
            None => None,
        };

        // Clamped like every other read limit: an `as u32` here wrapped a
        // huge `max_items` to a small number (2^32 to zero) and silently
        // returned nothing.
        let limit = u32::try_from(budget.max_items)
            .unwrap_or(u32::MAX)
            .min(MAX_READ_LIMIT);
        let candidates = self
            .store
            .list_checkpoint_originated_drawers(wing, limit)
            .await?;

        // Trim to the byte budget by whole drawers, never mid-content: a
        // drawer that would push the running total over `max_bytes` is left
        // out, not truncated (see `recall`'s verbatim guarantee, which this
        // must not undermine for `wake_up` either). Left out *and skipped
        // past*: stopping at the first one that does not fit would drop every
        // older highlight after it, however small, when the budget still has
        // room for them.
        let mut recent_highlights = Vec::new();
        let mut total_bytes = 0usize;
        for drawer in candidates {
            let next_total = total_bytes.saturating_add(drawer.content.len());
            if next_total > budget.max_bytes {
                continue;
            }
            total_bytes = next_total;
            recent_highlights.push(drawer);
        }

        Ok(WakeUpContext {
            diary,
            recent_highlights,
            generated_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_app() -> AppServices {
        test_app_with_store().await.0
    }

    /// Same as `test_app`, but also hands back the underlying `SurrealStore`
    /// — needed by tests that seed checkpoint-originated drawers directly
    /// through `checkpoint::run` (bypassing the scheduler entirely, the
    /// same shortcut `checkpoint::mod`'s own tests take) rather than
    /// through any `AppServices` method, since `AppServices` has no
    /// synchronous "run this checkpoint job right now" call.
    async fn test_app_with_store() -> (AppServices, SurrealStore) {
        let store = SurrealStore::connect_memory_for_tests().await;
        let scheduler = Arc::new(Scheduler::new(store.clone(), 1));
        (AppServices::new(store.clone(), scheduler), store)
    }

    /// Run a one-item checkpoint job directly against `store`, filing
    /// `content` under `wing`'s general-destination room. The resulting
    /// drawer's `provenance.job_id` points at a freshly saved
    /// `JobKind::Checkpoint` job, exactly like a real checkpoint job would
    /// leave behind — see `list_checkpoint_originated_drawers`'s doc
    /// comment for why that's what "checkpoint-originated" means.
    async fn seed_checkpoint_drawer(store: &SurrealStore, wing: &str, content: &str) {
        use crate::domain::{CheckpointDestination, CheckpointItem};
        use crate::jobs::{JobContext, JobControl};

        let payload = CheckpointPayload {
            items: vec![CheckpointItem {
                destination: CheckpointDestination::General,
                wing: Some(wing.to_string()),
                name: None,
                content: content.to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: Some("test-agent".to_string()),
                },
                fact: None,
            }],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());
        crate::checkpoint::run(
            &ctx,
            &mut job,
            crate::checkpoint::CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("checkpoint run");
    }

    #[tokio::test]
    async fn a_diary_entry_written_can_be_read_back_scoped_by_agent_and_wing() {
        let app = test_app().await;
        let written = app
            .diary_write(
                "agent-a",
                "project-x",
                "went well today".to_string(),
                "test",
                MemoryMode::Full,
            )
            .await
            .expect("write");

        let entries = app
            .diary_read("agent-a", "project-x", 10, MemoryMode::Full)
            .await
            .expect("read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, written.id);
        assert_eq!(entries[0].content, "went well today");
        assert_eq!(entries[0].source.agent.as_deref(), Some("agent-a"));
    }

    #[tokio::test]
    async fn two_agent_identities_in_the_same_wing_do_not_leak_into_each_others_diary_read() {
        let app = test_app().await;
        app.diary_write(
            "agent-a",
            "shared-wing",
            "a's entry".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("write a");
        app.diary_write(
            "agent-b",
            "shared-wing",
            "b's entry".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("write b");

        let a_entries = app
            .diary_read("agent-a", "shared-wing", 10, MemoryMode::Full)
            .await
            .expect("read a");
        assert_eq!(
            a_entries.len(),
            1,
            "agent-b's entry must not leak into agent-a's diary read, got {a_entries:?}"
        );
        assert_eq!(a_entries[0].content, "a's entry");

        let b_entries = app
            .diary_read("agent-b", "shared-wing", 10, MemoryMode::Full)
            .await
            .expect("read b");
        assert_eq!(
            b_entries.len(),
            1,
            "agent-a's entry must not leak into agent-b's diary read, got {b_entries:?}"
        );
        assert_eq!(b_entries[0].content, "b's entry");
    }

    #[tokio::test]
    async fn recall_returns_drawer_content_verbatim() {
        let app = test_app().await;
        app.diary_write(
            "agent-a",
            "project-x",
            "line one\nline two, verbatim — no paraphrasing".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("write");

        let hits = app
            .recall("verbatim", Some("project-x"), 10, MemoryMode::Full)
            .await
            .expect("recall");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].drawer.content,
            "line one\nline two, verbatim — no paraphrasing"
        );
    }

    #[tokio::test]
    async fn wake_up_never_exceeds_the_configured_item_and_byte_budget() {
        let (app, store) = test_app_with_store().await;
        // Five candidates, far more than the budget below allows either way.
        for i in 0..5 {
            seed_checkpoint_drawer(&store, "project-x", &format!("highlight number {i}")).await;
        }

        let budget = WakeUpBudget {
            max_items: 2,
            max_bytes: 30,
        };
        let context = app
            .wake_up("agent-a", Some("project-x"), budget, MemoryMode::Full)
            .await
            .expect("wake up");

        assert!(
            context.recent_highlights.len() <= budget.max_items,
            "got {} highlights, budget allows {}",
            context.recent_highlights.len(),
            budget.max_items
        );
        let total_bytes: usize = context
            .recent_highlights
            .iter()
            .map(|d| d.content.len())
            .sum();
        assert!(
            total_bytes <= budget.max_bytes,
            "highlights total {total_bytes} bytes, budget is {}",
            budget.max_bytes
        );
    }

    #[tokio::test]
    async fn wake_up_is_deterministic_given_a_fixed_database_state() {
        let (app, store) = test_app_with_store().await;
        app.diary_write(
            "agent-a",
            "project-x",
            "day one".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("diary write");
        seed_checkpoint_drawer(&store, "project-x", "an important highlight").await;

        let budget = WakeUpBudget::default();
        let first = app
            .wake_up("agent-a", Some("project-x"), budget, MemoryMode::Full)
            .await
            .expect("wake up 1");
        let second = app
            .wake_up("agent-a", Some("project-x"), budget, MemoryMode::Full)
            .await
            .expect("wake up 2");

        // Byte-identical modulo `generated_at`, per the issue's determinism
        // requirement — null that one field out on both sides before
        // comparing, rather than asserting on every other field by hand.
        let mut first_value = serde_json::to_value(&first).expect("serialize first");
        let mut second_value = serde_json::to_value(&second).expect("serialize second");
        first_value["generated_at"] = serde_json::Value::Null;
        second_value["generated_at"] = serde_json::Value::Null;
        assert_eq!(first_value, second_value);
    }

    #[tokio::test]
    async fn wake_up_skips_the_diary_when_no_wing_is_given() {
        let (app, store) = test_app_with_store().await;
        app.diary_write(
            "agent-a",
            "project-x",
            "should not appear".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("diary write");
        seed_checkpoint_drawer(&store, "project-x", "a highlight").await;

        let context = app
            .wake_up("agent-a", None, WakeUpBudget::default(), MemoryMode::Full)
            .await
            .expect("wake up");

        assert!(
            context.diary.is_none(),
            "wake_up(wing: None) must skip the diary lookup entirely, got {:?}",
            context.diary
        );
    }

    fn assert_mode_forbidden(result: &Result<impl std::fmt::Debug>, expected_mode: MemoryMode) {
        match result {
            Err(crate::Error::ModeForbidden { mode, .. }) => {
                assert_eq!(*mode, expected_mode);
            }
            other => panic!("expected Error::ModeForbidden, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rejected_recall_names_recall_not_the_search_it_delegates_to() {
        let app = test_app().await;

        match app.recall("anything", None, 5, MemoryMode::Disabled).await {
            Err(crate::Error::ModeForbidden { operation, .. }) => assert_eq!(operation, "recall"),
            other => panic!("expected Error::ModeForbidden, got {other:?}"),
        }
        match app
            .search("anything", None, None, 5, MemoryMode::Disabled)
            .await
        {
            Err(crate::Error::ModeForbidden { operation, .. }) => assert_eq!(operation, "search"),
            other => panic!("expected Error::ModeForbidden, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mining_a_relative_path_is_rejected_at_submission_not_as_a_failed_job() {
        let app = test_app().await;

        let result = app
            .submit_mine("some/relative/dir".into(), None, "test", MemoryMode::Full)
            .await;

        match result {
            Err(crate::Error::InvalidInput { field, .. }) => assert_eq!(field, "path"),
            other => panic!("expected Error::InvalidInput for `path`, got {other:?}"),
        }
        assert!(
            app.list_jobs(None, MemoryMode::Full)
                .await
                .expect("list")
                .is_empty(),
            "a rejected submission must not leave a job behind"
        );
    }

    #[tokio::test]
    async fn a_new_wing_with_an_unusable_name_is_refused_by_every_writer() {
        let app = test_app().await;
        let uuid_like = uuid::Uuid::new_v4().to_string();

        for bad in [uuid_like.as_str(), "has/slash"] {
            assert!(
                matches!(
                    app.diary_write("agent", bad, "entry".into(), "test", MemoryMode::Full)
                        .await,
                    Err(crate::Error::InvalidPalacePath { .. })
                ),
                "diary_write must refuse the wing {bad:?}"
            );
            assert!(
                matches!(
                    app.submit_mine(
                        "/tmp/anything".into(),
                        Some(bad.into()),
                        "test",
                        MemoryMode::Full
                    )
                    .await,
                    Err(crate::Error::InvalidPalacePath { .. })
                ),
                "submit_mine must refuse the wing {bad:?}"
            );
            let mut payload = one_item_payload("content");
            payload.items[0].wing = Some(bad.to_string());
            assert!(
                matches!(
                    app.submit_checkpoint(payload, "test", MemoryMode::Full)
                        .await,
                    Err(crate::Error::InvalidPalacePath { .. })
                ),
                "submit_checkpoint must refuse the wing {bad:?}"
            );
        }
        assert!(
            app.list_jobs(None, MemoryMode::Full)
                .await
                .expect("list")
                .is_empty(),
            "a refused submission must not leave a job behind"
        );
        assert!(
            app.list_wings(MemoryMode::Full)
                .await
                .expect("wings")
                .is_empty(),
            "a refused name must not create a wing"
        );
    }

    #[tokio::test]
    async fn a_fact_confidence_outside_zero_to_one_is_refused_at_submission() {
        let app = test_app().await;

        for confidence in [1.5_f32, -0.1, f32::NAN] {
            let mut payload = one_item_payload("content");
            payload.items[0].fact = Some(crate::domain::FactMutation::Add {
                subject: crate::domain::EntityId::new(),
                predicate: "likes".to_string(),
                object: crate::domain::EntityId::new(),
                confidence,
            });

            match app
                .submit_checkpoint(payload, "test", MemoryMode::Full)
                .await
            {
                Err(crate::Error::InvalidInput { field, .. }) => assert_eq!(field, "payload"),
                other => panic!("expected InvalidInput for confidence {confidence}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_blank_diary_entry_is_refused_and_writes_nothing() {
        let app = test_app().await;

        let result = app
            .diary_write(
                "agent",
                "project-x",
                "  \n".into(),
                "test",
                MemoryMode::Full,
            )
            .await;

        match result {
            Err(crate::Error::InvalidInput { field, .. }) => assert_eq!(field, "content"),
            other => panic!("expected Error::InvalidInput for `content`, got {other:?}"),
        }
        assert!(
            app.diary_read("agent", "project-x", 10, MemoryMode::Full)
                .await
                .expect("read")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_existing_wing_stays_writable_whatever_its_name() {
        let app = test_app().await;
        let odd = uuid::Uuid::new_v4().to_string();
        // Created straight through the store, as a pre-validation palace would have it.
        app.store.get_or_create_wing(&odd, None).await.unwrap();

        app.diary_write("agent", &odd, "entry".into(), "test", MemoryMode::Full)
            .await
            .expect("an existing wing is accepted");
    }

    #[test]
    fn a_wake_up_budget_falls_back_to_the_default_only_for_what_was_omitted() {
        let default = WakeUpBudget::default();

        let none = WakeUpBudget::from_options(None, None);
        assert_eq!(
            (none.max_items, none.max_bytes),
            (default.max_items, default.max_bytes)
        );

        let partial = WakeUpBudget::from_options(Some(3), None);
        assert_eq!(
            (partial.max_items, partial.max_bytes),
            (3, default.max_bytes)
        );

        let full = WakeUpBudget::from_options(Some(3), Some(100));
        assert_eq!((full.max_items, full.max_bytes), (3, 100));
    }

    #[tokio::test]
    async fn a_disabled_session_cannot_read_checkpointed_content_through_the_job_list() {
        let app = test_app().await;
        let submitted = app
            .submit_checkpoint(
                one_item_payload("a secret preference"),
                "test",
                MemoryMode::Full,
            )
            .await
            .expect("full-mode checkpoint");

        // A job record carries its whole payload, so listing or showing it
        // is a memory read: it must be refused, not merely redacted.
        assert_mode_forbidden(
            &app.list_jobs(None, MemoryMode::Disabled).await,
            MemoryMode::Disabled,
        );
        assert_mode_forbidden(
            &app.get_job(submitted.id, MemoryMode::Disabled).await,
            MemoryMode::Disabled,
        );
    }

    #[tokio::test]
    async fn a_read_only_session_can_still_list_and_show_jobs() {
        let app = test_app().await;
        let submitted = app
            .submit_checkpoint(one_item_payload("readable"), "test", MemoryMode::Full)
            .await
            .expect("full-mode checkpoint");

        // ReadOnly may already read the same content through search/recall,
        // so blocking job reads for it would protect nothing.
        assert_eq!(
            app.list_jobs(None, MemoryMode::ReadOnly)
                .await
                .expect("list")
                .len(),
            1
        );
        assert_eq!(
            app.get_job(submitted.id, MemoryMode::ReadOnly)
                .await
                .expect("get")
                .id,
            submitted.id
        );
    }

    #[tokio::test]
    async fn mining_is_a_write_so_read_only_and_disabled_sessions_cannot_submit_it() {
        let app = test_app().await;
        for mode in [MemoryMode::ReadOnly, MemoryMode::Disabled] {
            let result = app
                .submit_mine("/tmp/anything".into(), None, "test", mode)
                .await;
            assert_mode_forbidden(&result, mode);
        }
        assert!(
            app.list_jobs(None, MemoryMode::Full)
                .await
                .expect("list")
                .is_empty(),
            "a rejected mine must not leave a job behind"
        );
        app.submit_mine("/tmp/anything".into(), None, "test", MemoryMode::Full)
            .await
            .expect("full-mode mine is accepted");
    }

    #[tokio::test]
    async fn an_applied_repair_is_a_write_but_a_dry_run_is_only_a_report() {
        let app = test_app().await;

        assert_mode_forbidden(
            &app.submit_repair(false, None, "test", MemoryMode::ReadOnly)
                .await,
            MemoryMode::ReadOnly,
        );
        app.submit_repair(true, None, "test", MemoryMode::ReadOnly)
            .await
            .expect("a read-only session may ask what a repair would do");
        app.submit_repair(false, None, "test", MemoryMode::Full)
            .await
            .expect("a full-mode session may apply a repair");
    }

    #[tokio::test]
    async fn a_disabled_search_is_rejected_without_a_store_query() {
        let app = test_app().await;
        let result = app
            .search("anything", None, None, 10, MemoryMode::Disabled)
            .await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn recall_in_disabled_mode_is_rejected_the_same_as_search() {
        let app = test_app().await;
        let result = app.recall("anything", None, 10, MemoryMode::Disabled).await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn wake_up_in_disabled_mode_is_rejected_without_reading_the_diary_or_highlights() {
        let app = test_app().await;
        let result = app
            .wake_up(
                "agent-a",
                Some("project-x"),
                WakeUpBudget::default(),
                MemoryMode::Disabled,
            )
            .await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn a_disabled_diary_read_is_rejected() {
        let app = test_app().await;
        let result = app
            .diary_read("agent-a", "project-x", 10, MemoryMode::Disabled)
            .await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn a_disabled_diary_write_is_rejected_and_nothing_is_persisted() {
        let app = test_app().await;
        let result = app
            .diary_write(
                "agent-a",
                "project-x",
                "should never be written".to_string(),
                "test",
                MemoryMode::Disabled,
            )
            .await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);

        // Confirm via a `Full`-mode read that nothing was persisted —
        // the store must never have been touched, not just that the
        // caller received an error.
        let entries = app
            .diary_read("agent-a", "project-x", 10, MemoryMode::Full)
            .await
            .expect("read");
        assert!(
            entries.is_empty(),
            "a disabled diary_write must not persist anything, found {entries:?}"
        );
    }

    #[tokio::test]
    async fn a_disabled_checkpoint_submission_is_rejected() {
        let app = test_app().await;
        let payload = CheckpointPayload {
            items: vec![crate::domain::CheckpointItem {
                destination: crate::domain::CheckpointDestination::General,
                wing: Some("project-x".to_string()),
                name: None,
                content: "should never be queued".to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: Some("test-agent".to_string()),
                },
                fact: None,
            }],
        };
        let result = app
            .submit_checkpoint(payload, "test", MemoryMode::Disabled)
            .await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn a_read_only_diary_write_is_rejected_but_diary_read_still_works() {
        let app = test_app().await;
        app.diary_write(
            "agent-a",
            "project-x",
            "written while full".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("full-mode write");

        let write_result = app
            .diary_write(
                "agent-a",
                "project-x",
                "should be rejected".to_string(),
                "test",
                MemoryMode::ReadOnly,
            )
            .await;
        assert_mode_forbidden(&write_result, MemoryMode::ReadOnly);

        let entries = app
            .diary_read("agent-a", "project-x", 10, MemoryMode::ReadOnly)
            .await
            .expect("read-only diary_read must still succeed");
        assert_eq!(
            entries.len(),
            1,
            "the rejected write must not have been persisted, found {entries:?}"
        );
    }

    #[tokio::test]
    async fn a_read_only_checkpoint_submission_is_rejected() {
        let app = test_app().await;
        let payload = CheckpointPayload {
            items: vec![crate::domain::CheckpointItem {
                destination: crate::domain::CheckpointDestination::General,
                wing: Some("project-x".to_string()),
                name: None,
                content: "should never be queued".to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: Some("test-agent".to_string()),
                },
                fact: None,
            }],
        };
        let result = app
            .submit_checkpoint(payload, "test", MemoryMode::ReadOnly)
            .await;
        assert_mode_forbidden(&result, MemoryMode::ReadOnly);
    }

    #[tokio::test]
    async fn a_full_mode_checkpoint_still_succeeds() {
        let app = test_app().await;
        let payload = CheckpointPayload {
            items: vec![crate::domain::CheckpointItem {
                destination: crate::domain::CheckpointDestination::General,
                wing: Some("project-x".to_string()),
                name: None,
                content: "a full-mode checkpoint".to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: Some("test-agent".to_string()),
                },
                fact: None,
            }],
        };
        app.submit_checkpoint(payload, "test", MemoryMode::Full)
            .await
            .expect("full-mode checkpoint must be accepted");
    }

    #[tokio::test]
    async fn an_emergency_checkpoint_is_rejected_in_disabled_mode() {
        let app = test_app().await;
        let payload = CheckpointPayload {
            items: vec![crate::domain::CheckpointItem {
                destination: crate::domain::CheckpointDestination::General,
                wing: Some("project-x".to_string()),
                name: None,
                content: "should never be queued".to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: Some("test-agent".to_string()),
                },
                fact: None,
            }],
        };
        let result = app
            .submit_emergency_checkpoint(payload, "test", MemoryMode::Disabled)
            .await;
        assert_mode_forbidden(&result, MemoryMode::Disabled);
    }

    fn one_item_payload(content: &str) -> CheckpointPayload {
        CheckpointPayload {
            items: vec![crate::domain::CheckpointItem {
                destination: crate::domain::CheckpointDestination::General,
                wing: Some("project-x".to_string()),
                name: None,
                content: content.to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: Some("test-agent".to_string()),
                },
                fact: None,
            }],
        }
    }

    #[tokio::test]
    async fn a_checkpoint_without_items_is_rejected_and_queues_nothing() {
        let app = test_app().await;
        let result = app
            .submit_checkpoint(
                CheckpointPayload { items: vec![] },
                "test",
                MemoryMode::Full,
            )
            .await;
        assert!(
            matches!(&result, Err(Error::InvalidInput { field, .. }) if field == "payload"),
            "an empty payload must be refused, got {result:?}"
        );
        assert!(
            app.list_jobs(None, MemoryMode::Full)
                .await
                .expect("list")
                .is_empty(),
            "a refused checkpoint must not leave a job behind"
        );
    }

    #[tokio::test]
    async fn a_checkpoint_with_blank_content_is_rejected() {
        let app = test_app().await;
        let result = app
            .submit_checkpoint(one_item_payload("  \n "), "test", MemoryMode::Full)
            .await;
        assert!(
            matches!(&result, Err(Error::InvalidInput { field, .. }) if field == "payload"),
            "blank content must be refused, got {result:?}"
        );
    }

    #[tokio::test]
    async fn recall_finds_a_stored_fact_from_a_natural_language_question() {
        let app = test_app().await;
        app.diary_write(
            "agent-a",
            "project-x",
            "my main programming languages are Rust and Python".to_string(),
            "test",
            MemoryMode::Full,
        )
        .await
        .expect("write");

        let hits = app
            .recall(
                "programming languages I use preferences",
                None,
                10,
                MemoryMode::Full,
            )
            .await
            .expect("recall");
        assert_eq!(
            hits.len(),
            1,
            "extra words in the question must not hide the stored fact, got {hits:?}"
        );
    }

    #[tokio::test]
    async fn an_exact_multi_word_match_does_not_pull_in_partial_matches() {
        let app = test_app().await;
        for content in [
            "my main programming languages are Rust and Python",
            "the garden needs languages of water",
        ] {
            app.diary_write(
                "agent-a",
                "project-x",
                content.to_string(),
                "test",
                MemoryMode::Full,
            )
            .await
            .expect("write");
        }

        let hits = app
            .recall("programming languages", None, 10, MemoryMode::Full)
            .await
            .expect("recall");
        assert_eq!(
            hits.len(),
            1,
            "a query every term of which matches must stay strict, got {hits:?}"
        );
    }

    #[tokio::test]
    async fn a_read_only_emergency_checkpoint_is_rejected_and_queues_nothing() {
        let app = test_app().await;
        let result = app
            .submit_emergency_checkpoint(
                one_item_payload("never queued"),
                "test",
                MemoryMode::ReadOnly,
            )
            .await;
        assert_mode_forbidden(&result, MemoryMode::ReadOnly);
        assert!(
            app.list_jobs(None, MemoryMode::Full)
                .await
                .expect("list")
                .is_empty(),
            "a rejected emergency checkpoint must not leave a job behind"
        );
    }

    #[tokio::test]
    async fn a_full_mode_emergency_checkpoint_is_queued_at_critical_priority() {
        let app = test_app().await;
        let job = app
            .submit_emergency_checkpoint(one_item_payload("save me"), "test", MemoryMode::Full)
            .await
            .expect("full-mode emergency checkpoint must be accepted");
        assert_eq!(job.priority, Priority::Critical);
    }

    #[tokio::test]
    async fn a_routine_checkpoint_is_queued_below_critical_priority() {
        let app = test_app().await;
        let job = app
            .submit_checkpoint(one_item_payload("routine"), "test", MemoryMode::Full)
            .await
            .expect("full-mode checkpoint must be accepted");
        assert_eq!(job.priority, Priority::High);
    }

    #[tokio::test]
    async fn status_reports_the_mode_it_was_given_without_being_gated_by_it() {
        let app = test_app().await;
        let report = app
            .status(MemoryMode::Disabled)
            .await
            .expect("status must never be gated by mode");
        assert_eq!(report.mode, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn status_reports_a_migrated_reachable_datastore_as_healthy() {
        let (app, store) = test_app_with_store().await;
        crate::migrate::run(&store).await.expect("migrate");

        let report = app.status(MemoryMode::Full).await.expect("status");

        assert!(report.datastore.ok, "{:?}", report.datastore);
        assert!(report.datastore.is_healthy());
        assert!(report.datastore.pending.is_empty());
        assert_eq!(
            report.datastore.migration_version,
            report.datastore.latest_version
        );
        assert!(report.datastore.error.is_none());
        assert_eq!(report.pid, std::process::id());
    }

    #[tokio::test]
    async fn status_flags_pending_migrations_as_unhealthy_without_failing_the_request() {
        // The test store has its schema but never ran the data migrations, so
        // it stands in for a shared palace a newer daemon has not migrated yet.
        let app = test_app().await;

        let report = app
            .status(MemoryMode::Full)
            .await
            .expect("an unhealthy datastore is reported, not raised");

        assert!(report.datastore.ok, "the datastore itself answered");
        assert!(!report.datastore.pending.is_empty());
        assert!(!report.datastore.is_healthy());
    }

    #[tokio::test]
    async fn status_reports_the_runtime_context_the_daemon_was_started_with() {
        let app = test_app().await.with_runtime(RuntimeContext {
            bind_addr: "127.0.0.1:9999".into(),
            palace_path: "/palace".into(),
            backend: "embedded".into(),
            location: "/palace/db".into(),
        });

        let report = app.status(MemoryMode::Full).await.expect("status");

        assert_eq!(report.bind_addr, "127.0.0.1:9999");
        assert_eq!(report.palace_path, "/palace");
        assert_eq!(report.datastore.backend, "embedded");
        assert_eq!(report.datastore.location, "/palace/db");
    }

    #[test]
    fn a_report_from_an_older_daemon_still_deserializes() {
        // A newer CLI must not fail on a daemon that predates the new fields.
        let old = serde_json::json!({
            "version": "0.0.9", "uptime_secs": 3, "palace_name": "p",
            "drawer_count": 1, "jobs_queued": 0, "jobs_running": 0,
            "jobs_paused": 0, "mode": "full"
        });
        let report: StatusReport = serde_json::from_value(old).expect("old shape");
        assert_eq!(report.pid, 0);
        assert!(!report.datastore.ok);
    }

    #[tokio::test]
    async fn a_rejected_emergency_checkpoint_names_the_operation_the_caller_asked_for() {
        let app = test_app().await;
        let result = app
            .submit_emergency_checkpoint(one_item_payload("x"), "test", MemoryMode::ReadOnly)
            .await;
        assert!(
            matches!(&result, Err(Error::ModeForbidden { operation, .. }) if operation == "emergency_checkpoint"),
            "got {result:?}"
        );
        let result = app
            .submit_checkpoint(one_item_payload("x"), "test", MemoryMode::ReadOnly)
            .await;
        assert!(
            matches!(&result, Err(Error::ModeForbidden { operation, .. }) if operation == "checkpoint"),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn an_absurd_limit_is_clamped_to_the_maximum_instead_of_being_honoured() {
        let app = test_app().await;
        let extra = 5;
        for i in 0..(MAX_READ_LIMIT + extra) {
            app.diary_write(
                "agent-a",
                "wing",
                format!("entry {i}"),
                "test",
                MemoryMode::Full,
            )
            .await
            .expect("diary write");
        }
        let entries = app
            .diary_read("agent-a", "wing", u32::MAX, MemoryMode::Full)
            .await
            .expect("diary read");
        assert_eq!(entries.len(), MAX_READ_LIMIT as usize);
    }

    #[tokio::test]
    async fn getting_an_unknown_job_is_a_typed_not_found_error() {
        let app = test_app().await;
        let result = app.get_job(JobId::new(), MemoryMode::Full).await;
        assert!(
            matches!(result, Err(Error::JobNotFound { .. })),
            "got {result:?}"
        );
    }

    /// What "byte-for-byte unchanged" means for the records these reads used
    /// to create as a side effect.
    async fn structure_of(store: &SurrealStore) -> (bool, usize, usize) {
        let wings = store.list_wings().await.unwrap();
        let mut rooms = 0;
        for wing in &wings {
            rooms += store.list_rooms(wing.id).await.unwrap().len();
        }
        (
            store.get_palace().await.unwrap().is_some(),
            wings.len(),
            rooms,
        )
    }

    #[tokio::test]
    async fn read_only_reads_on_an_empty_palace_leave_the_store_unchanged() {
        let (app, store) = test_app_with_store().await;
        let before = structure_of(&store).await;
        assert_eq!(before, (false, 0, 0));

        for mode in [MemoryMode::ReadOnly, MemoryMode::Full] {
            let diary = app
                .diary_read("agent-a", "unknown-wing", 5, mode)
                .await
                .expect("diary read");
            assert!(diary.is_empty());
            let context = app
                .wake_up(
                    "agent-a",
                    Some("unknown-wing"),
                    WakeUpBudget::default(),
                    mode,
                )
                .await
                .expect("wake up");
            assert!(context.diary.is_none() && context.recent_highlights.is_empty());
            let status = app.status(mode).await.expect("status");
            assert_eq!(status.palace_name, crate::domain::DEFAULT_PALACE_NAME);
        }

        assert_eq!(
            structure_of(&store).await,
            before,
            "a read must not create the palace, wing or room it looked for"
        );
    }

    #[tokio::test]
    async fn reading_a_wing_that_exists_but_has_no_diary_room_creates_no_room() {
        let (app, store) = test_app_with_store().await;
        store.get_or_create_wing("project-x", None).await.unwrap();
        let before = structure_of(&store).await;

        let diary = app
            .diary_read("agent-a", "project-x", 5, MemoryMode::ReadOnly)
            .await
            .expect("diary read");

        assert!(diary.is_empty());
        assert_eq!(structure_of(&store).await, before);
    }

    #[tokio::test]
    async fn status_counts_jobs_by_status_without_fetching_them() {
        let app = test_app().await;
        app.submit_demo(1, "test").await.expect("submit");
        app.submit_demo(1, "test").await.expect("submit");

        let status = app.status(MemoryMode::Full).await.expect("status");

        // The test scheduler is never started, so both stay queued.
        assert_eq!(status.jobs_queued, 2);
        assert_eq!(status.jobs_running, 0);
    }

    #[tokio::test]
    async fn a_diary_entry_records_the_channel_as_requested_by_and_the_agent_as_source() {
        let app = test_app().await;

        let drawer = app
            .diary_write(
                "agent-a",
                "wing",
                "note".to_string(),
                "mcp",
                MemoryMode::Full,
            )
            .await
            .expect("diary write");

        assert_eq!(drawer.provenance.requested_by, "mcp");
        assert_eq!(drawer.source.agent.as_deref(), Some("agent-a"));
    }

    #[tokio::test]
    async fn an_oversize_highlight_is_skipped_and_the_smaller_older_ones_after_it_are_kept() {
        let (app, store) = test_app_with_store().await;
        // Oldest to newest, so newest-first reads: small-new, LARGE, small-old.
        seed_checkpoint_drawer(&store, "project-x", "small-old").await;
        seed_checkpoint_drawer(&store, "project-x", &"L".repeat(200)).await;
        seed_checkpoint_drawer(&store, "project-x", "small-new").await;

        let context = app
            .wake_up(
                "agent-a",
                Some("project-x"),
                WakeUpBudget {
                    max_items: 10,
                    max_bytes: 40,
                },
                MemoryMode::Full,
            )
            .await
            .expect("wake up");

        let contents: Vec<&str> = context
            .recent_highlights
            .iter()
            .map(|d| d.content.as_str())
            .collect();
        assert_eq!(
            contents,
            ["small-new", "small-old"],
            "the big one is left out, not a reason to drop everything older"
        );
    }

    #[tokio::test]
    async fn a_huge_max_items_is_clamped_instead_of_wrapping_to_nothing() {
        let (app, store) = test_app_with_store().await;
        seed_checkpoint_drawer(&store, "project-x", "a highlight").await;

        // 2^32 truncated to zero under an `as u32` cast, and returned nothing.
        let context = app
            .wake_up(
                "agent-a",
                Some("project-x"),
                WakeUpBudget {
                    max_items: 1 << 32,
                    max_bytes: 8192,
                },
                MemoryMode::Full,
            )
            .await
            .expect("wake up");

        assert_eq!(context.recent_highlights.len(), 1);
    }
}
