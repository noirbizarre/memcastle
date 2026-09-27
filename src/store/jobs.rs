//! Job queue repository methods.
//!
//! `save_job` is the only write path, for every state a job passes through
//! (submit, claim, checkpoint, terminate) — it always `UPSERT`s the full
//! record, so there is exactly one place that decides how a `Job` maps to
//! SurrealQL, rather than a separate `insert`/`update` pair that could drift
//! apart. Status transitions themselves are decided by
//! [`crate::domain::Job::apply`], never by SurrealQL — this module only
//! persists whatever the domain layer already decided.

use crate::domain::{Job, JobEvent, JobId, JobStatus};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The column list every job read projects. See `drawers::DRAWER_COLUMNS`
/// for why datetimes are cast to strings and `id` through `record::id()`.
const JOB_COLUMNS: &str = "record::id(id) AS id, kind, status, priority, \
     <string>created_at AS created_at, started_at, completed_at, requested_by, progress, \
     attempt, max_attempts, checkpoint, error, lease_owner, lease_expires_at";

impl SurrealStore {
    /// Insert a new job, or overwrite an existing one at the same id.
    ///
    /// `UPSERT ... SET` targeting a specific id creates the record if it is
    /// missing and updates it otherwise, so the scheduler can call this
    /// after every transition without branching on "is this the first
    /// save?".
    pub async fn save_job(&self, job: &Job) -> Result<()> {
        self.db
            .query(
                "UPSERT type::record('job', $id) SET \
                 kind = $kind, status = $status, priority = $priority, \
                 created_at = <datetime>$created_at, started_at = $started_at, \
                 completed_at = $completed_at, requested_by = $requested_by, \
                 progress = $progress, attempt = $attempt, max_attempts = $max_attempts, \
                 checkpoint = $checkpoint, error = $error, lease_owner = $lease_owner, \
                 lease_expires_at = $lease_expires_at",
            )
            .bind(("id", job.id.to_string()))
            .bind(("kind", super::bindable(&job.kind)?))
            .bind(("status", super::bindable(&job.status)?))
            .bind(("priority", i32::from(job.priority)))
            .bind(("created_at", job.created_at.to_rfc3339()))
            .bind(("started_at", job.started_at.map(|dt| dt.to_rfc3339())))
            .bind(("completed_at", job.completed_at.map(|dt| dt.to_rfc3339())))
            .bind(("requested_by", job.requested_by.clone()))
            .bind(("progress", super::bindable(&job.progress)?))
            .bind(("attempt", job.attempt))
            .bind(("max_attempts", job.max_attempts))
            .bind(("checkpoint", job.checkpoint.clone()))
            .bind(("error", job.error.clone()))
            .bind(("lease_owner", job.lease_owner.clone()))
            .bind((
                "lease_expires_at",
                job.lease_expires_at.map(|dt| dt.to_rfc3339()),
            ))
            .await?
            // `.await` alone only reports transport failures, not a
            // rejected statement — see `store::mod`'s module doc.
            .check()?;
        Ok(())
    }

    /// Fetch one job by id.
    pub async fn get_job(&self, id: JobId) -> Result<Option<Job>> {
        let sql = format!("SELECT {JOB_COLUMNS} FROM job WHERE id = type::record('job', $id)");
        let mut response = self.db.query(sql).bind(("id", id.to_string())).await?;
        let mut jobs: Vec<Job> = super::take_rows(&mut response, 0)?;
        Ok(jobs.pop())
    }

    /// List jobs, optionally filtered to one status, newest first.
    pub async fn list_jobs(&self, status: Option<JobStatus>) -> Result<Vec<Job>> {
        let sql = format!(
            // `NULL`, not `NONE`: binding `Option::None` through
            // `serde_json::Value` (see `bindable`) produces SurrealDB's
            // `NULL` (a real value), not its `NONE` (absence) sentinel —
            // they're distinct in SurrealQL, and `$status = NONE` never
            // matches a bound `NULL`, silently returning nothing.
            "SELECT {JOB_COLUMNS} FROM job WHERE $status = NULL OR status = $status \
             ORDER BY created_at DESC"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("status", super::bindable(&status)?))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// Every job left `Running` from a previous, uncleanly stopped daemon —
    /// crash-recovery's starting point. See `jobs::scheduler::recover`.
    pub async fn list_running_jobs(&self) -> Result<Vec<Job>> {
        self.list_jobs(Some(JobStatus::Running)).await
    }

    /// Atomically claim the highest-priority, oldest queued job, if any.
    ///
    /// "Atomically" here means "through the single sequential dispatcher
    /// loop, not through a database-level lock" — see the architecture
    /// doc's note on why that is sufficient for one daemon owning one
    /// queue.
    pub async fn claim_next_job(&self, worker: &str) -> Result<Option<Job>> {
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM job WHERE status = 'queued' \
             ORDER BY priority DESC, created_at ASC LIMIT 1"
        );
        let mut response = self.db.query(sql).await?;
        let candidates: Vec<Job> = super::take_rows(&mut response, 0)?;
        let Some(mut job) = candidates.into_iter().next() else {
            return Ok(None);
        };

        job.apply(JobEvent::Claim).map_err(Error::from)?;
        job.attempt += 1;
        job.lease_owner = Some(worker.to_string());
        self.save_job(&job).await?;
        Ok(Some(job))
    }
}
