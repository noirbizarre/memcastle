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

use crate::config::{DedupConfig, MiningConfig};
use crate::domain::{AccessTokens, Job, JobEvent, JobId, JobKind, JobStatus, Priority};
use crate::embed::Embeddings;
use crate::error::Result;
use crate::events::{Action, Event, EventBus};
use crate::extract::Extraction;
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
    /// resume from, for a handler that records one (audit and repair rescan
    /// instead).
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

/// How many times a job-control request re-reads and retries when the job
/// changes status underneath it. One retry already covers the only realistic
/// race (a worker claiming the job); the rest is headroom, not a loop anyone
/// should reach the end of.
const TRANSITION_ATTEMPTS: usize = 5;

/// How long shutdown waits for in-flight jobs to reach their next unit-of-work
/// boundary and checkpoint, unless configured otherwise
/// (`jobs.drain_timeout_secs`). Bounded because a handler stuck in one long
/// unit must not hang the daemon's exit; a job still running after this is
/// left `Running` and re-queued by [`Scheduler::recover`] on the next start,
/// losing only the work since its last checkpoint.
const DEFAULT_DRAIN_TIMEOUT: Duration =
    Duration::from_secs(crate::config::DEFAULT_DRAIN_TIMEOUT_SECS);

/// How long a job's lease lasts without being renewed, unless configured
/// otherwise (`jobs.lease_ttl_secs`). The heartbeat renews it every third of
/// this, so a live daemon survives two missed beats before it is reaped.
const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(crate::config::DEFAULT_LEASE_TTL_SECS);

/// A label unique to this scheduler instance, recorded as `lease_owner` on
/// the jobs it claims. Load-bearing: it is what a fenced write and a lease
/// renewal are checked against, so two daemons must never share one — hence
/// a random suffix on top of the pid (a pid alone repeats across hosts and
/// across restarts).
fn worker_id() -> String {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("memcastle-{}-{}", std::process::id(), &suffix[..8])
}

