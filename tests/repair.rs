//! End-to-end coverage of `JobKind::Repair` over real HTTP: submission via
//! the generic `POST /api/jobs` endpoint, and reading the report back off
//! `Job.result` via `GET /api/jobs/{id}` — see `memcastle::repair`'s module
//! doc for what this job does (and, deliberately, does not do).
//!
//! Fixtures that require bypassing normal write paths (a manufactured
//! orphan drawer) live as unit tests in `src/repair/mod.rs`'s own
//! `#[cfg(test)]` module, alongside direct `SurrealStore` access — the same
//! split `src/audit/mod.rs` and `tests/audit.rs` use.

mod common;

use common::TestDaemon;
use memcastle::domain::{Job, JobId, JobStatus};

#[tokio::test]
async fn a_fresh_palaces_dry_run_repair_completes_with_no_planned_actions() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let submitted: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({
            "type": "repair",
            "dry_run": true,
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
        .expect("a completed repair job must set Job.result");
    assert_eq!(result["dry_run"], serde_json::json!(true));
    assert_eq!(result["actions"], serde_json::json!([]));

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_repair_submitted_without_dry_run_over_http_is_a_dry_run() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({ "type": "repair", "requested_by": "test" }))
        .send()
        .await
        .expect("request");
    assert!(
        response.status().is_success(),
        "a repair without dry_run must be accepted, not rejected: {}",
        response.status()
    );
    let submitted: Job = response.json().await.expect("json");

    let job = wait_for_job_status(
        &client,
        &daemon.base_url,
        submitted.id,
        JobStatus::Completed,
    )
    .await;
    assert_eq!(
        job.result.expect("result")["dry_run"],
        serde_json::json!(true),
        "the default must be the non-destructive one"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_fresh_palaces_applied_repair_completes_with_no_actions_taken() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let submitted: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({
            "type": "repair",
            "dry_run": false,
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
        .expect("a completed repair job must set Job.result");
    assert_eq!(result["dry_run"], serde_json::json!(false));
    assert_eq!(result["actions"], serde_json::json!([]));

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
/// `tests/audit.rs`'s identical helper (each integration test binary keeps
/// its own copy; see `tests/common/mod.rs`'s module doc on why that
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
