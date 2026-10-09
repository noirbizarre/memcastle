//! Job queue repository methods.
//!
//! Every write of a whole job goes through `write_job`, so there is exactly
//! one place that decides how a `Job` maps to SurrealQL, rather than a
//! separate insert/update pair that could drift apart. It is unguarded for
//! submission (`save_job`) and guarded for anything that must not clobber
//! another worker: a claim (still `Queued`), a leased worker's writes (still
//! the owner) and a reap (lease unchanged) — see `docs/adr/006-job-leases.md`.
//! Status transitions themselves are decided by
//! [`crate::domain::Job::apply`], never by SurrealQL — this module only
//! persists whatever the domain layer already decided.

use crate::domain::{Job, JobEvent, JobId, JobStatus};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The column list every job read projects. See `drawers::DRAWER_COLUMNS`
/// for why datetimes are cast to strings and `id` through `record::id()`.
const JOB_COLUMNS: &str = "record::id(id) AS id, kind, status, priority, \
     <string>created_at AS created_at, started_at, completed_at, requested_by, progress, \
     attempt, recovery_attempts ?? 0 AS recovery_attempts, max_attempts, checkpoint, result, error, lease_owner, lease_expires_at, \
     pause_requested ?? false AS pause_requested, cancel_requested ?? false AS cancel_requested";

/// The SQL that writes every job column, shared by every write path so there
/// is exactly one place that decides how a `Job` maps to SurrealQL.
///
/// `pause_requested`/`cancel_requested` are kept, not overwritten, while the
/// job stays `Running`: the caller's copy predates any request the API made
/// since, and writing its `false` would silently drop that request. Likewise
/// `lease_expires_at` while the same owner keeps the job: the heartbeat
/// renews it independently, and writing back the copy from claim time would
/// undo every renewal at the next checkpoint. (It must be assigned *before*
/// `lease_owner`, since the comparison reads the stored owner.)
const JOB_SET: &str = "kind = $kind, status = $status, priority = $priority, \
     created_at = <datetime>$created_at, started_at = $started_at, \
     completed_at = $completed_at, requested_by = $requested_by, \
     progress = $progress, attempt = $attempt, \
     recovery_attempts = $recovery_attempts, max_attempts = $max_attempts, \
     checkpoint = $checkpoint, result = $result, error = $error, \
     lease_expires_at = IF $status = 'running' AND lease_owner = $lease_owner \
         { lease_expires_at } ELSE { $lease_expires_at }, \
     lease_owner = $lease_owner, \
     pause_requested = IF $status = 'running' { pause_requested ?? false } ELSE { false }, \
     cancel_requested = IF $status = 'running' { cancel_requested ?? false } ELSE { false }";

/// A condition a job write must satisfy against the *stored* record, making
/// the check and the write one atomic statement.
///
/// This is what turns "one daemon per palace" from an assumption into
/// something a second daemon cannot violate: a write that would clobber a
/// record another worker now owns matches nothing and reports it.
enum Guard {
    /// The job must still be in this status (a claim: `Queued`).
    Status(JobStatus),
    /// The job must still be leased to this worker (a fenced write).
    Owner(String),
    /// A force-cancel may only displace the exact running owner it observed.
    RunningOwner(String),
    /// The job must still carry exactly the lease that was read: the same
    /// owner and expiry, so a lease renewed since is not reaped from under
    /// its owner.
    Lease {
        owner: Option<String>,
        expires_at: Option<String>,
    },
}

impl SurrealStore {
    /// Insert a new job, or overwrite an existing one at the same id.
    ///
    /// `UPSERT ... SET` targeting a specific id creates the record if it is
    /// missing and updates it otherwise, so a caller can save after every
    /// transition without branching on "is this the first save?". Unfenced:
    /// use it for submission and for a store nobody else writes; a worker
    /// holding a lease writes with [`Self::save_job_fenced`] instead.
    pub async fn save_job(&self, job: &Job) -> Result<()> {
        super::retrying_on_conflict(|| self.write_job(job, None)).await?;
        Ok(())
    }

    /// Save `job` only if the stored record is still in status `expected`.
    /// Returns `false`, writing nothing, if it has moved on: a worker claimed
    /// it (or a request already changed it) between the caller's read and this
    /// write, so the caller's copy is stale and saving it would clobber the
    /// newer state — for example writing `Cancelled` over a job that is now
    /// running on a worker.
    pub async fn save_job_if_status(&self, job: &Job, expected: JobStatus) -> Result<bool> {
        let guard = Guard::Status(expected);
        super::retrying_on_conflict(|| self.write_job(job, Some(&guard))).await
    }

