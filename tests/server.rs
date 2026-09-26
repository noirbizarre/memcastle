//! Server boundary tests: start, health/status, graceful shutdown.

mod common;

use common::TestDaemon;

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
