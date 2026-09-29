//! Cooperative pause/cancel signalling between the scheduler and a running
//! handler.
//!
//! Deliberately not "kill the task": a handler only stops between discrete
//! units of work, of its own accord, after checkpointing — see
//! `docs/architecture.md` ("Pause and cancel are cooperative") for why pause
//! is cooperative rather than a process kill.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::domain::{Job, JobId, JobProgress};
use crate::error::Result;
use crate::store::SurrealStore;

/// The scheduler's handle onto one running job, used to request pause or
/// cancellation. Cheap to clone — every clone shares the same underlying
/// flags.
#[derive(Clone, Default)]
pub struct JobControl {
    cancel: CancellationToken,
    pause_requested: Arc<AtomicBool>,
    /// Set only when the *scheduler* (daemon shutdown) asked for the pause,
    /// as opposed to a user. Kept apart from `pause_requested` because the
    /// two must end differently: a user's pause leaves the job `Paused`
    /// until they resume it, while a shutdown's pause must put the job
    /// straight back in the queue so the next daemon picks it up unasked.
    interrupted: Arc<AtomicBool>,
}

impl JobControl {
    /// Ask the handler to pause at its next opportunity.
    ///
    /// A user's pause wins over a shutdown's: if the daemon was already
    /// interrupting this job, the request is now the user's, so the job ends
    /// `Paused` rather than being handed back to the queue.
    pub fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::Relaxed);
        self.interrupted.store(false, Ordering::Relaxed);
    }

    /// Ask the handler to stop at its next opportunity.
    pub fn request_cancel(&self) {
        self.cancel.cancel();
    }

    /// Ask the handler to stop at its next unit of work because the daemon
    /// is shutting down. Handlers see this as an ordinary pause request; the
    /// scheduler then re-queues the job instead of leaving it `Paused`.
    /// A pause a user already requested wins — they asked for it to stay
    /// paused, and a restart must not silently un-pause it.
    pub(crate) fn request_interrupt(&self) {
        if !self.pause_requested.swap(true, Ordering::Relaxed) {
            self.interrupted.store(true, Ordering::Relaxed);
        }
    }

    /// Whether the pause this control carries came from a daemon shutdown.
    pub(crate) fn was_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
    }
}

/// What a job handler is given to cooperate with the scheduler and persist
/// its own progress.
pub struct JobContext {
    job_id: JobId,
    control: JobControl,
    store: SurrealStore,
    /// The worker whose lease this run executes under, when it has one.
    /// With it, every write the handler makes is fenced: refused if the
    /// stored job has been leased to someone else since.
    lease_owner: Option<String>,
}

impl JobContext {
    /// Construct a context for `job_id`, backed by `control` and `store`.
    #[must_use]
    pub fn new(job_id: JobId, control: JobControl, store: SurrealStore) -> Self {
        Self {
            job_id,
            control,
            store,
            lease_owner: None,
        }
    }

    /// Fence this context's writes to `worker`'s lease: a checkpoint is
    /// refused with [`crate::Error::LeaseLost`] if another daemon has taken
    /// the job over, instead of overwriting the new owner's progress.
    #[must_use]
    pub fn with_lease(mut self, worker: impl Into<String>) -> Self {
        self.lease_owner = Some(worker.into());
        self
    }

    /// The job this context is executing.
    #[must_use]
    pub fn job_id(&self) -> JobId {
        self.job_id
    }

    /// The store this job runs against — handlers reach it through their
    /// context rather than taking it as a separate argument, so every handler
    /// has the same `run(ctx, job, params)` shape.
    #[must_use]
    pub fn store(&self) -> &SurrealStore {
        &self.store
    }

    /// Whether a pause has been requested. A handler should check this
    /// between discrete units of work (files mined, steps taken, ...).
    #[must_use]
    pub fn should_pause(&self) -> bool {
        self.control.pause_requested.load(Ordering::Relaxed)
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.control.cancel.is_cancelled()
    }

    /// Persist `job`'s current progress and checkpoint without changing its
    /// status — a handler calls this after every unit of work, so a crash
    /// mid-run loses at most one unit, not the whole job.
    ///
    /// # Errors
    ///
    /// Returns an error if the store write fails, or
    /// [`crate::Error::LeaseLost`] if this run's lease was taken over.
    pub async fn checkpoint(
        &self,
        job: &mut Job,
        progress: JobProgress,
        checkpoint: Value,
    ) -> Result<()> {
        job.progress = progress;
        job.checkpoint = checkpoint;
        match &self.lease_owner {
            None => self.store.save_job(job).await,
            Some(owner) => {
                if self.store.save_job_fenced(job, owner).await? {
                    Ok(())
                } else {
                    Err(crate::Error::LeaseLost {
                        id: job.id.to_string(),
                    })
                }
            }
        }
    }

    /// Checkpoint an index-based handler (mining, checkpoint): record that
    /// `index` of `total` units are done, with `{"next_index": index}` as the
    /// resume state and `"{verb} {index}/{total} {unit}"` as the progress
    /// line.
    ///
    /// Shared so every handler that walks a list agrees on the resume key —
    /// a handler that spelled it differently would silently restart from
    /// zero when resumed by the shared logic.
    ///
    /// # Errors
    ///
    /// Returns an error if the store write fails.
    pub async fn checkpoint_at(
        &self,
        job: &mut Job,
        index: usize,
        total: usize,
        verb: &str,
        unit: &str,
    ) -> Result<()> {
        let progress = JobProgress {
            // Saturating rather than wrapping: a count past `u32::MAX` must
            // read as "enormous", not as a small number.
            current: u32::try_from(index).unwrap_or(u32::MAX),
            total: Some(u32::try_from(total).unwrap_or(u32::MAX)),
            message: Some(format!("{verb} {index}/{total} {unit}")),
        };
        self.checkpoint(job, progress, serde_json::json!({ "next_index": index }))
            .await
    }
}
