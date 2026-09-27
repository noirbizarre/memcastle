//! The durable job model.
//!
//! A job is the unit the scheduler (`crate::jobs`) claims, runs, checkpoints,
//! and persists through `store`. This module only defines the *shape* and
//! the *legal transitions* — claiming, leasing, and execution are the
//! scheduler's concern, not the domain's.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::JobId;

/// A job's place in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting to be claimed by a worker.
    Queued,
    /// Claimed and actively executing.
    Running,
    /// Cooperatively paused mid-execution; resumable from `checkpoint`.
    Paused,
    /// Finished successfully. Terminal.
    Completed,
    /// Finished with an unrecoverable error. Terminal.
    Failed,
    /// Withdrawn before or during execution. Terminal.
    Cancelled,
}

/// What kind of work a job performs, and its parameters.
///
/// `serde(tag = "type")` gives each variant a stable, greppable name in
/// persisted records and over the API, independent of Rust's own enum
/// representation. Tagged `"type"` rather than `"kind"` so the persisted
/// shape isn't `kind: { kind: "demo", .. }` — [`Job::kind`] is already named
/// for what this enum *is*, this tag is for what variant it *is in*.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobKind {
    /// A synthetic job with no side effects, used to exercise the scheduler
    /// (checkpointing, pause/resume, cancellation) without touching storage.
    Demo {
        /// How many discrete steps to simulate.
        steps: u32,
    },
    /// Mine a directory on disk into drawers.
    Mine {
        /// The directory to walk.
        path: PathBuf,
        /// The wing to file mined drawers under (defaults to the directory name).
        wing: Option<String>,
    },
}

/// A snapshot of how far along a job is.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobProgress {
    /// Units of work completed so far.
    pub current: u32,
    /// Total units of work, when known up front.
    pub total: Option<u32>,
    /// A short human-readable status line.
    pub message: Option<String>,
}

/// An event that requests a status change.
///
/// Kept distinct from [`JobStatus`] itself: `Resume` and `Retry` both land on
/// `Queued`, but they are different *requests* with different preconditions,
/// and `Job::apply` needs to see the request, not just the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobEvent {
    /// A worker has claimed the job and begun execution.
    Claim,
    /// The handler is cooperatively pausing itself.
    Pause,
    /// A caller has asked for a paused job to run again.
    Resume,
    /// The handler finished all its work.
    Complete,
    /// The handler hit an unrecoverable error.
    Fail,
    /// A caller (or crash recovery) is withdrawing the job.
    Cancel,
    /// A caller has asked for a `Failed` job to run again. Distinct from
    /// `Resume` even though both land on `Queued`: `Retry` additionally
    /// clears `error` (see `Job::apply`), which `Resume` must never do
    /// since a `Paused` job was never in an error state to begin with.
    Retry,
    /// Crash recovery is re-queuing a job that was `Running` when the
    /// daemon last stopped, because its attempt budget is not exhausted.
    RecoverToQueued,
}

/// A transition was requested that the state machine does not allow.
#[derive(Debug, Clone, thiserror::Error)]
#[error("job {id} cannot go from {from:?} to {event:?}")]
pub struct TransitionError {
    /// The job that rejected the transition.
    pub id: JobId,
    /// The status it was in.
    pub from: JobStatus,
    /// The event that was rejected.
    pub event: JobEvent,
}

/// A job's scheduling priority — coarse buckets rather than an arbitrary
/// integer, so callers can't invent incomparable numeric scales.
///
/// Declared ascending so the derived `Ord` gives `Critical` the highest
/// rank without a manual impl. Serializes as its underlying `i32`, not the
/// variant name (`#[serde(into/try_from = "i32")]`) — the store's
/// `job.priority` column is `TYPE int` and takes no migration, so the wire
/// shape has to match exactly what it already writes today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "i32", try_from = "i32")]
pub enum Priority {
    /// Routine background work (mining). Runs only once nothing else is queued.
    Background,
    /// Below-normal work with no urgency.
    Low,
    /// The default for anything without a stronger opinion.
    Normal,
    /// Above-normal work that should generally preempt background jobs.
    High,
    /// Time-sensitive work that must preempt everything else (e.g. an
    /// emergency checkpoint before a crash).
    Critical,
}

