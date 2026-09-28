//! End-to-end coverage of `JobKind::Audit` over real HTTP: submission via
//! the generic `POST /api/jobs` endpoint, and reading the report back off
//! `Job.result` via `GET /api/jobs/{id}` — see `memcastle::audit`'s module
//! doc for what the report checks.

mod common;

use common::TestDaemon;
use memcastle::domain::{Job, JobId, JobStatus};

#[tokio::test]
async fn a_fresh_palaces_audit_completes_with_a_zero_finding_result() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let submitted: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({
            "type": "audit",
            "requested_by": "test",
        }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    let job = wait_for_job_status(
        &client,
        &daemon.base_url,
        submitted.id,
        JobStatus::Completed,
    )
    .await;

    let result = job
        .result
        .expect("a completed audit job must set Job.result");
    assert_eq!(result["orphan_drawers"], serde_json::json!([]));
    assert_eq!(result["dangling_provenance_drawers"], serde_json::json!([]));
    assert_eq!(result["stuck_failed_jobs"], 0);
    assert_eq!(result["running_jobs"], 0);
    assert_eq!(result["drawers_without_embedding"], 0);
    assert_eq!(result["total_drawers_in_scope"], 0);

    daemon.shutdown().await;
}

async fn get_job(client: &reqwest::Client, base_url: &str, id: JobId) -> Job {
    client
        .get(format!("{base_url}/api/jobs/{id}"))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

/// Poll rather than sleep a fixed duration — same convention as
/// `tests/server.rs`'s identical helper (each integration test binary
/// keeps its own copy; see `tests/common/mod.rs`'s module doc on why that
/// duplication is accepted here).
async fn wait_for_job_status(
    client: &reqwest::Client,
    base_url: &str,
    id: JobId,
    status: JobStatus,
) -> Job {
    for _ in 0..300 {
        let job = get_job(client, base_url, id).await;
        if job.status == status {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("job {id} did not reach {status:?} within 30s");
}
