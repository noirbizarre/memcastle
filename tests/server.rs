//! Server boundary tests: start, health/status, graceful shutdown.

mod common;

use common::TestDaemon;
use memcastle::domain::{Job, JobId, JobStatus};

#[tokio::test]
async fn health_endpoint_reports_ok() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{}/api/health", daemon.base_url))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    assert_eq!(body["status"], "ok");

    daemon.shutdown().await;
}

#[tokio::test]
async fn status_endpoint_reports_an_empty_freshly_created_palace() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let status: memcastle::app::StatusReport = client
        .get(format!("{}/api/status", daemon.base_url))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    assert_eq!(status.drawer_count, 0);
    assert_eq!(status.jobs_queued, 0);
    assert_eq!(status.jobs_running, 0);

    daemon.shutdown().await;
}

#[tokio::test]
async fn shutdown_request_stops_the_listener_and_removes_the_registry_file() {
    let daemon = TestDaemon::start().await;
    let palace_path = daemon.palace_path.clone();
    assert!(memcastle::server::lifecycle::read_if_live(&palace_path).is_some());

    daemon.shutdown().await;

    assert!(memcastle::server::lifecycle::read_if_live(&palace_path).is_none());
}

#[tokio::test]
async fn a_failed_job_can_be_retried_over_http() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    // `Mine` against a path that can't exist fails synchronously
    // (`std::fs::canonicalize`), which is the only way to drive a job to
    // `Failed` over real HTTP without reaching into the store directly.
    let submitted: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({
            "type": "mine",
            "path": "/nonexistent/does-not-exist",
            "requested_by": "test",
        }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    let job = wait_for_job_status(&client, &daemon.base_url, submitted.id, JobStatus::Failed).await;
    assert!(job.error.is_some(), "a failed mine job must carry an error");
    assert_eq!(job.attempt, 1);

    let response = client
        .post(format!(
            "{}/api/jobs/{}/retry",
            daemon.base_url, submitted.id
        ))
        .send()
        .await
        .expect("request");
    assert!(
        response.status().is_success(),
        "retry should succeed on a Failed job: {}",
        response.status()
    );

    // The dispatch loop reclaims the retried job and fails it again almost
    // immediately (same bad path) — `attempt` reaching 2 only happens if
    // the job actually left `Failed` and returned to `Queued`, which is
    // exactly what `Scheduler::retry` -> `Job::apply(JobEvent::Retry)` is
    // responsible for. Poll for `attempt` and `status` together, not
    // sequentially: `attempt` is incremented at claim time, before the
    // handler runs, so a job can transiently be `Running` with the new
    // `attempt` already visible — asserting `status` right after only
    // `attempt` settles is a real race, not a hypothetical one (caught by
    // CI on a slower runner).
    let job =
        wait_for_job_failed_with_attempt_at_least(&client, &daemon.base_url, submitted.id, 2).await;
    assert_eq!(job.status, JobStatus::Failed);

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

/// Poll rather than sleep a fixed duration, matching the convention used
/// throughout `tests/concurrency.rs` — a fixed sleep only holds up on a
/// fast, idle machine.
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

/// Unlike [`wait_for_job_status`], checks `attempt` and `status` in the
/// same poll iteration — necessary because `attempt` is bumped at claim
/// time, before the handler runs, so it can be observed before `status`
/// has caught up to the terminal outcome of that attempt.
async fn wait_for_job_failed_with_attempt_at_least(
    client: &reqwest::Client,
    base_url: &str,
    id: JobId,
    attempt: u32,
) -> Job {
    for _ in 0..300 {
        let job = get_job(client, base_url, id).await;
        if job.attempt >= attempt && job.status == JobStatus::Failed {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("job {id} did not settle into Failed with attempt >= {attempt} within 30s");
}
