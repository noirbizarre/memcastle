//! The `Demo` job kind: a synthetic, side-effect-free workload that
//! exercises checkpointing, pause/resume, and cancellation without touching
//! real storage content — see `domain::job::JobKind::Demo`'s doc comment.

use std::time::Duration;

use serde_json::json;

use crate::domain::{Job, JobProgress};
use crate::error::Result;
use crate::store::SurrealStore;

use super::{JobContext, JobOutcome};

/// How long each simulated step takes — long enough to make pause/cancel
/// observable in a manual smoke test, short enough not to slow down the
/// test suite.
const STEP_DELAY: Duration = Duration::from_millis(150);

pub(super) async fn run(
    _store: &SurrealStore,
    ctx: &JobContext,
    job: &mut Job,
    steps: u32,
) -> Result<JobOutcome> {
    // Resume from wherever the last checkpoint left off, rather than
    // restarting at zero — this is what makes `Paused -> Queued -> Running`
    // a real resumption instead of a silent do-over.
    let start = job
        .checkpoint
        .get("next_step")
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| n as u32);

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
