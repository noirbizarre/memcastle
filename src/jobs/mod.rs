//! The job scheduler: the daemon's execution mechanism over the durable
//! queue `store::jobs` persists.
//!
//! The invariant this module exists to uphold: **queue state is durable,
//! the in-memory scheduler is only the execution mechanism.** Every status
//! change goes through [`crate::domain::Job::apply`] and is persisted before
//! this module considers it real; a crash loses at most the in-flight
//! `JobControl` handles, never the job records themselves. A user's pause or
//! cancel request is written to the job record before it is acknowledged, so
//! it survives too: [`Scheduler::recover`] re-queues whatever was `Running`
//! (resuming from its checkpoint), unless the user had asked it to stop, in
//! which case it comes back `Paused` or `Cancelled`.

mod control;
mod demo;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{Job, JobEvent, JobId, JobKind, JobStatus, Priority};
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

/// How long shutdown waits for in-flight jobs to reach their next unit-of-work
/// boundary and checkpoint. Bounded because a handler stuck in one long unit
/// (or one that never checks for pause, like `Audit`/`Repair`) must not hang
/// the daemon's exit; a job still running after this is left `Running` and
/// re-queued by [`Scheduler::recover`] on the next start, losing only the
/// work since its last checkpoint.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// How many permits `semaphore` was created with — needed to tell when
    /// *every* worker has finished (all permits are back).
    max_concurrency: u32,
    /// Set once shutdown begins, so a job that starts (or is between
    /// `claim` and registering its control) after the drain has already
    /// walked `controls` still gets interrupted.
    shutting_down: AtomicBool,
    worker: String,
}

impl Scheduler {
    /// Build a scheduler over `store`, allowing at most `max_concurrency`
    /// jobs to execute at once.
    #[must_use]
    pub fn new(store: SurrealStore, max_concurrency: usize) -> Self {
        let max_concurrency = max_concurrency.max(1);
        Self {
            store,
            controls: Arc::new(DashMap::new()),
            semaphore: Arc::new(Semaphore::new(max_concurrency)),
            max_concurrency: u32::try_from(max_concurrency).unwrap_or(u32::MAX),
            shutting_down: AtomicBool::new(false),
            worker: worker_id(),
        }
    }

    /// Recover jobs left `Running` by a daemon that stopped uncleanly.
    ///
    /// Each is re-queued (to be picked up and resumed from its last
    /// checkpoint) if its crash-recovery budget (`Job::max_attempts`,
    /// counted in `Job::recovery_attempts`) allows, or marked `Failed`
    /// otherwise — a job is never silently forgotten.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be read from or written to.
    pub async fn recover(&self) -> Result<()> {
        let stuck = self.store.list_jobs(Some(JobStatus::Running)).await?;
        for mut job in stuck {
            // A stop the user asked for before the crash outranks resuming:
            // re-running a job they cancelled (an applied repair, a big mine)
            // is the one outcome they explicitly ruled out. Cancel beats
            // pause. Neither spends attempt budget: the job did not fail.
            if job.cancel_requested {
                info!(job_id = %job.id, "recovering a job the user had cancelled");
                job.apply(JobEvent::Cancel)?;
            } else if job.pause_requested {
                info!(job_id = %job.id, "recovering a job the user had paused");
                job.apply(JobEvent::Pause)?;
            } else {
                // Only a crash spends the budget. `attempt` also counts the
                // claims that follow a user's resume or a shutdown re-queue,
                // which are not failures: charging those made a job that had
                // merely been paused twice one crash from being failed.
                job.recovery_attempts += 1;
                if job.recovery_attempts < job.max_attempts {
                    info!(
                        job_id = %job.id,
                        recoveries = job.recovery_attempts,
                        "recovering interrupted job to queued"
                    );
                    job.apply(JobEvent::RecoverToQueued)?;
                } else {
                    warn!(
                        job_id = %job.id,
                        recoveries = job.recovery_attempts,
                        "interrupted job exhausted its crash-recovery budget"
                    );
                    job.error = Some(format!(
                        "exhausted its crash-recovery budget: the daemon stopped uncleanly \
                         {} times while this job was running",
                        job.recovery_attempts
                    ));
                    job.apply(JobEvent::Fail)?;
                }
            }
            self.store.save_job(&job).await?;
        }
        Ok(())
    }

