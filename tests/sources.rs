//! End-to-end source-driven mining against a real daemon: a source other than a plain directory is discovered,
//! read incrementally and mined with no agent and no model involved, through the same unified contract
//! (docs/adr/023) every source goes through.
//!
//! The fixture is a Pi session file; the daemon reads it from disk itself, which is the point of source-driven
//! mining.

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::{Job, JobStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};

const SESSION: &str = include_str!("fixtures/sources/pi/session.jsonl");
const MODE_HEADER: &str = "X-MemCastle-Mode";

/// Write a Pi session file under `root`, with a modification time `seconds` after the epoch so the cursor
/// ordering never depends on how fast the test runs.
fn write_session(root: &Path, content: &str, seconds: u64) {
    let dir = root.join("--home-me-project--");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("2026-07-14T14-27-12-546Z_019f6106.jsonl");
    std::fs::write(&file, content).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

async fn submit(client: &reqwest::Client, base: &str, body: Value) -> reqwest::Response {
    client
        .post(format!("{base}/api/jobs"))
        .json(&body)
        .send()
        .await
        .expect("request")
}

/// Mine the Pi sessions under `root` and wait for the job to complete.
async fn mine_pi(client: &reqwest::Client, base: &str, root: &Path, full: bool) -> Job {
    let response = submit(
        client,
        base,
        json!({
            "type": "mine", "provider": "pi-sessions", "locator": root, "full": full, "requested_by": "test",
        }),
    )
    .await;
    assert!(response.status().is_success(), "{}", response.status());
    let job: Job = response.json().await.expect("job");
    wait_for_job_status(client, base, job.id, JobStatus::Completed).await
}

async fn search(client: &reqwest::Client, base: &str, query: &[(&str, &str)]) -> Vec<Value> {
    client
        .get(format!("{base}/api/search"))
        .query(query)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

async fn sources(client: &reqwest::Client, base: &str) -> Value {
    client
        .get(format!("{base}/api/sources"))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn a_pi_session_is_discovered_read_and_mined_then_found_by_search_as_a_transcript() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let root = tempfile::tempdir().unwrap();
    write_session(root.path(), SESSION, 1_000);

    let job = mine_pi(&client, &daemon.base_url, root.path(), false).await;

    assert_eq!(job.result.as_ref().unwrap()["created"], 1);
    let hits = search(
        &client,
        &daemon.base_url,
        &[("q", "rotate signing keys"), ("source_kind", "transcript")],
    )
    .await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["source"]["kind"], "transcript");
    assert_eq!(hits[0]["source"]["origin"]["provider"], "pi-sessions");
    assert!(
        search(&client, &daemon.base_url, &[("q", "SECRET-TOOL-OUTPUT")])
            .await
            .is_empty(),
        "tool results are not filed"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn mining_a_source_again_reads_nothing_and_a_grown_session_adds_only_its_new_tail() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let root = tempfile::tempdir().unwrap();
    write_session(root.path(), SESSION, 1_000);
    mine_pi(&client, &daemon.base_url, root.path(), false).await;

    let second = mine_pi(&client, &daemon.base_url, root.path(), false).await;
    assert_eq!(
        second.result.as_ref().unwrap()["documents"],
        0,
        "the cursor is past the session"
    );

    let grown = format!(
        "{SESSION}{}\n",
        r#"{"type":"message","timestamp":"2026-07-14T15:00:00Z","message":{"role":"user","content":"what about certificate revocation?"}}"#
    );
    write_session(root.path(), &grown, 2_000);
    let third = mine_pi(&client, &daemon.base_url, root.path(), false).await;
    let summary = third.result.as_ref().unwrap();
    assert_eq!(
        summary["documents"], 1,
        "only the session that grew is read"
    );
    assert_eq!(
        summary["superseded"], 1,
        "its one chunk is replaced by the longer one"
    );

    let hits = search(
        &client,
        &daemon.base_url,
        &[("q", "certificate revocation")],
    )
    .await;
    assert_eq!(hits.len(), 1, "the current drawer holds the new message");
    let old = search(&client, &daemon.base_url, &[("q", "rotate signing keys")]).await;
    assert_eq!(
        old.len(),
        1,
        "history is superseded, so only the current version is a hit"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_sources_listing_shows_where_each_run_stopped_and_how_many_documents_it_holds() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let root = tempfile::tempdir().unwrap();
    write_session(root.path(), SESSION, 1_000);

    let before = sources(&client, &daemon.base_url).await;
    let names: Vec<_> = before["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].clone())
        .collect();
    assert_eq!(names, [json!("directory"), json!("pi-sessions")]);
    assert!(before["sources"].as_array().unwrap().is_empty());

    let job = mine_pi(&client, &daemon.base_url, root.path(), false).await;

    let after = sources(&client, &daemon.base_url).await;
    let source = &after["sources"][0];
    assert_eq!(source["provider"], "pi-sessions");
    assert_eq!(source["documents"], 1);
    assert_eq!(source["last_job"], json!(job.id));
    assert_eq!(source["cursor"]["mtime_ns"], 1_000_000_000_000i64);
    assert!(after.to_string().contains("pi-sessions"));
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_directory_job_in_its_original_wire_shape_still_works_and_is_now_idempotent() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.txt"), "an idempotent note").unwrap();

    let mut jobs = Vec::new();
    for _ in 0..2 {
        // Exactly what every caller sent before the source model existed.
        let response = submit(
            &client,
            &daemon.base_url,
            json!({"type": "mine", "path": dir.path(), "wing": "notes", "requested_by": "test"}),
        )
        .await;
        let job: Job = response.json().await.unwrap();
        jobs.push(
            wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await,
        );
    }

    assert_eq!(jobs[0].result.as_ref().unwrap()["created"], 1);
    assert_eq!(
        jobs[1].result.as_ref().unwrap()["created"],
        0,
        "a re-mine files nothing new"
    );
    let hits = search(&client, &daemon.base_url, &[("q", "idempotent note")]).await;
    assert_eq!(hits.len(), 1, "one drawer, not one per mine");
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_source_is_refused_at_submission_naming_the_known_ones() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let response = submit(
        &client,
        &daemon.base_url,
        json!({"type": "mine", "provider": "carrier-pigeon", "requested_by": "test"}),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "memcastle::input::invalid");
    assert!(
        body["error"].as_str().unwrap().contains("pi-sessions"),
        "{body}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_sessions_directory_that_does_not_exist_fails_the_job_instead_of_completing_empty() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let response = submit(
        &client,
        &daemon.base_url,
        json!({
            "type": "mine", "provider": "pi-sessions", "locator": "/nonexistent/pi/sessions", "requested_by": "test",
        }),
    )
    .await;
    let job: Job = response.json().await.unwrap();
    let job = wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Failed).await;
    assert!(job.error.is_some());
    daemon.shutdown().await;
}

#[tokio::test]
async fn mining_a_source_is_a_write_and_listing_sources_is_a_read() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let root = tempfile::tempdir().unwrap();
    write_session(root.path(), SESSION, 1_000);
    let body = json!({"type": "mine", "provider": "pi-sessions", "locator": root.path(), "requested_by": "test"});

    let refused = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .header(MODE_HEADER, "read_only")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::FORBIDDEN,
        "a read-only session must not file drawers"
    );

    let listed = client
        .get(format!("{}/api/sources", daemon.base_url))
        .header(MODE_HEADER, "read_only")
        .send()
        .await
        .unwrap();
    assert!(listed.status().is_success());
    let disabled = client
        .get(format!("{}/api/sources", daemon.base_url))
        .header(MODE_HEADER, "disabled")
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::FORBIDDEN);
    daemon.shutdown().await;
}
