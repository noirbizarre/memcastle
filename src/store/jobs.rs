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
     attempt, max_attempts, checkpoint, result, error, lease_owner, lease_expires_at, \
     pause_requested ?? false AS pause_requested, cancel_requested ?? false AS cancel_requested";

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
                 checkpoint = $checkpoint, result = $result, error = $error, \
                 lease_owner = $lease_owner, lease_expires_at = $lease_expires_at, \
                 pause_requested = IF $status = 'running' { pause_requested ?? false } ELSE { false }, \
                 cancel_requested = IF $status = 'running' { cancel_requested ?? false } ELSE { false }",
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
            .bind(("result", job.result.clone()))
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

    /// Record, on a job that is still `Running`, that the user asked for it
    /// to pause — in the store, so the request survives a crash that lands
    /// before the handler honours it (see [`Job::pause_requested`]).
    ///
    /// Returns whether a running job was marked: `false` means it is not (or
    /// no longer) `Running`, e.g. it finished a moment ago, and the caller
    /// should treat the request as an ordinary transition instead.
    pub async fn mark_pause_requested(&self, id: JobId) -> Result<bool> {
        self.mark_stop_requested(id, "pause_requested").await
    }

    /// The cancel counterpart of [`Self::mark_pause_requested`].
    pub async fn mark_cancel_requested(&self, id: JobId) -> Result<bool> {
        self.mark_stop_requested(id, "cancel_requested").await
    }

    async fn mark_stop_requested(&self, id: JobId, field: &'static str) -> Result<bool> {
        // `WHERE status = 'running'` makes the check and the write one
        // statement: a job that finished in between is left untouched instead
        // of being tagged with a request nothing will ever clear. `field` is
        // one of two literals above, never caller input, so formatting it in
        // is not an injection path.
        let mut response = self
            .db
            .query(format!(
                "UPDATE type::record('job', $id) SET {field} = true \
                 WHERE status = 'running' RETURN record::id(id) AS id"
            ))
            .bind(("id", id.to_string()))
            .await?;
        let rows: Vec<serde_json::Value> = response.take(0)?;
        Ok(!rows.is_empty())
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

    /// How many jobs there are, optionally only those in one status — a
    /// `count()` in the database rather than fetching every row (each of which
    /// carries its whole input and checkpoint) just to take `.len()`.
    pub async fn count_jobs(&self, status: Option<JobStatus>) -> Result<u64> {
        #[derive(serde::Deserialize)]
        struct Count {
            count: u64,
        }
        let mut response = self
            .db
            .query(
                // `= NULL`, not `NONE`: see `list_jobs`.
                "SELECT count() AS count FROM job WHERE $status = NULL OR status = $status \
                 GROUP ALL",
            )
            .bind(("status", super::bindable(&status)?))
            .await?;
        let counts: Vec<Count> = super::take_rows(&mut response, 0)?;
        // `GROUP ALL` over zero rows yields no row at all, not a zero one.
        Ok(counts.into_iter().next().map_or(0, |c| c.count))
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
