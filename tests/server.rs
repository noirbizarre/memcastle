//! Server boundary tests: start, health/status, graceful shutdown.

mod common;

use common::{TestDaemon, get_job, wait_for_job_status};
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

/// GET `path`, returning the status and the JSON body — for asserting on
/// the error contract (status class, diagnostic code, help), not just success.
async fn get_error(base_url: &str, path: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let response = reqwest::Client::new()
        .get(format!("{base_url}{path}"))
        .send()
        .await
        .expect("request");
    let status = response.status();
    (status, response.json().await.expect("json error body"))
}

#[tokio::test]
async fn a_malformed_job_id_is_a_400_with_its_own_diagnostic_code() {
    let daemon = TestDaemon::start().await;

    let (status, body) = get_error(&daemon.base_url, "/api/jobs/not-a-uuid").await;

    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "memcastle::jobs::invalid_id");
    assert!(
        body["help"].is_string(),
        "an error body must say what to do: {body}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_well_formed_id_no_job_has_is_a_404_not_found() {
    let daemon = TestDaemon::start().await;

    let (status, body) = get_error(&daemon.base_url, &format!("/api/jobs/{}", JobId::new())).await;

    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "memcastle::jobs::not_found");
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_status_filter_is_a_400_invalid_input() {
    let daemon = TestDaemon::start().await;

    let (status, body) = get_error(&daemon.base_url, "/api/jobs?status=bogus").await;

    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "memcastle::input::invalid");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_missing_query_parameter_is_a_400_with_the_shared_error_body() {
    let daemon = TestDaemon::start().await;

    // `/api/search` requires `q`; axum's stock extractor would answer with a
    // plain-text 400 that no client could read a code or help line from.
    let (status, body) = get_error(&daemon.base_url, "/api/search").await;

    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "memcastle::input::invalid");
    assert!(
        body["help"].is_string(),
        "an error body must say what to do: {body}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_malformed_job_submission_is_a_400_with_the_shared_error_body() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    for payload in [
        // An unknown job type, and a known one missing its required field.
        serde_json::json!({ "type": "no-such-kind" }),
        serde_json::json!({ "type": "demo" }),
        // A repair whose `based_on_job` is not a UUID.
        serde_json::json!({ "type": "repair", "based_on_job": "not-a-uuid" }),
    ] {
        let response = client
            .post(format!("{}/api/jobs", daemon.base_url))
            .json(&payload)
            .send()
            .await
            .expect("request");
        let status = response.status();
        let body: serde_json::Value = response.json().await.expect("json error body");

        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "{payload}: {body}"
        );
        assert_eq!(body["code"], "memcastle::input::invalid", "{payload}");
        assert!(body["help"].is_string(), "{payload}: {body}");
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn pausing_a_finished_job_is_a_400_invalid_transition_not_a_404() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let submitted: Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&serde_json::json!({ "type": "demo", "steps": 1, "requested_by": "test" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    wait_for_job_status(
        &client,
        &daemon.base_url,
        submitted.id,
        JobStatus::Completed,
    )
    .await;

    let response = client
        .post(format!(
            "{}/api/jobs/{}/pause",
            daemon.base_url, submitted.id
        ))
        .send()
        .await
        .expect("request");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = response.json().await.expect("json");
    assert_eq!(body["code"], "memcastle::jobs::invalid_transition");
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_client_reports_what_the_daemon_said_instead_of_blaming_the_connection() {
    let daemon = TestDaemon::start().await;
    let bind = daemon
        .base_url
        .trim_start_matches("http://")
        .parse()
        .unwrap();
    let client = memcastle::client::DaemonClient::discover(&daemon.palace_path, bind);

    let missing = client.get_job(JobId::new()).await.unwrap_err();

    match &missing {
        memcastle::Error::Remote {
            status, message, ..
        } => {
            assert_eq!(*status, 404);
            assert!(
                !message.contains("job job"),
                "the message must not be re-wrapped as if it were an id: {message}"
            );
        }
        other => panic!("expected the daemon's rejection, got {other:?}"),
    }
    let rendered = missing.to_string();
    assert!(rendered.contains("not found"), "{rendered}");
    assert!(
        !rendered.contains("not found not found"),
        "message must not be doubled: {rendered}"
    );

    // A pause on a finished job must come back as the daemon's 400, with the
    // daemon's own advice attached — not as "is the daemon running?".
    let job = client.demo(1).await.expect("submit");
    let http = reqwest::Client::new();
    wait_for_job_status(&http, &daemon.base_url, job.id, JobStatus::Completed).await;
    let error = client.pause_job(job.id).await.unwrap_err();
    assert!(
        matches!(&error, memcastle::Error::Remote { status: 400, .. }),
        "{error:?}"
    );
    daemon.shutdown().await;
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
