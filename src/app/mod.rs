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
use sha2::{Digest, Sha256};

use crate::domain::{
    CheckpointPayload, Drawer, DrawerId, Job, JobId, JobKind, JobStatus, Priority, Provenance,
    Source, SourceKind,
};
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

    /// Persist a diary entry for `agent_identity`, filed as a drawer under
    /// `wing`'s fixed `"diary"` room (the same convention
    /// `CheckpointDestination::Diary` already reserves — see
    /// `domain::checkpoint`). Diary writes are small and synchronous, not
    /// job-queued: MemCastle already has one daemon/one writer, so nothing
    /// forces this through the scheduler the way checkpoint/mining are
    /// (see issue #13 / `PLAN.md`'s design note).
    ///
    /// # Errors
    ///
    /// Returns an error if the store write fails.
    pub async fn diary_write(
        &self,
        agent_identity: &str,
        wing: &str,
        content: String,
    ) -> Result<Drawer> {
        let wing_record = self.store.get_or_create_wing(wing, None).await?;
        let room = self
            .store
            .get_or_create_room(wing_record.id, "diary", None)
            .await?;

        let now = Utc::now();
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let content_hash = hex_encode(&hasher.finalize());

        let drawer = Drawer {
            id: DrawerId::new(),
            room: room.id,
            content,
            content_hash,
            source: Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: Some(agent_identity.to_string()),
            },
            tags: vec![],
            embedding: None,
            provenance: Provenance {
                requested_by: agent_identity.to_string(),
                job_id: None,
            },
            valid_from: now,
            valid_to: None,
            created_at: now,
            updated_at: now,
        };
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
    /// Returns an error if the store query fails.
    pub async fn diary_read(
        &self,
        agent_identity: &str,
        wing: &str,
        limit: u32,
    ) -> Result<Vec<Drawer>> {
        let wing_record = self.store.get_or_create_wing(wing, None).await?;
        let room = self
            .store
            .get_or_create_room(wing_record.id, "diary", None)
            .await?;
        self.store
            .list_diary_drawers(room.id, agent_identity, limit)
            .await
    }
}

/// Lowercase hex — see `mining`/`checkpoint`'s identical helper for why this
/// is a few lines of its own rather than a shared dependency.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_app() -> AppServices {
        let store = SurrealStore::connect_memory_for_tests().await;
        let scheduler = Arc::new(Scheduler::new(store.clone(), 1));
        AppServices::new(store, scheduler)
    }

    #[tokio::test]
    async fn a_diary_entry_written_can_be_read_back_scoped_by_agent_and_wing() {
        let app = test_app().await;
        let written = app
            .diary_write("agent-a", "project-x", "went well today".to_string())
            .await
            .expect("write");

        let entries = app
            .diary_read("agent-a", "project-x", 10)
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
        app.diary_write("agent-a", "shared-wing", "a's entry".to_string())
            .await
            .expect("write a");
        app.diary_write("agent-b", "shared-wing", "b's entry".to_string())
            .await
            .expect("write b");

        let a_entries = app
            .diary_read("agent-a", "shared-wing", 10)
            .await
            .expect("read a");
        assert_eq!(
            a_entries.len(),
            1,
            "agent-b's entry must not leak into agent-a's diary read, got {a_entries:?}"
        );
        assert_eq!(a_entries[0].content, "a's entry");

        let b_entries = app
            .diary_read("agent-b", "shared-wing", 10)
            .await
            .expect("read b");
        assert_eq!(
            b_entries.len(),
            1,
            "agent-a's entry must not leak into agent-b's diary read, got {b_entries:?}"
        );
        assert_eq!(b_entries[0].content, "b's entry");
    }
}
