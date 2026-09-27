//! Application services: the one layer the CLI (for `serve`), the HTTP API,
//! and the MCP tools all call into.
//!
//! This is the layer the architecture constraint is actually about: nothing
//! under `api`/`mcp`/`cli` may reach past this into `store` or `jobs`
//! directly (enforced by a `prek` grep hook — see `AGENTS.md`), so a web
//! dashboard or a new transport can only ever do what this struct exposes.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{CheckpointPayload, Job, JobId, JobKind, JobStatus, Priority};
use crate::error::Result;
use crate::jobs::Scheduler;
use crate::store::{SearchHit, SurrealStore};

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
}

impl AppServices {
    /// Wrap a connected store and running scheduler.
    #[must_use]
    pub fn new(store: SurrealStore, scheduler: Arc<Scheduler>) -> Self {
        Self {
            store,
            scheduler,
            started_at: Utc::now(),
        }
    }

    /// Summarise current daemon health.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be read.
    pub async fn status(&self) -> Result<StatusReport> {
        let palace = self.store.ensure_palace("default").await?;
        let drawer_count = self.store.count_drawers().await?;
        let queued = self.store.list_jobs(Some(JobStatus::Queued)).await?.len() as u64;
        let running = self.store.list_jobs(Some(JobStatus::Running)).await?.len() as u64;
        let paused = self.store.list_jobs(Some(JobStatus::Paused)).await?.len() as u64;
        Ok(StatusReport {
            version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: (Utc::now() - self.started_at).num_seconds(),
            palace_name: palace.name,
            drawer_count,
            jobs_queued: queued,
            jobs_running: running,
            jobs_paused: paused,
        })
    }

    /// Lexical search over drawer content, optionally scoped to one wing
    /// and/or room by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails.
    pub async fn search(
        &self,
        query: &str,
        limit: u32,
        wing: Option<&str>,
        room: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        crate::search::lexical_search(&self.store, query, limit, wing, room).await
    }

    /// Submit a mining job for `path`, returning immediately with the
    /// job's id — the CLI/MCP/HTTP caller never runs the mine itself.
    ///
    /// Mining is background work: it always runs at [`Priority::Background`]
    /// so it never delays checkpoint/audit/repair jobs.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted.
    pub async fn submit_mine(
        &self,
        path: PathBuf,
        wing: Option<String>,
        requested_by: impl Into<String>,
    ) -> Result<Job> {
        self.scheduler
            .submit(
                JobKind::Mine { path, wing },
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
    pub async fn submit_demo(&self, steps: u32, requested_by: impl Into<String>) -> Result<Job> {
        self.scheduler
            .submit(JobKind::Demo { steps }, Priority::Normal, requested_by)
            .await
    }

    /// Shared submission path for both checkpoint priorities — `checkpoint`
    /// and `emergency_checkpoint` differ only in which `Priority` they
    /// pass, since there is exactly one `JobKind::Checkpoint` variant (see
    /// its doc comment).
    async fn submit_checkpoint(
        &self,
        payload: CheckpointPayload,
        priority: Priority,
        requested_by: impl Into<String>,
    ) -> Result<Job> {
        self.scheduler
            .submit(JobKind::Checkpoint { payload }, priority, requested_by)
            .await
    }

    /// Submit a checkpoint job at [`Priority::High`] — above background
    /// mining, below an emergency checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted.
    pub async fn checkpoint(
        &self,
        payload: CheckpointPayload,
        requested_by: impl Into<String>,
    ) -> Result<Job> {
        self.submit_checkpoint(payload, Priority::High, requested_by)
            .await
    }

    /// Submit an emergency checkpoint at [`Priority::Critical`] — preempts
    /// every other queued job, for save-before-crash situations.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted.
    pub async fn emergency_checkpoint(
        &self,
        payload: CheckpointPayload,
        requested_by: impl Into<String>,
    ) -> Result<Job> {
        self.submit_checkpoint(payload, Priority::Critical, requested_by)
            .await
    }

    /// List jobs, optionally filtered to one status.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails.
    pub async fn list_jobs(&self, status: Option<JobStatus>) -> Result<Vec<Job>> {
        self.store.list_jobs(status).await
    }

    /// Fetch one job by id.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails.
    pub async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        self.store.get_job(id).await
    }

    /// Request that a running job pause.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::JobNotFound`] if the job isn't running.
    pub fn pause_job(&self, id: JobId) -> Result<()> {
        self.scheduler.request_pause(id)
    }

    /// Resume a paused job.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't paused.
    pub async fn resume_job(&self, id: JobId) -> Result<()> {
        self.scheduler.resume(id).await
    }

    /// Cancel a queued, paused, or running job.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or can't be cancelled from
    /// its current status.
    pub async fn cancel_job(&self, id: JobId) -> Result<()> {
        self.scheduler.request_cancel(id).await
    }

    /// Retry a failed job.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't failed.
    pub async fn retry_job(&self, id: JobId) -> Result<()> {
        self.scheduler.retry(id).await
    }
}
