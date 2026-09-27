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

/// Bounds `AppServices::wake_up`'s output — deterministic and testable, no
/// LLM-based summarization (task brief §13: this is L0/L1 only). Both
/// limits apply to `recent_highlights` only: `max_items` caps how many
/// drawers are fetched at all (pushed into the store query's `LIMIT`);
/// `max_bytes` then caps their cumulative content length, dropping whole
/// drawers (never truncating one mid-string) once the running total would
/// exceed it — see `AppServices::wake_up`'s doc comment. The single `diary`
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

    /// Retrieve palace content matching `query`, scoped to `wing` if given.
    ///
    /// Distinct from [`Self::search`] only in *intent*: this is the
    /// recall-oriented primitive the task brief's vocabulary calls for
    /// (issue #14 / §14 — "MemCastle should expose excellent primitives for
    /// `recall(...)`/`search(...)`"), wired to exactly the same scoped
    /// `lexical_search` underneath — a future divergence (e.g.
    /// recall-specific reranking) has a name to hang off, not a reason to
    /// duplicate logic today. MemCastle does not itself force a
    /// search-before-answer protocol; enforcing that discipline is an
    /// integration/skill's job, not this primitive's. `recall` never
    /// paraphrases or truncates: every `SearchHit::drawer.content` returned
    /// is exactly what was stored.
    ///
    /// # Errors
    ///
    /// Returns an error if the store query fails.
    pub async fn recall(
        &self,
        query: &str,
        wing: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        self.search(query, limit, wing, None).await
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
    /// # Errors
    ///
    /// Returns an error if the store query fails.
    pub async fn wake_up(
        &self,
        agent_identity: &str,
        wing: Option<&str>,
        budget: WakeUpBudget,
    ) -> Result<WakeUpContext> {
        let diary = match wing {
            Some(wing) => self
                .diary_read(agent_identity, wing, 1)
                .await?
                .into_iter()
                .next(),
            None => None,
        };

        let candidates = self
            .store
            .list_checkpoint_originated_drawers(wing, budget.max_items as u32)
            .await?;

        // Trim to the byte budget by whole drawers, never mid-content: a
        // drawer that would push the running total over `max_bytes` is
        // simply left out, not truncated (see `recall`'s verbatim
        // guarantee, which this must not undermine for `wake_up` either).
        let mut recent_highlights = Vec::new();
        let mut total_bytes = 0usize;
        for drawer in candidates {
            let next_total = total_bytes + drawer.content.len();
            if next_total > budget.max_bytes {
                break;
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

/// Lowercase hex — see `mining`/`checkpoint`'s identical helper for why this
/// is a few lines of its own rather than a shared dependency.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
        crate::checkpoint::run(store, &ctx, &mut job, &payload)
            .await
            .expect("checkpoint run");
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

    #[tokio::test]
    async fn recall_returns_drawer_content_verbatim() {
        let app = test_app().await;
        app.diary_write(
            "agent-a",
            "project-x",
            "line one\nline two, verbatim — no paraphrasing".to_string(),
        )
        .await
        .expect("write");

        let hits = app
            .recall("verbatim", Some("project-x"), 10)
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
            .wake_up("agent-a", Some("project-x"), budget)
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
        app.diary_write("agent-a", "project-x", "day one".to_string())
            .await
            .expect("diary write");
        seed_checkpoint_drawer(&store, "project-x", "an important highlight").await;

        let budget = WakeUpBudget::default();
        let first = app
            .wake_up("agent-a", Some("project-x"), budget)
            .await
            .expect("wake up 1");
        let second = app
            .wake_up("agent-a", Some("project-x"), budget)
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
        app.diary_write("agent-a", "project-x", "should not appear".to_string())
            .await
            .expect("diary write");
        seed_checkpoint_drawer(&store, "project-x", "a highlight").await;

        let context = app
            .wake_up("agent-a", None, WakeUpBudget::default())
            .await
            .expect("wake up");

        assert!(
            context.diary.is_none(),
            "wake_up(wing: None) must skip the diary lookup entirely, got {:?}",
            context.diary
        );
    }
}