    /// Fetch a job the caller named by id.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::JobNotFound`] if no job has that id.
    async fn load_job(&self, id: JobId) -> Result<Job> {
        self.store
            .get_job(id)
            .await?
            .ok_or(crate::Error::JobNotFound { id: id.to_string() })
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

    /// Request that a running job pause at its next checkpoint. Pausing a
    /// job that hasn't started, or has already finished, isn't a legal
    /// transition (see `domain::job`'s transition table).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::JobNotFound`] if no such job exists, and
    /// [`crate::Error::InvalidJobTransition`] if it exists but isn't running
    /// — "not found" would send the caller looking for a typo in the id
    /// when the real answer is "this job is already finished".
    pub async fn request_pause(&self, id: JobId) -> Result<()> {
        // Cloned out of the map so the `DashMap` shard lock is not held
        // across the store write below (a worker finishing needs that shard
        // to remove its own control).
        let control = self.controls.get(&id).map(|control| control.clone());
        if let Some(control) = control {
            // Persist first, signal second: acknowledging a request that only
            // lives in memory is what let a crash drop it. If the job left
            // `Running` in the meantime nothing was marked, and the
            // transition below reports it precisely.
            if self.store.mark_pause_requested(id).await? {
                control.request_pause();
                return Ok(());
            }
        }
        let mut job = self.load_job(id).await?;
        // Not running: let the state machine produce the precise rejection.
        job.apply(JobEvent::Pause)?;
        // Only reachable if a record says `Running` with no worker behind it,
        // which `recover` clears before the API can be reached.
        Err(crate::Error::JobOrphaned { id: id.to_string() })
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
        let control = self.controls.get(&id).map(|control| control.clone());
        if let Some(control) = control {
            // Persist first, signal second — see `request_pause`.
            if self.store.mark_cancel_requested(id).await? {
                control.request_cancel();
                return Ok(());
            }
        }
        let mut job = self.load_job(id).await?;
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
        let mut job = self.load_job(id).await?;
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
        let mut job = self.load_job(id).await?;
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
                    info!("scheduler shutting down; draining in-flight jobs");
                    break;
                }
                _ = ticker.tick() => {
                    self.clone().try_dispatch_one().await;
                }
            }
        }
        self.drain(DRAIN_TIMEOUT).await;
    }

    /// Stop in-flight jobs at their next unit-of-work boundary and wait, up
    /// to `timeout`, for them to checkpoint and hand their job back to the
    /// queue. No new job is claimed once this runs (the dispatch loop has
    /// already exited).
    ///
    /// A job that does not stop in time is left `Running` on purpose:
    /// [`Scheduler::recover`] handles exactly that on the next start.
    async fn drain(&self, timeout: Duration) {
        // Flag first, then walk the controls: a job registering its control
        // concurrently either lands in the walk or sees the flag itself.
        self.shutting_down.store(true, Ordering::SeqCst);
        for control in self.controls.iter() {
            control.request_interrupt();
        }

        // Every running job holds one permit for its whole execution, so
        // owning all of them means every worker has finished writing its
        // final state.
        let all_idle = Arc::clone(&self.semaphore).acquire_many_owned(self.max_concurrency);
        match tokio::time::timeout(timeout, all_idle).await {
            Ok(_) => info!("all in-flight jobs stopped cleanly"),
            Err(_) => warn!(
                remaining = self.controls.len(),
                "in-flight jobs did not stop within the drain timeout; they stay running and are recovered on next start"
            ),
        }
    }

    async fn try_dispatch_one(self: Arc<Self>) {
        let Ok(permit) = Arc::clone(&self.semaphore).try_acquire_owned() else {
            return; // at capacity; try again next tick
        };
        let claimed = self.store.claim_next_job(&self.worker).await;
        match claimed {
            Ok(Some(job)) => {
                // Registered here, before the spawn, not inside `execute`:
                // the job is already `Running` in the store, so a pause or
                // cancel arriving in the gap would otherwise find no control
                // and act on a record the worker is about to overwrite.
                let control = JobControl::default();
                self.controls.insert(job.id, control.clone());
                let scheduler = Arc::clone(&self);
                tokio::spawn(async move {
                    let _permit = permit; // held for the job's whole execution
                    scheduler.execute(job, control).await;
                });
            }
            Ok(None) => {} // nothing queued; permit is dropped, released
            Err(error) => {
                warn!(%error, "failed to claim next job");
            }
        }
    }

    async fn execute(&self, mut job: Job, control: JobControl) {
        if self.shutting_down.load(Ordering::SeqCst) {
            // Claimed just as shutdown began: `drain` may already have
            // walked `controls` without seeing this job.
            control.request_interrupt();
        }
        let ctx = JobContext::new(job.id, control.clone(), self.store.clone());

        let outcome = match job.kind.clone() {
            JobKind::Demo { steps } => demo::run(&ctx, &mut job, demo::DemoParams { steps }).await,
            JobKind::Mine { source, wing } => {
                crate::mining::run(&ctx, &mut job, crate::mining::MiningParams { source, wing })
                    .await
            }
            JobKind::Checkpoint { payload } => {
                crate::checkpoint::run(
                    &ctx,
                    &mut job,
                    crate::checkpoint::CheckpointParams { payload },
                )
                .await
            }
            JobKind::Audit { scope } => {
                crate::audit::run(&ctx, &mut job, crate::audit::AuditParams { scope }).await
            }
            JobKind::Repair {
                dry_run,
                based_on_job,
            } => {
                crate::repair::run(
                    &ctx,
                    &mut job,
                    crate::repair::RepairParams {
                        dry_run,
                        based_on_job,
                    },
                )
                .await
            }
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
        } else if event == JobEvent::Pause && control.was_interrupted() {
            // Paused by shutdown, not by a user: hand the job straight back
            // to the queue (checkpoint intact) so the next daemon resumes it
            // without anyone having to notice and press resume.
            if let Err(error) = job.apply(JobEvent::Resume) {
                warn!(job_id = %job.id, %error, "could not re-queue a job interrupted by shutdown");
            }
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

    /// Pretend `job` has already survived `recoveries` daemon crashes.
    async fn set_recoveries(scheduler: &Scheduler, job: &Job, recoveries: u32) {
        let mut job = reload(scheduler, job).await;
        job.recovery_attempts = recoveries;
        scheduler.store.save_job(&job).await.unwrap();
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
    async fn a_running_job_whose_crash_budget_is_spent_is_failed_with_an_explanation() {
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Running, 1).await;
        // Two earlier crashes were survived; this is the third.
        set_recoveries(&scheduler, &job, 2).await;

        scheduler.recover().await.unwrap();

        let recovered = reload(&scheduler, &job).await;
        assert_eq!(recovered.status, JobStatus::Failed);
        assert!(
            recovered
                .error
                .as_deref()
                .is_some_and(|e| e.contains("exhausted its crash-recovery budget")),
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

    /// Submit a slow demo job (150ms/step), run the dispatch loop, and wait
    /// until it is genuinely mid-flight with at least one step checkpointed.
    async fn running_scheduler_with_a_job_in_flight() -> (
        Arc<Scheduler>,
        Job,
        CancellationToken,
        tokio::task::JoinHandle<()>,
    ) {
        let scheduler = Arc::new(scheduler().await);
        let job = scheduler
            .submit(JobKind::Demo { steps: 200 }, Priority::Normal, "test")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(Arc::clone(&scheduler).run(shutdown.clone()));
        for _ in 0..200 {
            let current = reload(&scheduler, &job).await;
            if current.status == JobStatus::Running && current.progress.current >= 1 {
                return (scheduler, job, shutdown, handle);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the job never got going");
    }

    #[tokio::test]
    async fn shutdown_waits_for_an_in_flight_job_and_requeues_it_with_its_checkpoint() {
        let (scheduler, job, shutdown, handle) = running_scheduler_with_a_job_in_flight().await;

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run must return once in-flight jobs have stopped")
            .unwrap();

        let after = reload(&scheduler, &job).await;
        assert_eq!(
            after.status,
            JobStatus::Queued,
            "a job interrupted by shutdown must be back in the queue, not stranded Running or Paused"
        );
        assert!(
            after.checkpoint["next_step"].as_u64().unwrap_or(0) >= 1,
            "the interrupted job must have checkpointed its progress: {}",
            after.checkpoint
        );
        assert_eq!(after.lease_owner, None);
    }

    #[tokio::test]
    async fn a_pause_the_user_asked_for_survives_a_shutdown() {
        let (scheduler, job, shutdown, handle) = running_scheduler_with_a_job_in_flight().await;

        scheduler.request_pause(job.id).await.unwrap();
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run must return once in-flight jobs have stopped")
            .unwrap();

        assert_eq!(
            reload(&scheduler, &job).await.status,
            JobStatus::Paused,
            "a restart must not silently un-pause a job the user paused"
        );
    }

    #[tokio::test]
    async fn a_job_that_ignores_the_interrupt_is_left_running_for_recovery() {
        // A handler that never checks for pause (like Audit/Repair) must not
        // hang shutdown: drain gives up at the timeout and leaves the record
        // `Running`, which is exactly what `recover` repairs on next start.
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Running, 1).await;
        let _held = Arc::clone(&scheduler.semaphore)
            .try_acquire_owned()
            .expect("the only permit is free");

        let started = tokio::time::Instant::now();
        scheduler.drain(Duration::from_millis(100)).await;

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "drain must honour its timeout instead of waiting forever"
        );
        assert_eq!(reload(&scheduler, &job).await.status, JobStatus::Running);
        scheduler.recover().await.unwrap();
        assert_eq!(reload(&scheduler, &job).await.status, JobStatus::Queued);
    }

    #[tokio::test]
    async fn pausing_a_job_that_is_not_running_is_an_invalid_transition_not_a_missing_job() {
        let scheduler = scheduler().await;
        let done = seed(&scheduler, JobStatus::Completed, 1).await;

        let error = scheduler.request_pause(done.id).await.unwrap_err();

        assert!(
            matches!(error, crate::Error::InvalidJobTransition { .. }),
            "the job exists, so the answer is 'wrong state', not 'not found': {error:?}"
        );
    }

    #[tokio::test]
    async fn pausing_a_job_that_does_not_exist_is_not_found() {
        let scheduler = scheduler().await;

        let error = scheduler.request_pause(JobId::new()).await.unwrap_err();

        assert!(
            matches!(error, crate::Error::JobNotFound { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn recovering_with_nothing_stuck_is_a_no_op() {
        let scheduler = scheduler().await;
        scheduler.recover().await.unwrap();
        assert!(scheduler.store.list_jobs(None).await.unwrap().is_empty());
    }

    /// Persist a `Running` demo job carrying the stop request a user made
    /// before a crash, as the API would have recorded it.
    async fn seed_running_with_request(scheduler: &Scheduler, pause: bool, cancel: bool) -> Job {
        let job = seed(scheduler, JobStatus::Running, 1).await;
        if pause {
            assert!(scheduler.store.mark_pause_requested(job.id).await.unwrap());
        }
        if cancel {
            assert!(scheduler.store.mark_cancel_requested(job.id).await.unwrap());
        }
        job
    }

    #[tokio::test]
    async fn a_job_cancelled_before_a_crash_is_cancelled_by_recovery_not_rerun() {
        let scheduler = scheduler().await;
        let job = seed_running_with_request(&scheduler, false, true).await;

        scheduler.recover().await.unwrap();

        let recovered = reload(&scheduler, &job).await;
        assert_eq!(recovered.status, JobStatus::Cancelled);
        assert!(!recovered.cancel_requested, "the request is spent");
        assert_eq!(recovered.attempt, 1, "a cancel is not a failed attempt");
    }

    #[tokio::test]
    async fn a_job_paused_before_a_crash_comes_back_paused_with_its_checkpoint() {
        let scheduler = scheduler().await;
        let job = seed_running_with_request(&scheduler, true, false).await;

        scheduler.recover().await.unwrap();

        let recovered = reload(&scheduler, &job).await;
        assert_eq!(recovered.status, JobStatus::Paused);
        assert!(!recovered.pause_requested);
        assert_eq!(recovered.checkpoint, serde_json::json!({ "next_step": 7 }));
    }

    #[tokio::test]
    async fn a_cancel_outranks_a_pause_when_both_were_pending_at_the_crash() {
        let scheduler = scheduler().await;
        let job = seed_running_with_request(&scheduler, true, true).await;

        scheduler.recover().await.unwrap();

        assert_eq!(reload(&scheduler, &job).await.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn a_request_is_persisted_on_the_running_job_before_it_is_acknowledged() {
        let (scheduler, job, shutdown, handle) = running_scheduler_with_a_job_in_flight().await;

        scheduler.request_cancel(job.id).await.unwrap();

        // Read straight after the call returns: the durable record, not the
        // in-memory control, is what a crash would leave behind. (The job may
        // already have honoured it, in which case it is `Cancelled` and the
        // flag is spent.)
        let after = reload(&scheduler, &job).await;
        assert!(
            after.cancel_requested || after.status == JobStatus::Cancelled,
            "an acknowledged cancel must be on the record: {after:?}"
        );
        shutdown.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn saving_a_running_job_does_not_erase_a_request_made_since_it_was_loaded() {
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Running, 1).await;
        // The handler's in-memory copy predates the user's request...
        let stale = job.clone();
        assert!(scheduler.store.mark_pause_requested(job.id).await.unwrap());

        // ...and its next checkpoint writes the whole record.
        scheduler.store.save_job(&stale).await.unwrap();

        assert!(reload(&scheduler, &job).await.pause_requested);
    }

    #[tokio::test]
    async fn marking_a_job_that_is_not_running_changes_nothing() {
        let scheduler = scheduler().await;
        let queued = seed(&scheduler, JobStatus::Queued, 0).await;

        assert!(
            !scheduler
                .store
                .mark_cancel_requested(queued.id)
                .await
                .unwrap()
        );
        assert!(!reload(&scheduler, &queued).await.cancel_requested);
    }

    #[tokio::test]
    async fn a_user_pause_that_arrives_during_shutdown_still_wins() {
        let (scheduler, job, shutdown, handle) = running_scheduler_with_a_job_in_flight().await;
        // Shutdown has begun and interrupted the job, but the handler has not
        // yet reached its next boundary.
        scheduler.shutting_down.store(true, Ordering::SeqCst);
        for control in scheduler.controls.iter() {
            control.request_interrupt();
        }

        scheduler.request_pause(job.id).await.unwrap();
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("run must return")
            .unwrap();

        assert_eq!(reload(&scheduler, &job).await.status, JobStatus::Paused);
    }

    #[tokio::test]
    async fn a_job_resumed_or_requeued_many_times_still_survives_one_crash() {
        let scheduler = scheduler().await;
        // Ten claims by a worker, none of them a crash: pauses that were
        // resumed and shutdown re-queues. `attempt` far past `max_attempts`.
        let job = seed(&scheduler, JobStatus::Running, 10).await;

        scheduler.recover().await.unwrap();

        let recovered = reload(&scheduler, &job).await;
        assert_eq!(
            recovered.status,
            JobStatus::Queued,
            "claims that were not crashes must not count against the budget"
        );
        assert_eq!(recovered.recovery_attempts, 1);
        assert_eq!(
            recovered.attempt, 10,
            "recovery does not touch the claim count"
        );
    }

    #[tokio::test]
    async fn a_user_resume_and_a_shutdown_requeue_do_not_spend_the_crash_budget() {
        let scheduler = scheduler().await;
        let mut job = seed(&scheduler, JobStatus::Queued, 0).await;
        for _ in 0..5 {
            let claimed = scheduler
                .store
                .claim_next_job("test-worker")
                .await
                .unwrap()
                .expect("claimable");
            job = claimed;
            // Pause then resume, exactly what the user path and the
            // shutdown re-queue both do.
            job.apply(JobEvent::Pause).unwrap();
            job.apply(JobEvent::Resume).unwrap();
            scheduler.store.save_job(&job).await.unwrap();
        }
        assert_eq!(job.attempt, 5, "five claims");
        assert_eq!(job.recovery_attempts, 0);
    }

    #[tokio::test]
    async fn a_job_that_crashes_max_attempts_times_is_failed_with_the_reason_recorded() {
        let scheduler = scheduler().await;
        let job = seed(&scheduler, JobStatus::Queued, 0).await;

        for crash in 1..=job.max_attempts {
            let claimed = scheduler
                .store
                .claim_next_job("test-worker")
                .await
                .unwrap()
                .expect("claimable until the budget is spent");
            assert_eq!(claimed.id, job.id);
            // The daemon dies while it runs, and the next one recovers it.
            scheduler.recover().await.unwrap();

            let after = reload(&scheduler, &job).await;
            assert_eq!(after.recovery_attempts, crash);
            if crash < job.max_attempts {
                assert_eq!(
                    after.status,
                    JobStatus::Queued,
                    "crash {crash} is survivable"
                );
            } else {
                assert_eq!(after.status, JobStatus::Failed);
                assert!(
                    after
                        .error
                        .as_deref()
                        .is_some_and(|e| e.contains("3 times")),
                    "the failure must say how many crashes: {:?}",
                    after.error
                );
            }
        }
    }
}