impl From<Priority> for i32 {
    fn from(priority: Priority) -> Self {
        match priority {
            Priority::Critical => 100,
            Priority::High => 75,
            Priority::Normal => 50,
            Priority::Low => 25,
            Priority::Background => 0,
        }
    }
}

/// A raw priority value read back from storage that doesn't match one of
/// [`Priority`]'s fixed levels — surfaces a corrupted `job.priority` column
/// instead of silently coercing it to some default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not a valid job priority")]
pub struct InvalidPriority(pub i32);

impl TryFrom<i32> for Priority {
    type Error = InvalidPriority;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            100 => Ok(Self::Critical),
            75 => Ok(Self::High),
            50 => Ok(Self::Normal),
            25 => Ok(Self::Low),
            0 => Ok(Self::Background),
            other => Err(InvalidPriority(other)),
        }
    }
}

/// A durable unit of work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Unique identifier.
    pub id: JobId,
    /// What this job does.
    pub kind: JobKind,
    /// Current lifecycle status.
    pub status: JobStatus,
    /// Higher runs first, among otherwise-equal jobs.
    pub priority: Priority,
    /// When the job was submitted.
    pub created_at: DateTime<Utc>,
    /// When a worker first claimed it.
    pub started_at: Option<DateTime<Utc>>,
    /// When it reached a terminal status.
    pub completed_at: Option<DateTime<Utc>>,
    /// Who asked for this (`"cli"`, `"mcp:<client>"`, `"http"`).
    pub requested_by: String,
    /// How far along it is.
    pub progress: JobProgress,
    /// How many times a worker has claimed this job.
    pub attempt: u32,
    /// The attempt budget — beyond this, a crash-recovered `Running` job
    /// goes to `Failed` instead of back to `Queued`.
    pub max_attempts: u32,
    /// Opaque, handler-defined resumption state.
    pub checkpoint: Value,
    /// The terminal error, when `status == Failed`.
    pub error: Option<String>,
    /// The scheduler instance currently holding this job, if `Running`.
    pub lease_owner: Option<String>,
    /// When the current lease is considered stale (crash-recovery threshold).
    pub lease_expires_at: Option<DateTime<Utc>>,
}