    /// Save `job` only if the stored record is still leased to `owner`.
    /// Returns `false`, writing nothing, if it is not — another daemon
    /// reaped this job after the lease lapsed, and this worker's view of it
    /// is now stale.
    pub async fn save_job_fenced(&self, job: &Job, owner: &str) -> Result<bool> {
        let guard = Guard::Owner(owner.to_string());
        super::retrying_on_conflict(|| self.write_job(job, Some(&guard))).await
    }

    /// Commit a forced terminal state only while this worker still owns a running job.
    pub async fn save_job_if_running_owner(&self, job: &Job, owner: &str) -> Result<bool> {
        let guard = Guard::RunningOwner(owner.to_string());
        super::retrying_on_conflict(|| self.write_job(job, Some(&guard))).await
    }

    /// Save `job` (typically a reaped, re-queued copy of `seen`) only if the
    /// stored record still carries the lease `seen` had. Returns `false`,
    /// writing nothing, if it changed: the owner renewed, or someone else
    /// already reaped it.
    pub async fn save_job_if_lease_unchanged(&self, job: &Job, seen: &Job) -> Result<bool> {
        let guard = Guard::Lease {
            owner: seen.lease_owner.clone(),
            expires_at: seen.lease_expires_at.map(super::stored),
        };
        super::retrying_on_conflict(|| self.write_job(job, Some(&guard))).await
    }

    /// Run the job write under an optional guard, returning whether a record
    /// was written. An unguarded write always reports `true`.
    async fn write_job(&self, job: &Job, guard: Option<&Guard>) -> Result<bool> {
        let (verb, condition) = match guard {
            None => ("UPSERT", ""),
            Some(Guard::Status(_)) => ("UPDATE", " WHERE status = $guard_status"),
            Some(Guard::Owner(_)) => ("UPDATE", " WHERE lease_owner = $guard_owner"),
            Some(Guard::RunningOwner(_)) => (
                "UPDATE",
                " WHERE status = 'running' AND lease_owner = $guard_owner",
            ),
            // `= NONE`/`= NULL` differ (see `list_jobs`), and a legacy job
            // has no lease at all: `??` folds both into one comparable value.
            Some(Guard::Lease { .. }) => (
                "UPDATE",
                " WHERE (lease_owner ?? '') = $guard_owner AND (lease_expires_at ?? '') = $guard_expires_at",
            ),
        };
        let query = self
            .db
            .query(format!(
                "{verb} type::record('job', $id) SET {JOB_SET}{condition} RETURN record::id(id) AS id"
            ))
            .bind(("id", job.id.to_string()))
            .bind(("kind", super::bindable(&job.kind)?))
            .bind(("status", super::bindable(&job.status)?))
            .bind(("priority", i32::from(job.priority)))
            .bind(("created_at", super::stored(job.created_at)))
            .bind(("started_at", job.started_at.map(super::stored)))
            .bind(("completed_at", job.completed_at.map(super::stored)))
            .bind(("requested_by", job.requested_by.clone()))
            .bind(("progress", super::bindable(&job.progress)?))
            .bind(("attempt", job.attempt))
            .bind(("recovery_attempts", job.recovery_attempts))
            .bind(("max_attempts", job.max_attempts))
            .bind(("checkpoint", job.checkpoint.clone()))
            .bind(("result", job.result.clone()))
            .bind(("error", job.error.clone()))
            .bind(("lease_owner", job.lease_owner.clone()))
            .bind((
                "lease_expires_at",
                job.lease_expires_at.map(super::stored),
            ));
        let query = match guard {
            None => query,
            Some(Guard::Status(status)) => query.bind(("guard_status", super::bindable(status)?)),
            Some(Guard::Owner(owner) | Guard::RunningOwner(owner)) => {
                query.bind(("guard_owner", owner.clone()))
            }
            Some(Guard::Lease { owner, expires_at }) => query
                .bind(("guard_owner", owner.clone().unwrap_or_default()))
                .bind(("guard_expires_at", expires_at.clone().unwrap_or_default())),
        };
        // `.await` alone only reports transport failures, not a rejected
        // statement — see `store::mod`'s module doc.
        let mut response = query.await?.check()?;
        let rows: Vec<serde_json::Value> = response.take(0)?;
        Ok(guard.is_none() || !rows.is_empty())
    }

