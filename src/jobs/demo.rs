//! The `Demo` job kind: a synthetic, side-effect-free workload that
//! exercises checkpointing, pause/resume, and cancellation without touching
//! real storage content — see `domain::job::JobKind::Demo`'s doc comment.

use std::time::Duration;

use serde_json::json;

use crate::domain::{Job, JobProgress};
use crate::error::Result;

use super::{JobContext, JobOutcome};

/// How long each simulated step takes — long enough to make pause/cancel
/// observable in a manual smoke test, short enough not to slow down the
/// test suite.
const STEP_DELAY: Duration = Duration::from_millis(150);

/// What a `Demo` job needs, gathered from its [`crate::domain::JobKind`].
pub struct DemoParams {
    /// How many simulated steps to take.
    pub steps: u32,
}

pub(super) async fn run(ctx: &JobContext, job: &mut Job, params: DemoParams) -> Result<JobOutcome> {
    let DemoParams { steps } = params;
    // Resume from wherever the last checkpoint left off, rather than
    // restarting at zero — this is what makes `Paused -> Queued -> Running`
    // a real resumption instead of a silent do-over.
    // Saturating, not truncating: a stored step past `u32::MAX` must end the
    // loop, not wrap around to a small number and redo work.
    let start = u32::try_from(JobContext::resume_index(job, "next_step")).unwrap_or(u32::MAX);

    for step in start..steps {
        if ctx.is_cancelled() {
            return Ok(JobOutcome::Cancelled);
        }
        if ctx.should_pause() {
            let progress = JobProgress {
                current: step,
                total: Some(steps),
                message: Some(format!("paused before step {step}")),
            };
            ctx.checkpoint(job, progress, json!({ "next_step": step }))
                .await?;
            return Ok(JobOutcome::Paused);
        }

        tokio::time::sleep(STEP_DELAY).await;

        let progress = JobProgress {
            current: step + 1,
            total: Some(steps),
            message: Some(format!("completed step {}/{steps}", step + 1)),
        };
        ctx.checkpoint(job, progress, json!({ "next_step": step + 1 }))
            .await?;
    }

    Ok(JobOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{JobKind, Priority};
    use crate::jobs::JobControl;
    use crate::store::SurrealStore;

    /// `{"next_step": N}` is persisted in job records already on disk, and
    /// is deliberately not `next_index` like the other handlers' key.
    #[tokio::test]
    async fn a_checkpoint_persisted_in_the_current_format_resumes_past_the_finished_steps() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let mut job = Job::new(JobKind::Demo { steps: 3 }, Priority::Normal, "test");
        job.checkpoint = json!({ "next_step": 2 });
        let ctx = JobContext::new(job.id, JobControl::default(), store);

        let outcome = run(&ctx, &mut job, DemoParams { steps: 3 })
            .await
            .expect("run");

        assert_eq!(outcome, JobOutcome::Completed);
        assert_eq!(job.progress.current, 3);
        assert_eq!(
            job.progress.message.as_deref(),
            Some("completed step 3/3"),
            "steps 1 and 2 were already done, so only step 3 may run"
        );
    }
}
