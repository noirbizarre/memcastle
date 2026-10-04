//! End-to-end source-driven mining against a real daemon: a source other than a plain directory is discovered,
//! read incrementally and mined with no agent and no model involved, through the same unified contract
//! (docs/adr/023) every source goes through.
//!
//! The source is the built-in `directory` provider named explicitly, which is the form every other source takes
//! on the wire. What is specific to one source (Pi's history, `sources/pi`) is tested with that source, in
//! `tests/wasm_pi.rs`; what is tested here is what the daemon does for any source.

use crate::common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::{Job, JobStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};

const NOTE: &str =
    "How do I rotate the signing keys?\nRun the rotation script with the new key id.\n";
const MODE_HEADER: &str = "X-MemCastle-Mode";

/// Write a note under `root`, with a modification time `seconds` after the epoch so the cursor ordering never depends
/// on how fast the test runs.
fn write_note(root: &Path, content: &str, seconds: u64) {
    let file = root.join("notes.txt");
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

/// Mine the `directory` source at `root`, by name, and wait for the job to complete.
async fn mine_source(client: &reqwest::Client, base: &str, root: &Path, full: bool) -> Job {
    let response = submit(
        client,
        base,
        json!({
            "type": "mine", "provider": "directory", "locator": root, "full": full, "requested_by": "test",
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
async fn a_named_source_is_discovered_read_and_mined_then_found_by_search_with_its_provenance() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let root = tempfile::tempdir().unwrap();
    write_note(root.path(), NOTE, 1_000);

    let job = mine_source(&client, &daemon.base_url, root.path(), false).await;

    assert_eq!(job.result.as_ref().unwrap()["created"], 1);
    let hits = search(
        &client,
        &daemon.base_url,
        &[("q", "rotate signing keys"), ("source_kind", "file")],
    )
    .await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["source"]["kind"], "file");
    assert_eq!(hits[0]["source"]["origin"]["provider"], "directory");
    assert_eq!(hits[0]["source"]["origin"]["document"], "notes.txt");
    daemon.shutdown().await;
}

#[tokio::test]
async fn mining_a_source_again_reads_nothing_and_a_grown_document_adds_only_its_new_tail() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let root = tempfile::tempdir().unwrap();
    write_note(root.path(), NOTE, 1_000);
    mine_source(&client, &daemon.base_url, root.path(), false).await;

    let second = mine_source(&client, &daemon.base_url, root.path(), false).await;
    assert_eq!(
        second.result.as_ref().unwrap()["documents"],
        0,
        "the cursor is past the document"
    );

    let grown = format!("{NOTE}what about certificate revocation?\n");
    write_note(root.path(), &grown, 2_000);
    let third = mine_source(&client, &daemon.base_url, root.path(), false).await;
    let summary = third.result.as_ref().unwrap();
    assert_eq!(
        summary["documents"], 1,
        "only the document that grew is read"
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
    write_note(root.path(), NOTE, 1_000);

    let before = sources(&client, &daemon.base_url).await;
    let names: Vec<_> = before["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].clone())
        .collect();
    assert_eq!(names, [json!("directory")]);
    assert!(before["sources"].as_array().unwrap().is_empty());

    let job = mine_source(&client, &daemon.base_url, root.path(), false).await;

    let after = sources(&client, &daemon.base_url).await;
    let source = &after["sources"][0];
    assert_eq!(source["provider"], "directory");
    assert_eq!(source["documents"], 1);
    assert_eq!(source["last_job"], json!(job.id));
    assert_eq!(source["cursor"]["mtime_ns"], 1_000_000_000_000i64);
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
async fn a_directory_inside_a_project_is_mined_into_the_wing_the_project_file_declares() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    // A `.git` marker makes the temp directory its own project root, so no project file above it can apply.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    std::fs::create_dir_all(dir.path().join(".config")).unwrap();
    std::fs::write(
        dir.path().join(".config/memcastle.toml"),
        "[memcastle]\nwing = \"declared-by-the-project\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("note.txt"), "a note about the project file").unwrap();

    // No wing is given: the project file decides it.
    let response = submit(
        &client,
        &daemon.base_url,
        json!({"type": "mine", "path": dir.path(), "requested_by": "test"}),
    )
    .await;
    let job: Job = response.json().await.unwrap();
    wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await;
    // A hit carries a room id, not a wing name, so the wing is checked by filtering on it.
    let query = "note about the project file";
    let declared = search(
        &client,
        &daemon.base_url,
        &[("q", query), ("wing", "declared-by-the-project")],
    )
    .await;
    assert_eq!(declared.len(), 1, "{declared:?}");
    let by_dirname = search(&client, &daemon.base_url, &[("q", query)]).await;
    assert_eq!(by_dirname.len(), 1, "one drawer, in the declared wing only");

    // An explicit wing still wins over the file.
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(other.path().join(".git")).unwrap();
    std::fs::create_dir_all(other.path().join(".config")).unwrap();
    std::fs::write(
        other.path().join(".config/memcastle.toml"),
        "[memcastle]\nwing = \"ignored\"\n",
    )
    .unwrap();
    std::fs::write(
        other.path().join("note.txt"),
        "an explicit wing beats the file",
    )
    .unwrap();
    let response = submit(
        &client,
        &daemon.base_url,
        json!({"type": "mine", "path": other.path(), "wing": "explicit", "requested_by": "test"}),
    )
    .await;
    let job: Job = response.json().await.unwrap();
    wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await;
    let explicit = search(
        &client,
        &daemon.base_url,
        &[("q", "explicit wing beats the file"), ("wing", "explicit")],
    )
    .await;
    assert_eq!(explicit.len(), 1, "{explicit:?}");
    let ignored = search(
        &client,
        &daemon.base_url,
        &[("q", "explicit wing beats the file"), ("wing", "ignored")],
    )
    .await;
    assert!(ignored.is_empty(), "{ignored:?}");
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
        body["error"].as_str().unwrap().contains("directory"),
        "{body}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_locator_that_does_not_exist_fails_the_job_instead_of_completing_empty() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let response = submit(
        &client,
        &daemon.base_url,
        json!({
            "type": "mine", "provider": "directory", "locator": "/nonexistent/source/root", "requested_by": "test",
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
    write_note(root.path(), NOTE, 1_000);
    let body = json!({"type": "mine", "provider": "directory", "locator": root.path(), "requested_by": "test"});

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