    /// Extend the lease on `id` to `expires_at`, if it is still `Running` and
    /// leased to `owner`. Returns `false` if not — the lease was lost (the
    /// job was reaped and possibly re-claimed elsewhere), so the caller must
    /// stop working on it.
    pub async fn renew_job_lease(
        &self,
        id: JobId,
        owner: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        super::retrying_on_conflict(|| async {
            let mut response = self
                .db
                .query(
                    "UPDATE type::record('job', $id) SET lease_expires_at = $expires_at \
                     WHERE status = 'running' AND lease_owner = $owner \
                     RETURN record::id(id) AS id",
                )
                .bind(("id", id.to_string()))
                .bind(("expires_at", super::stored(expires_at)))
                .bind(("owner", owner.to_string()))
                .await?
                .check()?;
            let rows: Vec<serde_json::Value> = response.take(0)?;
            Ok(!rows.is_empty())
        })
        .await
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

    /// Record a force request only for the run owned by this daemon.
    pub async fn mark_cancel_requested_by(&self, id: JobId, owner: &str) -> Result<bool> {
        super::retrying_on_conflict(|| async {
            let mut response = self
                .db
                .query(
                    "UPDATE type::record('job', $id) SET cancel_requested = true \
                 WHERE status = 'running' AND lease_owner = $owner RETURN record::id(id) AS id",
                )
                .bind(("id", id.to_string()))
                .bind(("owner", owner.to_string()))
                .await?
                .check()?;
            let rows: Vec<serde_json::Value> = response.take(0)?;
            Ok(!rows.is_empty())
        })
        .await
    }

    async fn mark_stop_requested(&self, id: JobId, field: &'static str) -> Result<bool> {
        super::retrying_on_conflict(|| self.mark_stop_requested_once(id, field)).await
    }

    async fn mark_stop_requested_once(&self, id: JobId, field: &'static str) -> Result<bool> {
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

    /// List jobs narrowed to a status and to a kind (the `type` tag of [`crate::domain::JobKind`]), newest first,
    /// at most `limit` of them. The dashboard's history table: a bounded page rather than every job there ever was.
    pub async fn list_jobs_page(
        &self,
        status: Option<JobStatus>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Job>> {
        let sql = format!(
            // `= NULL` for an absent filter: see `list_jobs`. `LIMIT` is bound, never formatted into the text.
            "SELECT {JOB_COLUMNS} FROM job \
             WHERE ($status = NULL OR status = $status) AND ($kind = NULL OR kind.type = $kind) \
             ORDER BY created_at DESC LIMIT $limit"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("status", super::bindable(&status)?))
            .bind(("kind", super::bindable(&kind)?))
            .bind(("limit", i64::from(limit)))
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

    /// Claim the highest-priority, oldest queued job for `worker`, leasing it
    /// for `lease_ttl`, if there is one and this worker wins it.
    ///
    /// The claim is a `SELECT` followed by a write guarded on the job still
    /// being `Queued`, so two daemons racing for the same job cannot both
    /// win: the loser's write matches nothing and this returns `None`
    /// (the next poll tries again). That guard, not a database-level lock,
    /// is what makes the claim safe when more than one daemon shares a
    /// queue; see `docs/adr/006-job-leases.md`.
    pub async fn claim_next_job(
        &self,
        worker: &str,
        lease_ttl: chrono::Duration,
    ) -> Result<Option<Job>> {
        self.claim_next_job_eligible(worker, lease_ttl, true).await
    }

    /// When background slots are full, skip expensive job kinds *before* claiming:
    /// a claimed job waiting on an in-memory permit would need a lease heartbeat
    /// and could block higher-priority work for no useful reason.
    pub async fn claim_next_job_eligible(
        &self,
        worker: &str,
        lease_ttl: chrono::Duration,
        include_background: bool,
    ) -> Result<Option<Job>> {
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM job WHERE status = 'queued' \
             AND ($include_background = true OR (kind.type != 'mine' \
                  AND kind.type != 'embed' AND kind.type != 'extract')) \
             ORDER BY priority DESC, created_at ASC LIMIT 1"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("include_background", include_background))
            .await?;
        let candidates: Vec<Job> = super::take_rows(&mut response, 0)?;
        let Some(mut job) = candidates.into_iter().next() else {
            return Ok(None);
        };

        job.apply(JobEvent::Claim).map_err(Error::from)?;
        job.attempt += 1;
        job.lease_owner = Some(worker.to_string());
        job.lease_expires_at = Some(chrono::Utc::now() + lease_ttl);
        let guard = Guard::Status(JobStatus::Queued);
        let won = super::retrying_on_conflict(|| self.write_job(&job, Some(&guard))).await?;
        Ok(won.then_some(job))
    }
}
