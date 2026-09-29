//! The job scheduler: the daemon's execution mechanism over the durable
//! queue `store::jobs` persists.
//!
//! The invariant this module exists to uphold: **queue state is durable,
//! the in-memory scheduler is only the execution mechanism.** Every status
//! change goes through [`crate::domain::Job::apply`] and is persisted before
//! this module considers it real; a crash loses at most the in-flight
//! `JobControl` handles — and with them any pause or cancel request not yet
//! honoured — never the job records themselves. [`Scheduler::recover`]
//! re-queues whatever was `Running`, and the job resumes from its checkpoint.

mod control;
mod demo;

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Job, JobEvent, JobId, JobKind, Priority};
use crate::error::Result;
use crate::store::SurrealStore;

pub use control::{JobContext, JobControl};

/// How the current job invocation ended — distinct from `Result<()>`
/// because pausing and cancelling are expected, non-error outcomes a
/// handler reports deliberately, not failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobOutcome {
    /// The handler finished all its work.
    Completed,
    /// The handler cooperatively paused; `job.checkpoint` holds where to
    /// resume from.
    Paused,
    /// The handler cooperatively stopped in response to a cancellation
    /// request.
    Cancelled,
}

/// How often the dispatch loop polls for a queued job. A fixed interval
/// rather than a `LIVE SELECT` on the `job` table — simpler, and at this
/// scale (a handful of concurrent agents) the latency cost is invisible
/// next to the job's own execution time.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// A unique-enough label for this scheduler instance, recorded as
/// `lease_owner` on jobs it claims. Not load-bearing for correctness (see
/// `docs/architecture.md` on why single-dispatcher claiming needs no
/// distributed lock) — it exists so a job record is self-describing when inspected.
fn worker_id() -> String {
    format!("memcastle-{}", std::process::id())
}

/// Owns the in-process side of the job queue: claiming, dispatching to
/// bounded concurrent workers, and the cooperative pause/cancel signalling
/// that lets a handler check in on its own terms rather than being killed.
pub struct Scheduler {
    store: SurrealStore,
    controls: Arc<DashMap<JobId, JobControl>>,
    semaphore: Arc<Semaphore>,
    worker: String,
}

impl Scheduler {
    /// Build a scheduler over `store`, allowing at most `max_concurrency`
    /// jobs to execute at once.
    #[must_use]
    pub fn new(store: SurrealStore, max_concurrency: usize) -> Self {
        Self {
            store,
            controls: Arc::new(DashMap::new()),
            semaphore: Arc::new(Semaphore::new(max_concurrency.max(1))),
            worker: worker_id(),
        }
    }

    /// Recover jobs left `Running` by a daemon that stopped uncleanly.
    ///
    /// Each is re-queued (to be picked up and resumed from its last
    /// checkpoint) if its attempt budget allows, or marked `Failed`
    /// otherwise — a job is never silently forgotten.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be read from or written to.
    pub async fn recover(&self) -> Result<()> {
        let stuck = self.store.list_running_jobs().await?;
        for mut job in stuck {
            if job.attempt < job.max_attempts {
                info!(job_id = %job.id, attempt = job.attempt, "recovering interrupted job to queued");
                job.apply(JobEvent::RecoverToQueued)?;
            } else {
                warn!(job_id = %job.id, attempt = job.attempt, "interrupted job exhausted its attempt budget");
                job.error = Some("exhausted attempt budget after an unclean restart".to_string());
                job.apply(JobEvent::Fail)?;
            }
            self.store.save_job(&job).await?;
        }
        Ok(())
    }

    /// Submit a new job and persist it as `Queued`.
    ///
    /// # Errors
    ///
    /// Returns an error if the job cannot be persisted.
    pub async fn submit(
        &self,
        kind: JobKind,
        priority: Priority,
        requested_by: impl Into<String>,
    ) -> Result<Job> {
        let job = Job::new(kind, priority, requested_by);
        self.store.save_job(&job).await?;
        Ok(job)
    }