impl Job {
    /// Construct a freshly submitted, `Queued` job.
    #[must_use]
    pub fn new(kind: JobKind, priority: Priority, requested_by: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: JobId::new(),
            kind,
            status: JobStatus::Queued,
            priority,
            created_at: now,
            started_at: None,
            completed_at: None,
            requested_by: requested_by.into(),
            progress: JobProgress::default(),
            attempt: 0,
            max_attempts: 3,
            // An empty object, not `Value::Null`: the store schema requires
            // `checkpoint` to always be an object (handlers read specific
            // keys out of it with `.get(...)`, which works identically
            // either way, but `TYPE object` — not `option<object>` — is
            // what lets a handler's own `DEFINE FIELD checkpoint.foo` ever
            // be added later without an `option` wrapper in the way).
            checkpoint: serde_json::json!({}),
            error: None,
            lease_owner: None,
            lease_expires_at: None,
        }
    }

    /// Attempt a status transition, mutating `self` on success.
    ///
    /// This is the *only* place `status` is allowed to change — every other
    /// module goes through it, so "what transitions exist" stays answerable
    /// by reading one `match` instead of auditing every call site.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] if `event` is not legal from the current
    /// status.
    pub fn apply(&mut self, event: JobEvent) -> Result<(), TransitionError> {
        use JobEvent::{Cancel, Claim, Complete, Fail, Pause, RecoverToQueued, Resume, Retry};
        use JobStatus::{Cancelled, Completed, Failed, Paused, Queued, Running};

        let next = match (self.status, event) {
            (Queued, Claim) => Running,
            (Running, Pause) => Paused,
            (Paused, Resume) => Queued,
            (Running, Complete) => Completed,
            (Running, Fail) => Failed,
            (Queued, Cancel) | (Paused, Cancel) | (Running, Cancel) => Cancelled,
            (Running, RecoverToQueued) => Queued,
            (Failed, Retry) => Queued,
            (from, event) => {
                return Err(TransitionError {
                    id: self.id,
                    from,
                    event,
                });
            }
        };

        let now = Utc::now();
        if next == Running {
            self.started_at.get_or_insert(now);
        }
        if matches!(next, Completed | Failed | Cancelled) {
            self.completed_at = Some(now);
        }
        if event == Retry {
            // Retry's whole point is a clean second attempt; a stale
            // terminal error must not survive the transition back to
            // Queued. Centralized here (not left to the caller) for the
            // same reason `completed_at` is set here rather than by every
            // caller of `Complete`/`Fail`/`Cancel`: one place to get right.
            self.error = None;
        }
        self.status = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_job() -> Job {
        Job::new(JobKind::Demo { steps: 3 }, Priority::Normal, "test")
    }

    #[test]
    fn a_queued_job_can_be_claimed() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        assert_eq!(job.status, JobStatus::Running);
        assert!(job.started_at.is_some());
    }

    #[test]
    fn a_running_job_can_be_paused_and_resumed() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        job.apply(JobEvent::Pause).unwrap();
        assert_eq!(job.status, JobStatus::Paused);
        job.apply(JobEvent::Resume).unwrap();
        assert_eq!(job.status, JobStatus::Queued);
    }

    #[test]
    fn a_queued_job_can_be_cancelled() {
        let mut job = demo_job();
        job.apply(JobEvent::Cancel).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn a_paused_job_can_be_cancelled() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        job.apply(JobEvent::Pause).unwrap();
        job.apply(JobEvent::Cancel).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[test]
    fn completing_a_job_that_was_never_claimed_is_rejected() {
        let mut job = demo_job();
        let err = job.apply(JobEvent::Complete).unwrap_err();
        assert_eq!(err.from, JobStatus::Queued);
        assert_eq!(
            job.status,
            JobStatus::Queued,
            "rejected transition must not mutate state"
        );
    }

    #[test]
    fn a_completed_job_cannot_be_cancelled() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        job.apply(JobEvent::Complete).unwrap();
        assert!(job.apply(JobEvent::Cancel).is_err());
    }

    #[test]
    fn crash_recovery_requeues_a_running_job() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        job.apply(JobEvent::RecoverToQueued).unwrap();
        assert_eq!(job.status, JobStatus::Queued);
    }

    #[test]
    fn a_failed_job_can_be_retried_to_queued() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        job.error = Some("boom".to_string());
        job.checkpoint = serde_json::json!({ "next_step": 2 });
        job.apply(JobEvent::Fail).unwrap();
        assert_eq!(job.status, JobStatus::Failed);

        job.apply(JobEvent::Retry).unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        assert_eq!(job.error, None, "retry must clear the terminal error");
        assert_eq!(
            job.checkpoint,
            serde_json::json!({ "next_step": 2 }),
            "retry must preserve checkpoint so work already done isn't redone"
        );
    }

    #[test]
    fn retrying_a_job_that_was_never_run_is_rejected() {
        let mut job = demo_job();
        let err = job.apply(JobEvent::Retry).unwrap_err();
        assert_eq!(err.from, JobStatus::Queued);
        assert_eq!(
            job.status,
            JobStatus::Queued,
            "rejected transition must not mutate state"
        );
    }

    #[test]
    fn retrying_a_running_job_is_rejected() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        let err = job.apply(JobEvent::Retry).unwrap_err();
        assert_eq!(err.from, JobStatus::Running);
        assert_eq!(job.status, JobStatus::Running);
    }

    #[test]
    fn retrying_a_completed_job_is_rejected() {
        let mut job = demo_job();
        job.apply(JobEvent::Claim).unwrap();
        job.apply(JobEvent::Complete).unwrap();
        let err = job.apply(JobEvent::Retry).unwrap_err();
        assert_eq!(err.from, JobStatus::Completed);
        assert_eq!(job.status, JobStatus::Completed);
    }

    #[test]
    fn priority_round_trips_through_its_i32_mapping() {
        for priority in [
            Priority::Critical,
            Priority::High,
            Priority::Normal,
            Priority::Low,
            Priority::Background,
        ] {
            let value = i32::from(priority);
            assert_eq!(Priority::try_from(value), Ok(priority));
        }
    }

    #[test]
    fn priority_ordering_is_critical_high_normal_low_background() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
        assert!(Priority::Low > Priority::Background);
    }

    #[test]
    fn an_out_of_range_priority_value_is_rejected() {
        let err = Priority::try_from(42).unwrap_err();
        assert_eq!(err, InvalidPriority(42));
    }
}
