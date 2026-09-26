//! Cooperative pause/cancel signalling between the scheduler and a running
//! handler.
//!
//! Deliberately not "kill the task": a handler only stops between discrete
//! units of work, of its own accord, after checkpointing — see the
//! `jobs` module doc and `domain::job`'s doc comment on why pause is
//! cooperative rather than a process kill.

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
}

impl JobControl {
    /// Ask the handler to pause at its next opportunity.
    pub fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::Relaxed);
    }

    /// Ask the handler to stop at its next opportunity.
    pub fn request_cancel(&self) {
        self.cancel.cancel();
    }
}

/// What a job handler is given to cooperate with the scheduler and persist
/// its own progress.
pub struct JobContext {
    job_id: JobId,
    control: JobControl,
    store: SurrealStore,
}

impl JobContext {
    /// Construct a context for `job_id`, backed by `control` and `store`.
    #[must_use]
    pub fn new(job_id: JobId, control: JobControl, store: SurrealStore) -> Self {
        Self {
            job_id,
            control,
            store,
        }
    }

    /// The job this context is executing.
    #[must_use]
    pub fn job_id(&self) -> JobId {
        self.job_id
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
    /// Returns an error if the store write fails.
    pub async fn checkpoint(
        &self,
        job: &mut Job,
        progress: JobProgress,
        checkpoint: Value,
    ) -> Result<()> {
        job.progress = progress;
        job.checkpoint = checkpoint;
        self.store.save_job(job).await
    }
}