    /// Request that a running job pause at its next checkpoint. A no-op
    /// error if the job isn't currently running — pausing a job that
    /// hasn't started, or has already finished, isn't a legal transition
    /// (see `domain::job`'s transition table).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::JobNotFound`] if the job isn't running.
    pub fn request_pause(&self, id: JobId) -> Result<()> {
        match self.controls.get(&id) {
            Some(control) => {
                control.request_pause();
                Ok(())
            }
            None => Err(crate::Error::JobNotFound { id: id.to_string() }),
        }
    }

    /// Request that a job stop. For a running job this is cooperative
    /// (signals the handler; it finishes its current unit of work first).
    /// For a queued or paused job, cancellation applies immediately since
    /// there is no handler to cooperate with.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or the transition is
    /// illegal from its current status.
    pub async fn request_cancel(&self, id: JobId) -> Result<()> {
        if let Some(control) = self.controls.get(&id) {
            control.request_cancel();
            return Ok(());
        }
        let mut job = self
            .store
            .get_job(id)
            .await?
            .ok_or(crate::Error::JobNotFound { id: id.to_string() })?;
        job.apply(JobEvent::Cancel)?;
        self.store.save_job(&job).await?;
        Ok(())
    }

    /// Move a `Paused` job back to `Queued` so the dispatch loop picks it
    /// up again.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't `Paused`.
    pub async fn resume(&self, id: JobId) -> Result<()> {
        let mut job = self
            .store
            .get_job(id)
            .await?
            .ok_or(crate::Error::JobNotFound { id: id.to_string() })?;
        job.apply(JobEvent::Resume)?;
        self.store.save_job(&job).await?;
        Ok(())
    }

    /// Reset a `Failed` job back to `Queued` for another attempt, clearing
    /// its error but keeping its checkpoint (a mining job, for instance,
    /// should resume past the files it already wrote, not redo them).
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't `Failed`.
    pub async fn retry(&self, id: JobId) -> Result<()> {
        let mut job = self
            .store
            .get_job(id)
            .await?
            .ok_or(crate::Error::JobNotFound { id: id.to_string() })?;
        job.apply(JobEvent::Retry)?;
        self.store.save_job(&job).await?;
        Ok(())
    }

