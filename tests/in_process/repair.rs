//! End-to-end coverage of `JobKind::Repair` over real HTTP: submission via
//! the generic `POST /api/jobs` endpoint, and reading the report back off
//! `Job.result` via `GET /api/jobs/{id}` — see `memcastle::repair`'s module
//! doc for what this job does (and, deliberately, does not do).
//!
//! Fixtures that require bypassing normal write paths (a manufactured
//! orphan drawer) live as unit tests in `src/repair/mod.rs`'s own
//! `#[cfg(test)]` module, alongside direct `SurrealStore` access — the same
//! split `src/audit/mod.rs` and `tests/in_process/audit.rs` use.

use crate::common;

use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::{Job, JobStatus};

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

/// Submit a repair over HTTP, returning the raw response so a test can
/// assert on the status and error body.
async fn submit_repair_based_on(
    client: &reqwest::Client,
    base_url: &str,
    based_on_job: memcastle::domain::JobId,
) -> reqwest::Response {
    client
        .post(format!("{base_url}/api/jobs"))
        .json(&serde_json::json!({
            "type": "repair",
            "dry_run": true,
            "based_on_job": based_on_job,
            "requested_by": "test",
        }))
        .send()
        .await
        .expect("request")
}

async fn job_count(client: &reqwest::Client, base_url: &str) -> usize {
    let jobs: Vec<Job> = client
        .get(format!("{base_url}/api/jobs"))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    jobs.len()
}

#[tokio::test]
async fn a_repair_based_on_an_unknown_job_is_rejected_at_submission_and_creates_no_job() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let response =
        submit_repair_based_on(&client, &daemon.base_url, memcastle::domain::JobId::new()).await;

    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("json");
    assert_eq!(body["code"], "memcastle::repair::based_on_job_invalid");
    assert_eq!(
        job_count(&client, &daemon.base_url).await,
        0,
        "a rejected submission must not leave a job behind"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_repair_based_on_a_job_that_is_not_an_audit_is_rejected_at_submission() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let demo: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({ "type": "demo", "steps": 1, "requested_by": "test" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    let response = submit_repair_based_on(&client, &daemon.base_url, demo.id).await;

    assert_eq!(response.status(), 400);
    let body: serde_json::Value = response.json().await.expect("json");
    assert_eq!(body["code"], "memcastle::repair::based_on_job_invalid");
    assert_eq!(
        job_count(&client, &daemon.base_url).await,
        1,
        "only the demo job may exist"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_repair_based_on_a_completed_audit_is_accepted() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let audit: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({ "type": "audit", "requested_by": "test" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    wait_for_job_status(&client, &daemon.base_url, audit.id, JobStatus::Completed).await;

    let response = submit_repair_based_on(&client, &daemon.base_url, audit.id).await;

    assert!(response.status().is_success(), "{}", response.status());

    daemon.shutdown().await;
}