/// Owns the in-process side of the job queue: claiming, dispatching to
/// bounded concurrent workers, and the cooperative pause/cancel signalling
/// that lets a handler check in on its own terms rather than being killed.
pub struct Scheduler {
    store: SurrealStore,
    controls: Arc<DashMap<JobId, JobControl>>,
    semaphore: Arc<Semaphore>,
    /// A second bound on long-running work so short jobs retain a scheduler slot.
    background_semaphore: Arc<Semaphore>,
    /// How many permits `semaphore` was created with — needed to tell when
    /// *every* worker has finished (all permits are back).
    max_concurrency: u32,
    /// Set once shutdown begins, so a job that starts (or is between
    /// `claim` and registering its control) after the drain has already
    /// walked `controls` still gets interrupted.
    shutting_down: AtomicBool,
    worker: String,
    /// How long [`Scheduler::run`] waits for in-flight jobs on shutdown.
    drain_timeout: Duration,
    /// How long a claimed job's lease lasts between heartbeats.
    lease_ttl: Duration,
    /// Whether this daemon is the only one that can ever use this store (an
    /// embedded palace, guarded by SurrealKV's file lock). If so, every
    /// `Running` job at startup belongs to a dead predecessor and is
    /// recovered at once; if not, only jobs whose lease has expired are.
    exclusive_store: bool,
    /// When a heartbeat last renewed every lease it tried to. If renewals
    /// keep failing for a whole lease, this daemon must assume its jobs have
    /// been reaped and stop them.
    last_renewed: std::sync::Mutex<tokio::time::Instant>,
    /// The embedding provider handed to every job's context, and consulted to
    /// decide whether finishing a drawer-writing job should queue a sweep.
    embeddings: Embeddings,
    /// The `[mining]` settings handed to every mining job's context.
    mining: MiningConfig,
    /// The extraction provider handed to every job's context, and consulted
    /// to decide whether finishing a mining job should queue a sweep.
    extraction: Extraction,
    /// The `[dedup]` settings handed to every job's context.
    dedup: DedupConfig,
    /// The access tokens of the sources that sign in with OAuth, handed to every job's context.
    credentials: Option<Arc<dyn AccessTokens>>,
    /// Where a job's changes are announced, and handed to every job's context for the handler's own writes.
    events: EventBus,
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
            background_semaphore: Arc::new(Semaphore::new(2.min(if max_concurrency == 1 {
                1
            } else {
                max_concurrency - 1
            }))),
            max_concurrency: u32::try_from(max_concurrency).unwrap_or(u32::MAX),
            shutting_down: AtomicBool::new(false),
            worker: worker_id(),
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            lease_ttl: DEFAULT_LEASE_TTL,
            exclusive_store: true,
            last_renewed: std::sync::Mutex::new(tokio::time::Instant::now()),
            embeddings: Embeddings::disabled(),
            mining: MiningConfig::default(),
            extraction: Extraction::disabled(),
            dedup: DedupConfig::default(),
            credentials: None,
            events: EventBus::new(),
        }
    }

    /// Give the scheduler the daemon's event bus, so job changes reach `GET /api/events`.
    #[must_use]
    pub fn with_events(mut self, events: EventBus) -> Self {
        self.events = events;
        self
    }

    /// Keep one job slot for short work when the total limit allows it.
    #[must_use]
    pub fn with_background_concurrency(mut self, limit: usize) -> Self {
        let total = self.max_concurrency as usize;
        let effective = limit.max(1).min(if total == 1 { 1 } else { total - 1 });
        self.background_semaphore = Arc::new(Semaphore::new(effective));
        self
    }

    /// Announce a job change that has already been saved.
    fn announce(&self, action: Action, job: &Job) {
        self.events.publish(job_event(action, job));
    }

    /// Give the scheduler the access tokens of the sources that sign in with OAuth.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Arc<dyn AccessTokens>) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Give the scheduler the deduplication settings (see `[dedup]`).
    #[must_use]
    pub fn with_dedup(mut self, dedup: DedupConfig) -> Self {
        self.dedup = dedup;
        self
    }

    /// Give the scheduler an extraction provider (see `[extraction]`).
    #[must_use]
    pub fn with_extraction(mut self, extraction: Extraction) -> Self {
        self.extraction = extraction;
        self
    }

    /// Queue an extraction sweep unless there is nothing to do it with or one
    /// is already waiting.
    ///
    /// Called after a mining job completes (only mined drawers are read) and
    /// at startup. Coalesced on *queued* jobs exactly like
    /// [`Self::ensure_embedding_sweep`], and never an error to the caller:
    /// extraction is derived data, so failing to schedule it must not fail
    /// the job that triggered it.
    pub async fn ensure_extraction_sweep(&self) {
        if !self.extraction.is_configured() {
            return;
        }
        let queued = match self.store.list_jobs(Some(JobStatus::Queued)).await {
            Ok(jobs) => jobs,
            Err(error) => {
                warn!(%error, "could not check for a queued extraction sweep");
                return;
            }
        };
        if queued
            .iter()
            .any(|job| matches!(job.kind, JobKind::Extract { .. }))
        {
            return;
        }
        if let Err(error) = self
            .submit(
                JobKind::Extract { wing: None },
                Priority::Background,
                // Not a channel: nobody asked, the daemon did.
                "system",
            )
            .await
        {
            warn!(%error, "could not queue an extraction sweep");
        }
    }

    /// Give the scheduler the `[mining]` settings its jobs run with.
    #[must_use]
    pub fn with_mining(mut self, mining: MiningConfig) -> Self {
        self.mining = mining;
        self
    }

    /// Give the scheduler an embedding provider (see `[embeddings]`).
    #[must_use]
    pub fn with_embeddings(mut self, embeddings: Embeddings) -> Self {
        self.embeddings = embeddings;
        self
    }

    /// Queue an embedding sweep unless there is nothing to do it with or one
    /// is already waiting.
    ///
    /// Called after anything that writes drawers, so new memory becomes
    /// semantically searchable without anyone asking. Coalesced on *queued*
    /// jobs only: a sweep that is already running may have passed the new
    /// drawer, so one more is queued behind it, but a pile of writes yields a
    /// single waiting sweep. Never an error to the caller: embedding is
    /// derived data, so failing to schedule it must not fail the write that
    /// triggered it.
    pub async fn ensure_embedding_sweep(&self) {
        if !self.embeddings.is_configured() {
            return;
        }
        let queued = match self.store.list_jobs(Some(JobStatus::Queued)).await {
            Ok(jobs) => jobs,
            Err(error) => {
                warn!(%error, "could not check for a queued embedding sweep");
                return;
            }
        };
        if queued
            .iter()
            .any(|job| matches!(job.kind, JobKind::Embed { .. }))
        {
            return;
        }
        if let Err(error) = self
            .submit(
                JobKind::Embed { wing: None },
                Priority::Background,
                // Not a channel: nobody asked, the daemon did.
                "system",
            )
            .await
        {
            warn!(%error, "could not queue an embedding sweep");
        }
    }

    /// Set how long a job's lease lasts between heartbeats (see
    /// `jobs.lease_ttl_secs`).
    #[must_use]
    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    /// Declare that other daemons may share this store (a remote backend), so
    /// startup recovery must not treat every `Running` job as abandoned: only
    /// those whose lease has expired are. The default assumes the embedded
    /// backend, where SurrealKV's file lock guarantees this daemon is alone.
    #[must_use]
    pub fn with_shared_store(mut self) -> Self {
        self.exclusive_store = false;
        self
    }

    /// Set how long shutdown waits for in-flight jobs (see
    /// `jobs.drain_timeout_secs`).
    #[must_use]
    pub fn with_drain_timeout(mut self, drain_timeout: Duration) -> Self {
        self.drain_timeout = drain_timeout;
        self
    }

    /// Recover jobs left `Running` by a daemon that stopped uncleanly.
    ///
    /// Each is re-queued (to be picked up and resumed from its last
    /// checkpoint) if its crash-recovery budget (`Job::max_attempts`,
    /// counted in `Job::recovery_attempts`) allows, or marked `Failed`
    /// otherwise — a job is never silently forgotten.
    ///
    /// With an exclusive store (embedded) every `Running` job is a dead
    /// predecessor's and is recovered at once. With a shared one (remote)
    /// only jobs whose lease has expired are: a live daemon's jobs are
    /// renewed, and stealing one would run it twice.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be read from or written to.
    pub async fn recover(&self) -> Result<()> {
        if self.exclusive_store {
            for job in self.store.list_jobs(Some(JobStatus::Running)).await? {
                self.recover_job(job).await?;
            }
            Ok(())
        } else {
            self.reap_expired_leases().await
        }
    }

    /// Recover every `Running` job whose lease has lapsed and that this
    /// daemon is not itself running: the job of a daemon that died, stalled
    /// or was partitioned away. Run periodically alongside the heartbeat.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be read from or written to.
    async fn reap_expired_leases(&self) -> Result<()> {
        let now = chrono::Utc::now();
        for job in self.store.list_jobs(Some(JobStatus::Running)).await? {
            // Our own jobs are renewed by the heartbeat; one that merely
            // looks expired because a beat was late must not be re-queued
            // while it is still executing here.
            if self.controls.contains_key(&job.id) {
                continue;
            }
            // No expiry at all is a job written before leases existed: there
            // is no live lease to respect.
            if job
                .lease_expires_at
                .is_some_and(|expires_at| expires_at > now)
            {
                continue;
            }
            self.recover_job(job).await?;
        }
        Ok(())
    }

    /// Put one abandoned `Running` job back where it belongs.
    ///
    /// Written back only if the stored record still carries the lease that
    /// was read (unless the store is exclusive, where nothing else writes):
    /// if the owner renewed in the meantime, or another daemon already
    /// reaped it, this does nothing.
    async fn recover_job(&self, seen: Job) -> Result<()> {
        let mut job = seen.clone();
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
        if self.exclusive_store {
            self.store.save_job(&job).await?;
            self.announce(Action::Updated, &job);
            return Ok(());
        }
        if self.store.save_job_if_lease_unchanged(&job, &seen).await? {
            self.announce(Action::Updated, &job);
        } else {
            info!(job_id = %seen.id, "another daemon recovered or renewed this job first");
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
        info!(
            job_id = %job.id,
            job_type = kind_name(&job.kind),
            priority = ?job.priority,
            requested_by = %job.requested_by,
            "job queued"
        );
        self.announce(Action::Created, &job);
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
                info!(job_id = %id, "pause requested for running job");
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
        // A bounded loop: the guarded save below only fails when the job
        // changed status after it was read (typically a worker claimed a queued
        // job), and the fix is to start over so the now-running job takes the
        // cooperative path above rather than being cancelled behind its
        // worker's back.
        for _ in 0..TRANSITION_ATTEMPTS {
            let control = self.controls.get(&id).map(|control| control.clone());
            if let Some(control) = control {
                // Persist first, signal second — see `request_pause`.
                if self.store.mark_cancel_requested(id).await? {
                    control.request_cancel();
                    info!(job_id = %id, "cancel requested for running job");
                    return Ok(());
                }
            }
            if self.apply_guarded(id, JobEvent::Cancel).await? {
                info!(job_id = %id, "job cancelled before it ran");
                return Ok(());
            }
        }
        Err(Self::contended(id))
    }

    /// Abort a mining worker owned by this daemon after durably requesting cancellation.
    /// Blocking work already handed to another thread may continue briefly; removing
    /// the lease before returning fences any later checkpoint from that work.
    pub async fn force_cancel(&self, id: JobId) -> Result<()> {
        let job = self.load_job(id).await?;
        if !matches!(job.kind, JobKind::Mine { .. }) {
            return Err(crate::Error::invalid_input(
                "job",
                "force-cancel is only available for mining jobs",
            ));
        }
        if job.status != JobStatus::Running {
            return Err(crate::Error::invalid_input(
                "job",
                "force-cancel requires a running mining job",
            ));
        }
        if job.lease_owner.as_deref() != Some(&self.worker) {
            return Err(crate::Error::invalid_input(
                "job",
                "this mining job runs on another daemon; connect to its owner or request a graceful cancel",
            ));
        }
        let control = self
            .controls
            .get(&id)
            .map(|item| item.clone())
            .ok_or_else(|| crate::Error::JobOrphaned { id: id.to_string() })?;
        // This write survives a daemon crash between receiving the request and aborting the task.
        if !self
            .store
            .mark_cancel_requested_by(id, &self.worker)
            .await?
        {
            return Err(crate::Error::job_contended(id.to_string()));
        }
        control.request_cancel();
        let handle = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(handle) = control.worker.lock().await.take() {
                    break Ok(handle);
                }
                // The claim registers its control before spawning; do not mistake that
                // short gap for a worker on another daemon.
                if !self.controls.contains_key(&id) {
                    break Err(crate::Error::job_contended(id.to_string()));
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| crate::Error::JobOrphaned { id: id.to_string() })??;
        handle.abort();
        let _ = handle.await;
        self.controls.remove(&id);
        let mut current = self.load_job(id).await?;
        if current.status != JobStatus::Running
            || current.lease_owner.as_deref() != Some(&self.worker)
        {
            return Err(crate::Error::job_contended(id.to_string()));
        }
        current.apply(JobEvent::Cancel)?;
        if !self
            .store
            .save_job_if_running_owner(&current, &self.worker)
            .await?
        {
            return Err(crate::Error::job_contended(id.to_string()));
        }
        self.announce(Action::Updated, &current);
        Ok(())
    }

    /// Move a `Paused` job back to `Queued` so the dispatch loop picks it
    /// up again.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist or isn't `Paused`.
    pub async fn resume(&self, id: JobId) -> Result<()> {
        self.apply_guarded_until_settled(id, JobEvent::Resume)
            .await?;
        info!(job_id = %id, "job resumed; queued again");
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
        self.apply_guarded_until_settled(id, JobEvent::Retry)
            .await?;
        info!(job_id = %id, "failed job retried; queued again");
        Ok(())
    }

    /// Load `id`, apply `event`, and save the result only if the job is still
    /// in the status it was read in — so a transition decided from a stale read
    /// can never overwrite what another writer did in between. Returns whether
    /// the save happened; `false` means the job changed under us.
    ///
    /// # Errors
    ///
    /// Returns an error if the job doesn't exist, `event` is not legal from
    /// its status, or the store fails.
    async fn apply_guarded(&self, id: JobId, event: JobEvent) -> Result<bool> {
        let mut job = self.load_job(id).await?;
        let seen = job.status;
        job.apply(event)?;
        let saved = self.store.save_job_if_status(&job, seen).await?;
        if saved {
            self.announce(Action::Updated, &job);
        }
        Ok(saved)
    }

    /// [`Self::apply_guarded`], re-reading and retrying when the job changed
    /// mid-request. The re-read is what turns "it was claimed meanwhile" into
    /// the state machine's precise rejection instead of a lost update.
    async fn apply_guarded_until_settled(&self, id: JobId, event: JobEvent) -> Result<()> {
        for _ in 0..TRANSITION_ATTEMPTS {
            if self.apply_guarded(id, event).await? {
                return Ok(());
            }
        }
        Err(Self::contended(id))
    }

    /// The job kept changing status faster than a request could be applied.
    /// Practically unreachable; reported rather than looping forever.
    fn contended(id: JobId) -> crate::Error {
        crate::Error::job_contended(id.to_string())
    }

    /// Run the dispatch loop until `shutdown` fires. Claims at most one job
    /// per tick and spawns it onto a bounded worker; polling (rather than a
    /// live query) keeps this loop's own logic trivial to read and test.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        // A third of the lease, so two beats can be missed before a live
        // daemon's job looks abandoned; floored so a tiny test TTL cannot
        // turn the heartbeat into a busy loop.
        let mut heartbeat =
            tokio::time::interval((self.lease_ttl / 3).max(Duration::from_millis(50)));
        loop {
            tokio::select! {
                () = shutdown.cancelled() => {
                    info!("scheduler shutting down; draining in-flight jobs");
                    break;
                }
                _ = ticker.tick() => {
                    self.clone().try_dispatch_one().await;
                }
                _ = heartbeat.tick() => {
                    self.heartbeat().await;
                    if let Err(error) = self.reap_expired_leases().await {
                        warn!(%error, "failed to reap expired job leases");
                    }
                }
            }
        }
        self.drain(self.drain_timeout).await;
    }

    /// Renew the lease on every job this daemon is running, and stop any job
    /// whose lease it can no longer hold.
    ///
    /// A renewal that finds the lease gone (the job was reaped and may be
    /// running elsewhere now) interrupts the job at its next boundary; its
    /// fenced writes are refused meanwhile, so it cannot clobber the new
    /// owner. If the store itself is unreachable for a whole lease, this
    /// daemon assumes the same has happened and stops everything: a
    /// partitioned daemon must fence itself, because nothing else can.
    async fn heartbeat(&self) {
        let running: Vec<(JobId, JobControl)> = self
            .controls
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();
        let ttl = chrono::Duration::from_std(self.lease_ttl).unwrap_or(chrono::Duration::MAX);
        let expires_at = chrono::Utc::now() + ttl;
        let mut every_renewal_reached_the_store = true;
        for (id, control) in running {
            match self
                .store
                .renew_job_lease(id, &self.worker, expires_at)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    warn!(job_id = %id, "lost the lease on a running job; stopping it");
                    control.request_interrupt();
                }
                Err(error) => {
                    every_renewal_reached_the_store = false;
                    warn!(job_id = %id, %error, "failed to renew a job lease");
                }
            }
        }
        let mut last_renewed = self
            .last_renewed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if every_renewal_reached_the_store {
            *last_renewed = tokio::time::Instant::now();
        } else if last_renewed.elapsed() > self.lease_ttl {
            warn!("could not renew job leases for a whole lease; stopping every job");
            for entry in self.controls.iter() {
                entry.value().request_interrupt();
            }
        }
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
        // Never lease a job only to park it behind another long-running job.
        let background_permit = Arc::clone(&self.background_semaphore)
            .try_acquire_owned()
            .ok();
        let ttl = chrono::Duration::from_std(self.lease_ttl).unwrap_or(chrono::Duration::MAX);
        let claimed = self
            .store
            .claim_next_job_eligible(&self.worker, ttl, background_permit.is_some())
            .await;
        match claimed {
            Ok(Some(job)) => {
                let background_permit = if is_background(&job.kind) {
                    background_permit
                } else {
                    None
                };
                info!(
                    job_id = %job.id,
                    job_type = kind_name(&job.kind),
                    priority = ?job.priority,
                    attempt = job.attempt,
                    worker_id = %self.worker,
                    "job claimed by worker"
                );
                self.announce(Action::Updated, &job);
                // Registered here, before the spawn, not inside `execute`:
                // the job is already `Running` in the store, so a pause or
                // cancel arriving in the gap would otherwise find no control
                // and act on a record the worker is about to overwrite.
                let control = JobControl::default();
                let worker_slot = control.worker.clone();
                self.controls.insert(job.id, control.clone());
                let scheduler = Arc::clone(&self);
                let handle = tokio::spawn(async move {
                    let _permit = permit; // held for the job's whole execution
                    let _background_permit = background_permit;
                    scheduler.execute(job, control).await;
                });
                // The abort handle is installed on the same control registered
                // before spawn, so a force request cannot miss the worker.
                *worker_slot.lock().await = Some(handle);
            }
            Ok(None) => {} // nothing queued; permit is dropped, released
            Err(error) => {
                warn!(%error, "failed to claim next job");
            }
        }
    }

    async fn execute(&self, job: Job, control: JobControl) {
        // Every log line a handler emits inherits the job's identity.
        let span = tracing::info_span!("job", job_id = %job.id, job_type = kind_name(&job.kind));
        tracing::Instrument::instrument(self.execute_inner(job, control), span).await;
    }

    async fn execute_inner(&self, mut job: Job, control: JobControl) {
        let started = std::time::Instant::now();
        info!(attempt = job.attempt, "job started");
        if self.shutting_down.load(Ordering::SeqCst) {
            // Claimed just as shutdown began: `drain` may already have
            // walked `controls` without seeing this job.
            control.request_interrupt();
        }
        // Fenced to this worker's lease: if a partition let another daemon
        // reap this job, this run's checkpoints are refused, not merged.
        let ctx = JobContext::new(job.id, control.clone(), self.store.clone())
            .with_lease(self.worker.clone())
            .with_embeddings(self.embeddings.clone())
            .with_extraction(self.extraction.clone())
            .with_dedup(self.dedup.clone())
            .with_mining(self.mining.clone())
            .with_credentials(self.credentials.clone())
            .with_events(self.events.clone());

        let kind_wrote_drawers =
            matches!(job.kind, JobKind::Mine { .. } | JobKind::Checkpoint { .. });
        let outcome = match job.kind.clone() {
            JobKind::Demo { steps } => demo::run(&ctx, &mut job, demo::DemoParams { steps }).await,
            JobKind::Mine {
                source,
                wing,
                full,
                options,
            } => {
                crate::mining::run(
                    &ctx,
                    &mut job,
                    crate::mining::MiningParams {
                        source,
                        wing,
                        full,
                        options,
                    },
                )
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
            JobKind::Audit { wing } => {
                crate::audit::run(&ctx, &mut job, crate::audit::AuditParams { wing }).await
            }
            JobKind::Embed { wing } => {
                crate::embed::job::run(&ctx, &mut job, crate::embed::job::EmbedParams { wing })
                    .await
            }
            JobKind::Extract { wing } => {
                crate::extract::job::run(
                    &ctx,
                    &mut job,
                    crate::extract::job::ExtractParams { wing },
                )
                .await
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

        if let Err(crate::Error::LeaseLost { .. }) = &outcome {
            // Another daemon owns this job now. Marking it `Failed` here
            // would overwrite the new owner's record with a stale verdict,
            // so drop everything and let it run its course there.
            warn!(job_id = %job.id, "lease lost while running; abandoning this run");
            self.controls.remove(&job.id);
            return;
        }

        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let event = match outcome {
            Ok(JobOutcome::Completed) => {
                info!(elapsed_ms, "job completed");
                JobEvent::Complete
            }
            Ok(JobOutcome::Paused) => {
                info!(
                    elapsed_ms,
                    interrupted = control.was_interrupted(),
                    "job paused"
                );
                JobEvent::Pause
            }
            Ok(JobOutcome::Cancelled) => {
                info!(elapsed_ms, "job cancelled");
                JobEvent::Cancel
            }
            Err(error) => {
                warn!(job_id = %job.id, %error, elapsed_ms, attempt = job.attempt, "job failed");
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
        // Fenced like every other write of a leased run: if the lease lapsed
        // and the job was reaped, this final state is stale and is dropped.
        match self.store.save_job_fenced(&job, &self.worker).await {
            Ok(true) => self.announce(Action::Updated, &job),
            Ok(false) => warn!(
                job_id = %job.id,
                "lease lost before the final state could be saved; dropping it"
            ),
            Err(error) => warn!(job_id = %job.id, %error, "failed to persist final job state"),
        }
        // Keep the abort handle registered through the final write: a force
        // request in that window must not mistake a still-running task for an orphan.
        self.controls.remove(&job.id);
        // After the final state is saved, so the sweep sees the finished
        // job's drawers and never races its last write.
        if event == JobEvent::Complete && kind_wrote_drawers {
            self.ensure_embedding_sweep().await;
        }
        // A mining job files drawers with an origin, which (with notes, queued
        // by `app::palace` when one is captured) is all the extraction sweep
        // reads.
        if event == JobEvent::Complete && matches!(job.kind, JobKind::Mine { .. }) {
            self.ensure_extraction_sweep().await;
        }
    }
}

/// The notice that `job` changed: its id, kind and status, and nothing of its input or progress line.
fn job_event(action: Action, job: &Job) -> Event {
    Event::job(action, job.id, kind_name(&job.kind), job.status)
}

/// The kinds that can sustain heavy acquisition, index writes and provider work.
fn is_background(kind: &JobKind) -> bool {
    matches!(
        kind,
        JobKind::Mine { .. } | JobKind::Embed { .. } | JobKind::Extract { .. }
    )
}

/// The stable `snake_case` name of a job's kind, for log fields. Never
/// includes parameters: those can hold paths or memory content.
fn kind_name(kind: &JobKind) -> &'static str {
    match kind {
        JobKind::Demo { .. } => "demo",
        JobKind::Mine { .. } => "mine",
        JobKind::Checkpoint { .. } => "checkpoint",
        JobKind::Audit { .. } => "audit",
        JobKind::Repair { .. } => "repair",
        JobKind::Embed { .. } => "embed",
        JobKind::Extract { .. } => "extract",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::JobStatus;

    fn mining_kind() -> JobKind {
        JobKind::Mine {
            source: crate::domain::MiningSource::Directory {
                path: "/tmp/memcastle-force-test".into(),
            },
            wing: None,
            full: false,
            options: Default::default(),
        }
    }

    #[tokio::test]
    async fn forcing_a_local_mine_aborts_its_worker_and_fences_future_writes() {
        let scheduler = scheduler().await;
        let submitted = scheduler
            .submit(mining_kind(), Priority::Background, "test")
            .await
            .unwrap();
        let running = scheduler
            .store
            .claim_next_job(&scheduler.worker, chrono::Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        let permit = Arc::clone(&scheduler.semaphore)
            .try_acquire_owned()
            .unwrap();
        let control = JobControl::default();
        let handle = tokio::spawn(async move {
            let _permit = permit;
            std::future::pending::<()>().await;
        });
        *control.worker.lock().await = Some(handle);
        scheduler.controls.insert(running.id, control);

        scheduler.force_cancel(submitted.id).await.unwrap();

        let cancelled = reload(&scheduler, &submitted).await;
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(!cancelled.cancel_requested);
        assert!(cancelled.lease_owner.is_none());
        assert!(
            !scheduler
                .store
                .save_job_fenced(&running, &scheduler.worker)
                .await
                .unwrap()
        );
        assert!(scheduler.semaphore.try_acquire().is_ok());
    }

    #[tokio::test]
    async fn force_cancel_refuses_a_remote_lease_without_changing_the_job() {
        let scheduler = scheduler().await;
        let submitted = scheduler
            .submit(mining_kind(), Priority::Background, "test")
            .await
            .unwrap();
        scheduler
            .store
            .claim_next_job("another-daemon", chrono::Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();

        assert!(scheduler.force_cancel(submitted.id).await.is_err());
        let unchanged = reload(&scheduler, &submitted).await;
        assert_eq!(unchanged.status, JobStatus::Running);
        assert!(!unchanged.cancel_requested);
    }

    #[tokio::test]
    async fn force_cancel_refuses_non_mining_and_finished_jobs() {
        let scheduler = scheduler().await;
        let mine = scheduler
            .submit(mining_kind(), Priority::Background, "test")
            .await
            .unwrap();
        let demo = scheduler
            .submit(JobKind::Demo { steps: 1 }, Priority::Normal, "test")
            .await
            .unwrap();
        assert!(scheduler.force_cancel(mine.id).await.is_err());
        assert!(scheduler.force_cancel(demo.id).await.is_err());
        assert_eq!(reload(&scheduler, &mine).await.status, JobStatus::Queued);
    }

    #[tokio::test]
    async fn no_embedding_sweep_is_queued_without_a_provider() {
        let scheduler = scheduler().await;
        scheduler.ensure_embedding_sweep().await;
        assert_eq!(scheduler.store.count_jobs(None).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn repeated_writes_queue_a_single_waiting_embedding_sweep() {
        let scheduler = scheduler()
            .await
            .with_embeddings(crate::embed::Embeddings::new(
                crate::embed::fake::WordHashEmbedder,
                4,
            ));
        for _ in 0..5 {
            scheduler.ensure_embedding_sweep().await;
        }
        let queued = scheduler
            .store
            .list_jobs(Some(JobStatus::Queued))
            .await
            .unwrap();
        assert_eq!(
            queued.len(),
            1,
            "writes must coalesce into one waiting sweep"
        );
        assert!(matches!(queued[0].kind, JobKind::Embed { wing: None }));
        assert_eq!(queued[0].priority, Priority::Background);
    }

    #[tokio::test]
    async fn no_extraction_sweep_is_queued_without_a_provider() {
        let scheduler = scheduler().await;
        scheduler.ensure_extraction_sweep().await;
        assert_eq!(scheduler.store.count_jobs(None).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn repeated_mines_queue_a_single_waiting_extraction_sweep() {
        let scheduler = scheduler().await.with_extraction(Extraction::new(
            crate::extract::heuristic::HeuristicExtractor,
            &crate::config::ExtractionConfig::default(),
        ));
        for _ in 0..5 {
            scheduler.ensure_extraction_sweep().await;
        }
        let queued = scheduler
            .store
            .list_jobs(Some(JobStatus::Queued))
            .await
            .unwrap();
        assert_eq!(
            queued.len(),
            1,
            "mines must coalesce into one waiting sweep"
        );
        assert!(matches!(queued[0].kind, JobKind::Extract { wing: None }));
        assert_eq!(queued[0].priority, Priority::Background);
    }

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
            .claim_next_job("test-worker", chrono::Duration::seconds(30))
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
        // A handler that never checks for pause (one stuck inside a single long unit of work) must not
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
    async fn pausing_a_job_that_is_not_running_is_a_transition_invalid_not_a_missing_job() {
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
                .claim_next_job("test-worker", chrono::Duration::seconds(30))
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
                .claim_next_job("test-worker", chrono::Duration::seconds(30))
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

    #[tokio::test]
    async fn the_drain_waits_no_longer_than_the_configured_timeout() {
        let scheduler = Arc::new(
            Scheduler::new(SurrealStore::connect_memory_for_tests().await, 1)
                .with_drain_timeout(Duration::from_millis(100)),
        );
        // A worker that never lets go of its permit, like a stuck handler.
        let _held = Arc::clone(&scheduler.semaphore)
            .try_acquire_owned()
            .expect("the only permit is free");
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        let started = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(5), Arc::clone(&scheduler).run(shutdown))
            .await
            .expect("run must give up at the configured drain timeout");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the 100ms setting, not the 10s default, must bound the drain"
        );
    }

    #[tokio::test]
    async fn a_checkpoint_racing_a_stop_request_neither_fails_nor_loses_the_request() {
        // SurrealDB fails one of two transactions writing the same record at
        // once with a retryable conflict: the worker's checkpoint and the
        // user's request are exactly that pair.
        let scheduler = Arc::new(scheduler().await);
        let job = seed(&scheduler, JobStatus::Running, 1).await;

        let saver = {
            let (scheduler, job) = (Arc::clone(&scheduler), job.clone());
            tokio::spawn(async move {
                for _ in 0..200 {
                    scheduler.store.save_job(&job).await?;
                }
                Ok::<_, crate::Error>(())
            })
        };
        let marker = {
            let (scheduler, id) = (Arc::clone(&scheduler), job.id);
            tokio::spawn(async move {
                for _ in 0..200 {
                    scheduler.store.mark_pause_requested(id).await?;
                }
                Ok::<_, crate::Error>(())
            })
        };

        saver.await.unwrap().expect("checkpoints must not fail");
        marker.await.unwrap().expect("requests must not fail");
        assert!(reload(&scheduler, &job).await.pause_requested);
    }

    /// A second daemon over the same store, as a remote backend allows.
    fn second_daemon(first: &Scheduler) -> Scheduler {
        Scheduler::new(first.store.clone(), 1).with_shared_store()
    }

    async fn submit_demo(scheduler: &Scheduler) -> Job {
        scheduler
            .submit(JobKind::Demo { steps: 1 }, Priority::Normal, "test")
            .await
            .unwrap()
    }

    /// Collects everything logged on this thread, so a test can read it.
    #[derive(Clone, Default)]
    struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn a_job_leaves_a_queued_claimed_started_completed_trail_naming_its_id() {
        let buffer = LogBuffer::default();
        // Thread-local default is enough: `#[tokio::test]` runs the spawned
        // worker on this same thread.
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_writer(buffer.clone())
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish(),
        );
        let scheduler = Arc::new(scheduler().await);
        let job = submit_demo(&scheduler).await;
        let shutdown = CancellationToken::new();
        let run = tokio::spawn(Arc::clone(&scheduler).run(shutdown.clone()));
        for _ in 0..200 {
            if reload(&scheduler, &job).await.status == JobStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        shutdown.cancel();
        run.await.unwrap();

        let log = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        for event in ["job queued", "job claimed", "job started", "job completed"] {
            assert!(log.contains(event), "missing `{event}` in: {log}");
        }
        assert!(log.contains(&job.id.to_string()), "{log}");
    }

    /// Every job event the bus holds right now.
    fn drain(receiver: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[tokio::test]
    async fn submitting_a_job_announces_it_by_id_kind_and_status_only() {
        let bus = EventBus::new();
        let mut heard = bus.subscribe();
        let scheduler = scheduler().await.with_events(bus);

        let job = submit_demo(&scheduler).await;

        assert_eq!(
            drain(&mut heard),
            vec![Event::job(Action::Created, job.id, "demo", "queued")]
        );
    }

    #[tokio::test]
    async fn a_job_announces_its_claim_every_progress_write_and_its_final_status() {
        let bus = EventBus::new();
        let mut heard = bus.subscribe();
        let scheduler = Arc::new(scheduler().await.with_events(bus));
        let job = scheduler
            .submit(JobKind::Demo { steps: 2 }, Priority::Normal, "test")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let run = tokio::spawn(Arc::clone(&scheduler).run(shutdown.clone()));
        for _ in 0..200 {
            if reload(&scheduler, &job).await.status == JobStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        shutdown.cancel();
        run.await.unwrap();

        let statuses: Vec<String> = drain(&mut heard)
            .into_iter()
            .map(|event| {
                assert_eq!(event.id.as_deref(), Some(job.id.to_string().as_str()));
                event.status.expect("a job event carries its status")
            })
            .collect();
        // Queued, claimed, one write per step, then the final state.
        assert_eq!(
            statuses,
            ["queued", "running", "running", "running", "completed"]
        );
    }

    #[tokio::test]
    async fn a_cancel_before_the_job_ran_is_announced() {
        let bus = EventBus::new();
        let scheduler = scheduler().await.with_events(bus.clone());
        let job = submit_demo(&scheduler).await;
        let mut heard = bus.subscribe();

        scheduler.request_cancel(job.id).await.unwrap();

        assert_eq!(
            drain(&mut heard),
            vec![Event::job(Action::Updated, job.id, "demo", "cancelled")]
        );
    }

    #[tokio::test]
    async fn two_daemons_racing_for_the_queue_never_claim_the_same_job() {
        let first = scheduler().await;
        let second = second_daemon(&first);
        for _ in 0..20 {
            submit_demo(&first).await;
        }
        let ttl = chrono::Duration::seconds(30);

        let claim_all = |store: SurrealStore, worker: &'static str| async move {
            let mut mine = Vec::new();
            while let Some(job) = store.claim_next_job(worker, ttl).await.unwrap() {
                mine.push(job.id);
            }
            mine
        };
        let (a, b) = tokio::join!(
            claim_all(first.store.clone(), "daemon-a"),
            claim_all(second.store.clone(), "daemon-b"),
        );

        let mut all: Vec<_> = a.iter().chain(&b).copied().collect();
        assert_eq!(all.len(), 20, "every job is claimed exactly once");
        all.sort_by_key(|id| id.to_string());
        all.dedup();
        assert_eq!(all.len(), 20, "no job may be claimed by both daemons");
    }

    #[tokio::test]
    async fn a_second_daemon_cannot_steal_a_job_whose_lease_is_live() {
        let first = scheduler().await;
        let job = submit_demo(&first).await;
        first
            .store
            .claim_next_job("daemon-a", chrono::Duration::seconds(30))
            .await
            .unwrap()
            .expect("claimed");

        second_daemon(&first).recover().await.unwrap();

        let after = reload(&first, &job).await;
        assert_eq!(
            after.status,
            JobStatus::Running,
            "a live daemon's job is not abandoned"
        );
        assert_eq!(after.lease_owner.as_deref(), Some("daemon-a"));
        assert_eq!(after.recovery_attempts, 0);
    }

    #[tokio::test]
    async fn a_second_daemon_reclaims_a_job_once_its_lease_has_expired() {
        let first = scheduler().await;
        let job = submit_demo(&first).await;
        // Already expired: the owner never renewed.
        first
            .store
            .claim_next_job("daemon-a", chrono::Duration::seconds(-1))
            .await
            .unwrap()
            .expect("claimed");

        second_daemon(&first).recover().await.unwrap();

        let after = reload(&first, &job).await;
        assert_eq!(after.status, JobStatus::Queued);
        assert_eq!(after.recovery_attempts, 1, "a lapsed lease is a crash");
        assert_eq!(after.lease_owner, None);
    }

    #[tokio::test]
    async fn a_shared_store_recovers_a_running_job_that_never_had_a_lease() {
        let first = scheduler().await;
        // Written by a version from before leases existed.
        let job = seed(&first, JobStatus::Running, 1).await;

        second_daemon(&first).recover().await.unwrap();

        assert_eq!(reload(&first, &job).await.status, JobStatus::Queued);
    }

    #[tokio::test]
    async fn a_worker_whose_job_was_reaped_and_reclaimed_cannot_write_it_any_more() {
        let first = scheduler().await;
        let job = submit_demo(&first).await;
        let stale = first
            .store
            .claim_next_job("daemon-a", chrono::Duration::seconds(-1))
            .await
            .unwrap()
            .expect("claimed");
        // Another daemon reaps it and takes it over.
        second_daemon(&first).recover().await.unwrap();
        let taken = first
            .store
            .claim_next_job("daemon-b", chrono::Duration::seconds(30))
            .await
            .unwrap()
            .expect("re-claimed");
        assert_eq!(taken.id, job.id);

        // The stale worker wakes up and tries to checkpoint.
        let ctx = JobContext::new(job.id, JobControl::default(), first.store.clone())
            .with_lease("daemon-a");
        let mut stale = stale;
        let error = ctx
            .checkpoint(
                &mut stale,
                crate::domain::JobProgress::default(),
                serde_json::json!({ "next_step": 99 }),
            )
            .await
            .expect_err("a fenced checkpoint must be refused");

        assert!(matches!(error, crate::Error::LeaseLost { .. }), "{error:?}");
        let after = reload(&first, &job).await;
        assert_eq!(after.lease_owner.as_deref(), Some("daemon-b"));
        assert_ne!(after.checkpoint, serde_json::json!({ "next_step": 99 }));
    }

    #[tokio::test]
    async fn the_heartbeat_keeps_a_running_jobs_lease_from_expiring() {
        let scheduler = Arc::new(
            Scheduler::new(SurrealStore::connect_memory_for_tests().await, 1)
                .with_lease_ttl(Duration::from_millis(600)),
        );
        let job = scheduler
            .submit(JobKind::Demo { steps: 200 }, Priority::Normal, "test")
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(Arc::clone(&scheduler).run(shutdown.clone()));

        // Wait until it is running, then note its lease and let several TTLs pass.
        let mut first_expiry = None;
        for _ in 0..100 {
            let current = reload(&scheduler, &job).await;
            if current.status == JobStatus::Running {
                first_expiry = current.lease_expires_at;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let first_expiry = first_expiry.expect("the job started");
        tokio::time::sleep(Duration::from_millis(1500)).await;

        let after = reload(&scheduler, &job).await;
        assert_eq!(
            after.status,
            JobStatus::Running,
            "still running, not reaped"
        );
        assert!(
            after.lease_expires_at > Some(first_expiry),
            "the lease must have been renewed past {first_expiry}: {:?}",
            after.lease_expires_at
        );
        assert!(
            after.lease_expires_at > Some(chrono::Utc::now()),
            "and must be live now"
        );
        shutdown.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn a_daemon_that_finds_its_lease_gone_stops_the_job() {
        let scheduler = scheduler().await;
        // The store says another daemon owns this job.
        let job = submit_demo(&scheduler).await;
        scheduler
            .store
            .claim_next_job("someone-else", chrono::Duration::seconds(30))
            .await
            .unwrap()
            .expect("claimed");
        // ...but this daemon still believes it is running it.
        let control = JobControl::default();
        scheduler.controls.insert(job.id, control.clone());

        scheduler.heartbeat().await;

        assert!(
            control.was_interrupted(),
            "a job we no longer own must be stopped"
        );
    }

    #[tokio::test]
    async fn the_reaper_leaves_a_job_this_daemon_is_still_running_alone() {
        let scheduler = scheduler().await;
        let job = submit_demo(&scheduler).await;
        scheduler
            .store
            .claim_next_job(&scheduler.worker, chrono::Duration::seconds(-1))
            .await
            .unwrap()
            .expect("claimed");
        // Expired on paper because a beat was late, but it is ours and live.
        scheduler.controls.insert(job.id, JobControl::default());

        scheduler.reap_expired_leases().await.unwrap();

        assert_eq!(reload(&scheduler, &job).await.status, JobStatus::Running);
    }
}