    /// Run the dispatch loop until `shutdown` fires. Claims at most one job
    /// per tick and spawns it onto a bounded worker; polling (rather than a
    /// live query) keeps this loop's own logic trivial to read and test.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => {
                    info!("scheduler shutting down; in-flight jobs get their current unit of work to finish");
                    break;
                }
                _ = ticker.tick() => {
                    self.clone().try_dispatch_one().await;
                }
            }
        }
    }

    async fn try_dispatch_one(self: Arc<Self>) {
        let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() else {
            return; // at capacity; try again next tick
        };
        let claimed = self.store.claim_next_job(&self.worker).await;
        match claimed {
            Ok(Some(job)) => {
                let scheduler = Arc::clone(&self);
                tokio::spawn(async move {
                    let _permit = permit; // held for the job's whole execution
                    scheduler.execute(job).await;
                });
            }
            Ok(None) => {} // nothing queued; permit is dropped, released
            Err(error) => {
                warn!(%error, "failed to claim next job");
            }
        }
    }

    async fn execute(&self, mut job: Job) {
        let control = JobControl::default();
        self.controls.insert(job.id, control.clone());
        let ctx = JobContext::new(job.id, control, self.store.clone());

        let outcome = match job.kind.clone() {
            JobKind::Demo { steps } => demo::run(&self.store, &ctx, &mut job, steps).await,
            JobKind::Mine { source, wing } => {
                crate::mining::run(&self.store, &ctx, &mut job, &source, wing.as_deref()).await
            }
            JobKind::Checkpoint { payload } => {
                crate::checkpoint::run(&self.store, &ctx, &mut job, &payload).await
            }
            JobKind::Audit { scope } => {
                crate::audit::run(&self.store, &ctx, &mut job, scope.as_deref()).await
            }
            JobKind::Repair {
                dry_run,
                based_on_job,
            } => crate::repair::run(&self.store, &ctx, &mut job, dry_run, based_on_job).await,
        };

        self.controls.remove(&job.id);

        let event = match outcome {
            Ok(JobOutcome::Completed) => JobEvent::Complete,
            Ok(JobOutcome::Paused) => JobEvent::Pause,
            Ok(JobOutcome::Cancelled) => JobEvent::Cancel,
            Err(error) => {
                warn!(job_id = %job.id, %error, "job failed");
                job.error = Some(error.to_string());
                JobEvent::Fail
            }
        };
        if let Err(error) = job.apply(event) {
            warn!(job_id = %job.id, %error, "job finished in a status its outcome couldn't transition from");
        }
        if let Err(error) = self.store.save_job(&job).await {
            warn!(job_id = %job.id, %error, "failed to persist final job state");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::JobStatus;

    async fn scheduler() -> Scheduler {
        Scheduler::new(SurrealStore::connect_memory_for_tests().await, 1)
    }

    /// Persist a demo job driven into `status` purely through
    /// `Job::apply` — the tests seed state the same way production reaches
    /// it, so a seeded record can never be one the state machine forbids.
    async fn seed(scheduler: &Scheduler, status: JobStatus, attempt: u32) -> Job {
        let mut job = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        match status {
            JobStatus::Queued => {}
            JobStatus::Running => job.apply(JobEvent::Claim).unwrap(),
            JobStatus::Paused => {
                job.apply(JobEvent::Claim).unwrap();
                job.apply(JobEvent::Pause).unwrap();
            }
            JobStatus::Completed => {
                job.apply(JobEvent::Claim).unwrap();
                job.apply(JobEvent::Complete).unwrap();
            }
            JobStatus::Failed => {
                job.apply(JobEvent::Claim).unwrap();
                job.apply(JobEvent::Fail).unwrap();
            }
            JobStatus::Cancelled => job.apply(JobEvent::Cancel).unwrap(),
        }
        job.attempt = attempt;
        job.checkpoint = serde_json::json!({ "next_step": 7 });
        scheduler.store.save_job(&job).await.unwrap();
        job
    }

    async fn reload(scheduler: &Scheduler, job: &Job) -> Job {
        scheduler.store.get_job(job.id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_running_job_with_attempts_left_is_requeued_with_its_checkpoint_intact() {
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Running, 1).await;

        scheduler.recover().await.unwrap();

        let recovered = reload(&scheduler, &job).await;
        assert_eq!(recovered.status, JobStatus::Queued);
        assert_eq!(
            recovered.checkpoint,
            serde_json::json!({ "next_step": 7 }),
            "a requeued job must resume from its checkpoint, not restart"
        );
        assert_eq!(
            recovered.attempt, 1,
            "recovery itself must not spend an attempt"
        );
    }

    #[tokio::test]
    async fn a_running_job_that_exhausted_its_attempts_is_failed_with_an_explanation() {
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Running, 3).await;

        scheduler.recover().await.unwrap();

        let recovered = reload(&scheduler, &job).await;
        assert_eq!(recovered.status, JobStatus::Failed);
        assert!(
            recovered
                .error
                .as_deref()
                .is_some_and(|e| e.contains("exhausted attempt budget")),
            "a failed recovery must say why: {:?}",
            recovered.error
        );
        assert!(recovered.completed_at.is_some());
    }

    #[tokio::test]
    async fn recovery_leaves_every_job_that_was_not_running_untouched() {
        let scheduler = scheduler().await;
        let mut untouched = Vec::new();
        for status in [
            JobStatus::Queued,
            JobStatus::Paused,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            untouched.push((status, seed(&scheduler, status, 1).await));
        }

        scheduler.recover().await.unwrap();

        for (status, job) in untouched {
            assert_eq!(
                reload(&scheduler, &job).await.status,
                status,
                "recover must not touch a {status:?} job (a paused job stays paused until someone resumes it)"
            );
        }
    }

    #[tokio::test]
    async fn a_requeued_job_is_claimable_after_recovery() {
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Running, 1).await;

        scheduler.recover().await.unwrap();
        let claimed = scheduler
            .store
            .claim_next_job("test-worker")
            .await
            .unwrap()
            .expect("the recovered job must be claimable");

        assert_eq!(claimed.id, job.id);
        assert_eq!(
            claimed.attempt, 2,
            "the re-claim is the job's second attempt"
        );
    }

    #[tokio::test]
    async fn recovering_with_nothing_stuck_is_a_no_op() {
        let scheduler = scheduler().await;
        scheduler.recover().await.unwrap();
        assert!(scheduler.store.list_jobs(None).await.unwrap().is_empty());
    }
}
