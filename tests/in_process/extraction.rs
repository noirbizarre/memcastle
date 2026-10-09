//! End-to-end entity extraction against a real daemon (docs/adr/024): content mined through the unified Source model
//! is read by the extraction job, and the entities and relationships it names appear in the knowledge graph with
//! provenance and temporal validity, while the drawers they came from stay exactly as mined.
//!
//! The built-in `heuristic` provider is used: deterministic, no model, nothing to stub.

use crate::common;

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use assert_cmd::cargo::cargo_bin;
use axum::routing::post;
use axum::{Json, Router};
use common::{TestDaemon, wait_for_job_status};
use memcastle::config::ExtractionProvider;
use memcastle::domain::{Job, JobStatus};
use serde_json::{Value, json};

const TEAM: &str = include_str!("../fixtures/sources/extraction/team.md");
const ARCHITECTURE: &str = include_str!("../fixtures/sources/extraction/architecture.md");

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn daemon() -> TestDaemon {
    TestDaemon::start_configured(|config| {
        config.extraction.provider = ExtractionProvider::Heuristic
    })
    .await
}

#[tokio::test]
async fn a_configured_document_criterion_ranks_relevant_mined_evidence_and_explains_why() {
    let source = tempfile::tempdir().unwrap();
    let file = source.path().join("decision.md");
    std::fs::write(&file, "Aurora protocol routes blue messages").unwrap();
    std::fs::write(
        source.path().join("draft.md"),
        "Aurora protocol routes blue messages",
    )
    .unwrap();
    let config = format!(
        "[sources.directory]\nlevel = 'low'\n[[sources.directory.criteria]]\npath = 'path'\nequals = '{}'\nlevel = 'high'",
        file.display(),
    );
    let daemon = TestDaemon::start_configured(move |settings| {
        settings.preferences = toml::from_str(&config).unwrap();
    })
    .await;
    let base = daemon.base_url.clone();
    mine(&base, source.path()).await;
    let response = client()
        .get(format!("{base}/api/search"))
        .query(&[("q", "Aurora protocol"), ("ranking", "lexical")])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let hits: Value = response.json().await.unwrap();
    assert_eq!(
        hits.as_array().unwrap().len(),
        2,
        "both documents remain evidence"
    );
    let hit = &hits.as_array().unwrap()[0];
    assert_eq!(hit["signals"]["preference"]["level"], "high");
    assert_eq!(hit["signals"]["preference"]["source"], "directory");
    assert_eq!(hit["signals"]["preference"]["criterion"], "path");
    assert_eq!(
        hit["source"]["origin"]["metadata"]["path"],
        file.display().to_string()
    );
    assert_eq!(hits[1]["signals"]["preference"]["level"], "low");
}

#[tokio::test]
async fn metadata_only_remining_keeps_the_old_event_time_on_historical_evidence() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "decision.md",
        "Aurora protocol routes blue messages",
        1_700_000_000,
    );
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    mine(base, dir.path()).await;
    let first = get(base, "/api/search?q=Aurora%20protocol&ranking=lexical").await;
    let previous = &first[0];
    let first_id = previous["id"].clone();
    let first_time = previous["source"]["origin"]["occurred_at"].clone();

    // The directory adapter's revision hashes the text, not the file's timestamp.
    write(
        dir.path(),
        "decision.md",
        "Aurora protocol routes blue messages",
        1_700_000_100,
    );
    let response = client().post(format!("{base}/api/jobs"))
        .json(&json!({"type": "mine", "path": dir.path(), "wing": "docs", "full": true, "requested_by": "test"}))
        .send().await.unwrap();
    assert!(response.status().is_success());
    let job: Job = response.json().await.unwrap();
    wait_for_job_status(&client(), base, job.id, JobStatus::Completed).await;
    let all = get(
        base,
        "/api/search?q=Aurora%20protocol&ranking=lexical&include_historical=true",
    )
    .await;
    let versions = all.as_array().unwrap();
    assert_eq!(versions.len(), 2);
    let old = versions.iter().find(|hit| hit["id"] == first_id).unwrap();
    let new = versions.iter().find(|hit| hit["id"] != first_id).unwrap();
    assert_eq!(old["source"]["origin"]["occurred_at"], first_time);
    assert_ne!(new["source"]["origin"]["occurred_at"], first_time);
    assert!(old["valid_to"].is_string());
}

/// Put `content` at `name` under `dir`, with a modification time `seconds` after the epoch so the mining cursor
/// ordering never depends on how fast the test runs.
fn write(dir: &Path, name: &str, content: &str, seconds: u64) {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

async fn mine(base: &str, dir: &Path) -> Job {
    let response = client()
        .post(format!("{base}/api/jobs"))
        .json(&json!({"type": "mine", "path": dir, "wing": "docs", "requested_by": "test"}))
        .send()
        .await
        .expect("submit");
    assert!(response.status().is_success(), "{}", response.status());
    let job: Job = response.json().await.expect("job");
    wait_for_job_status(&client(), base, job.id, JobStatus::Completed).await
}

async fn get(base: &str, path: &str) -> Value {
    let response = client()
        .get(format!("{base}{path}"))
        .send()
        .await
        .expect("request");
    assert!(
        response.status().is_success(),
        "{path}: {}",
        response.status()
    );
    response.json().await.expect("json")
}

/// Wait until every queued or running job (the mine, then the extraction sweep it queues) has finished.
async fn settle(base: &str) {
    for _ in 0..400 {
        let jobs = get(base, "/api/jobs").await;
        let busy = jobs
            .as_array()
            .unwrap()
            .iter()
            .any(|job| matches!(job["status"].as_str(), Some("queued" | "running")));
        let swept = jobs
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["kind"]["type"] == "extract");
        if swept && !busy {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the extraction sweep never finished");
}

async fn entity(base: &str, name: &str) -> Value {
    let found = get(base, &format!("/api/entities?name={name}")).await;
    found
        .as_array()
        .unwrap()
        .iter()
        .find(|entity| entity["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("no entity {name}: {found}"))
}

async fn relationships(base: &str, name: &str, expired: bool) -> Vec<Value> {
    let id = entity(base, name).await["id"].as_str().unwrap().to_string();
    get(
        base,
        &format!("/api/entities/{id}/relationships?include_expired={expired}"),
    )
    .await
    .as_array()
    .unwrap()
    .clone()
}

#[tokio::test]
async fn mined_fixtures_populate_the_graph_with_provenance_and_temporal_validity() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", TEAM, 1_000);
    write(dir.path(), "architecture.md", ARCHITECTURE, 1_001);

    mine(base, dir.path()).await;
    settle(base).await;

    // Entities, typed from the closed vocabulary.
    assert_eq!(entity(base, "Ada Lovelace").await["kind"], "person");
    assert_eq!(entity(base, "MemCastle").await["kind"], "project");
    assert_eq!(entity(base, "surrealdb").await["kind"], "tool");

    // Relationships: a fact, still true, traceable to the mined document it came from.
    let ada = relationships(base, "Ada Lovelace", false).await;
    let works_on = ada
        .iter()
        .find(|edge| edge["predicate"] == "works_on")
        .expect("Ada works on MemCastle");
    assert_eq!(works_on["valid_to"], Value::Null, "a fresh fact is current");
    assert!(works_on["valid_from"].is_string());
    let provenance = &works_on["provenance"];
    assert_eq!(provenance["extractor"], "heuristic");
    assert_eq!(provenance["origin"]["source"], "directory");
    assert_eq!(provenance["origin"]["document"], "team.md");
    assert!(provenance["job_id"].is_string());
    assert!(provenance["drawer"].is_string());
    assert_eq!(works_on["lifecycle"]["state"], "current");
    let history = get(
        base,
        &format!(
            "/api/relationships/{}/history",
            works_on["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["provenance"]["drawer"], provenance["drawer"]);
    assert!(ada.iter().any(|edge| edge["predicate"] == "member_of"));

    // The drawer it names is the one mined, verbatim.
    let drawer_id = provenance["drawer"].as_str().unwrap();
    let hits = get(base, "/api/search?q=Lovelace").await;
    let hit = hits
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["id"] == drawer_id)
        .expect("the provenance drawer is searchable");
    assert_eq!(hit["content"], TEAM);

    // And the mention is queryable from the entity, with the same provenance.
    let id = entity(base, "MemCastle").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mentions = get(base, &format!("/api/entities/{id}/mentions")).await;
    let documents: Vec<&str> = mentions
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["provenance"]["origin"]["document"].as_str().unwrap())
        .collect();
    assert!(
        documents.contains(&"team.md") && documents.contains(&"architecture.md"),
        "{documents:?}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn extraction_leaves_the_mined_drawers_exactly_as_they_were() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", TEAM, 1_000);

    // The sweep follows the mine job, so what the search returns right after the mine may or may not already have
    // been read; either way it must equal what it returns once the sweep is done. (The unit tests in
    // `extract::job` compare every stored field of every drawer before and after.)
    mine(base, dir.path()).await;
    let before = get(base, "/api/search?q=Lovelace").await;
    settle(base).await;
    let after = get(base, "/api/search?q=Lovelace").await;

    let strip = |hits: &Value| -> Vec<(Value, Value, Value, Value)> {
        hits.as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["id"].clone(),
                    h["content"].clone(),
                    h["valid_from"].clone(),
                    h["valid_to"].clone(),
                )
            })
            .collect()
    };
    assert_eq!(strip(&before), strip(&after));
    assert_eq!(after[0]["content"], TEAM);
    daemon.shutdown().await;
}

#[tokio::test]
async fn graph_expansion_reaches_a_drawer_through_an_entity_extraction_found() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", TEAM, 1_000);
    write(dir.path(), "architecture.md", ARCHITECTURE, 1_001);
    mine(base, dir.path()).await;
    settle(base).await;

    // "daemon" appears only in the architecture note; the team note shares `surrealdb` and MemCastle with it.
    let plain = get(base, "/api/search?q=daemon").await;
    assert_eq!(plain.as_array().unwrap().len(), 1);
    let expanded = get(base, "/api/search?q=daemon&expand=true").await;
    let hits = expanded.as_array().unwrap();
    assert_eq!(hits.len(), 2, "{expanded}");
    assert!(
        hits[1]["content"]
            .as_str()
            .unwrap()
            .contains("Ada Lovelace")
    );
    assert!(!hits[1]["via"].as_array().unwrap().is_empty());
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_changed_document_retires_the_facts_it_no_longer_supports_and_keeps_their_history() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "team.md",
        "Ada Lovelace works on MemCastle.",
        1_000,
    );
    mine(base, dir.path()).await;
    settle(base).await;
    assert_eq!(relationships(base, "Ada Lovelace", false).await.len(), 1);

    write(
        dir.path(),
        "team.md",
        "Ada Lovelace works on Compiler.",
        2_000,
    );
    mine(base, dir.path()).await;
    // The first sweep has finished; wait for the one this mine queued.
    for _ in 0..400 {
        if relationships(base, "Ada Lovelace", true).await.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    settle(base).await;

    let current = relationships(base, "Ada Lovelace", false).await;
    assert_eq!(
        current.len(),
        1,
        "only the new fact is current: {current:?}"
    );
    let all = relationships(base, "Ada Lovelace", true).await;
    assert_eq!(all.len(), 2, "history is kept");
    let closed = all
        .iter()
        .find(|edge| edge["valid_to"].is_string())
        .expect("the old fact is closed");
    assert_ne!(closed["id"], current[0]["id"]);
    assert_eq!(closed["lifecycle"]["state"], "superseded");
    let history = get(
        base,
        &format!(
            "/api/relationships/{}/history",
            closed["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert!(history.as_array().unwrap().iter().any(|fact| {
        fact["lifecycle"]["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["kind"] == "supersedes")
    }));
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_checkpoint_correction_is_explained_through_fact_history_and_as_of_reads() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "Ada works on MemCastle.", 1_000);
    mine(base, dir.path()).await;
    settle(base).await;
    let old = relationships(base, "Ada", false).await.remove(0);
    let id = old["id"].as_str().unwrap();
    let subject = old["from"].as_str().unwrap();
    let object = old["to"].as_str().unwrap();
    for invalid in [
        json!({"op": "link", "relationship_id": id, "other_id": id, "kind": "confirms", "reason": "same assertion"}),
        json!({"op": "link", "relationship_id": id, "other_id": "00000000-0000-4000-8000-000000000001", "kind": "refines", "reason": "   "}),
        json!({"op": "link", "relationship_id": id, "other_id": "00000000-0000-4000-8000-000000000001", "kind": "supersedes", "reason": "use the atomic operation"}),
        json!({"op": "supersede", "relationship_id": id, "from": subject, "to": object,
            "predicate": "works_on", "confidence": 1.0, "reason": " "}),
        json!({"op": "invalidate", "relationship_id": id, "reason": " "}),
    ] {
        let response = client()
            .post(format!("{base}/api/jobs"))
            .json(&json!({"type": "checkpoint", "payload": {"items": [{
                "destination": "general", "content": "invalid correction", "tags": [],
                "source": {"kind": "manual"}, "fact": invalid
            }]}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["code"], "memcastle::input::invalid");
    }
    let submitted: Job = client().post(format!("{base}/api/jobs"))
        .json(&json!({"type": "checkpoint", "requested_by": "test", "payload": {"items": [{
            "destination": "general", "content": "Correction: Ada still works on MemCastle.", "tags": [],
            "source": {"kind": "manual", "agent": "test"},
            "fact": {"op": "supersede", "relationship_id": id, "from": subject, "to": object,
                "predicate": "works_on", "confidence": 1.0, "reason": "confirmed by Ada"}
        }]}})).send().await.unwrap().json().await.unwrap();
    wait_for_job_status(&client(), base, submitted.id, JobStatus::Completed).await;

    let history = get(base, &format!("/api/relationships/{id}/history")).await;
    let assertions = history.as_array().unwrap();
    assert_eq!(assertions.len(), 2, "{history}");
    let previous = assertions.iter().find(|fact| fact["id"] == id).unwrap();
    assert_eq!(previous["lifecycle"]["state"], "superseded");
    assert_eq!(
        previous["lifecycle"]["links"][0]["reason"],
        "confirmed by Ada"
    );
    let replacement = assertions.iter().find(|fact| fact["id"] != id).unwrap();
    assert_eq!(replacement["lifecycle"]["state"], "current");
    assert!(replacement["assertion"].is_string());

    let boundary = replacement["valid_from"].as_str().unwrap();
    let at: Value = client()
        .get(format!("{base}/api/entities/{subject}/relationships"))
        .query(&[("as_of", boundary)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(at.as_array().unwrap().len(), 1, "{at}");
    assert_eq!(at[0]["id"], replacement["id"]);
    for path in [
        format!("/api/entities/{subject}/relationships?include_expired=true&as_of=2026-01-01"),
        "/api/relationships/not-a-uuid/history".to_string(),
        format!("/api/relationships/{id}/history?as_of=not-a-date"),
    ] {
        let response = client().get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{path}"
        );
        let error: Value = response.json().await.unwrap();
        assert_eq!(
            error["code"], "memcastle::input::invalid",
            "{path}: {error}"
        );
    }

    // The CLI must use the same authenticated REST read and pass as_of through without changing its meaning.
    let output = tokio::process::Command::new(cargo_bin("memcastle"))
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .env_remove("MEMCASTLE_MODE")
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdin(Stdio::null())
        .args(["--json", "fact", "history", id, "--as-of", boundary])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cli[0]["id"], previous["id"]);
    assert_eq!(cli[1]["id"], replacement["id"]);
    assert_eq!(cli[1]["lifecycle"]["state"], "current");

    let pretty = tokio::process::Command::new(cargo_bin("memcastle"))
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .env("CLICOLOR_FORCE", "1")
        .stdin(Stdio::null())
        .args(["fact", "history", id])
        .output()
        .await
        .unwrap();
    assert!(
        pretty.status.success(),
        "{}",
        String::from_utf8_lossy(&pretty.stderr)
    );
    // Piped output is JSON regardless of terminal-colour settings.
    let piped: Value = serde_json::from_slice(&pretty.stdout).unwrap();
    assert_eq!(piped.as_array().unwrap().len(), 2);

    let malformed = tokio::process::Command::new(cargo_bin("memcastle"))
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .stdin(Stdio::null())
        .args(["fact", "history", "not-a-uuid"])
        .output()
        .await
        .unwrap();
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("relationship_id"));

    let refined: Job = client().post(format!("{base}/api/jobs"))
        .json(&json!({"type": "checkpoint", "requested_by": "test", "payload": {"items": [{
            "destination": "general", "content": "The correction refines the earlier claim.", "tags": [],
            "source": {"kind": "manual", "agent": "test"},
            "fact": {"op": "link", "relationship_id": replacement["id"], "other_id": id,
                "kind": "refines", "reason": "explicitly reviewed"}
        }]}})).send().await.unwrap().json().await.unwrap();
    wait_for_job_status(&client(), base, refined.id, JobStatus::Completed).await;
    let linked = get(base, &format!("/api/relationships/{id}/history")).await;
    assert!(
        linked
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|fact| fact["lifecycle"]["links"].as_array().unwrap())
            .any(|link| link["kind"] == "refines" && link["reason"] == "explicitly reviewed")
    );
    let session = common::mcp::connect(base).await;
    common::mcp::set_mode(&session, "read_only").await;
    let via_mcp = common::mcp::call(
        &session,
        "memcastle_fact_history",
        json!({"relationship_id": id, "as_of": boundary}),
    )
    .await
    .ok();
    assert_eq!(via_mcp.as_array().unwrap().len(), 2);
    assert!(
        via_mcp
            .as_array()
            .unwrap()
            .iter()
            .any(|fact| fact["id"] == replacement["id"])
    );
    session.cancel().await.unwrap();
    daemon.shutdown().await;
}

#[tokio::test]
async fn without_a_provider_nothing_is_extracted_and_an_extract_job_is_refused() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", TEAM, 1_000);
    mine(base, dir.path()).await;

    let jobs = get(base, "/api/jobs").await;
    assert!(
        jobs.as_array()
            .unwrap()
            .iter()
            .all(|job| job["kind"]["type"] != "extract")
    );
    assert_eq!(get(base, "/api/entities").await, json!([]));

    let refused = client()
        .post(format!("{base}/api/jobs"))
        .json(&json!({"type": "extract"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 400);
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["code"], "memcastle::extract::not_configured");
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_extract_job_can_be_requested_by_hand_and_finds_nothing_left_to_read() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", TEAM, 1_000);
    mine(base, dir.path()).await;
    settle(base).await;

    let job: Job = client()
        .post(format!("{base}/api/jobs"))
        .json(&json!({"type": "extract", "wing": "docs", "requested_by": "test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let done = wait_for_job_status(&client(), base, job.id, JobStatus::Completed).await;
    assert_eq!(done.result.unwrap()["drawers"], 0);
    daemon.shutdown().await;
}

#[tokio::test]
async fn reading_an_entity_that_does_not_exist_is_a_404_with_a_code() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let missing = "00000000-0000-4000-8000-000000000000";
    for suffix in ["relationships", "mentions"] {
        let response = client()
            .get(format!("{base}/api/entities/{missing}/{suffix}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 404);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["code"], "memcastle::graph::entity_not_found");
    }
    let malformed = client()
        .get(format!("{base}/api/entities/not-a-uuid/mentions"))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status().as_u16(), 400);
    daemon.shutdown().await;
}

/// An OpenAI-compatible `/v1/chat/completions` that "extracts" one fixed graph from any text (wrapped in a Markdown
/// fence, as chatty models do), and records what it was asked. Returns its base URL.
async fn chat_stub(seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body);
                let reply = "```json\n{\"entities\":[{\"name\":\"Ada\",\"kind\":\"Human\"},\
                    {\"name\":\"MemCastle\",\"kind\":\"project\"}],\
                    \"relations\":[{\"subject\":\"Ada\",\"predicate\":\"hacks on\",\"object\":\"MemCastle\",\
                    \"confidence\":0.9}]}\n```";
                Json(json!({"choices": [{"message": {"role": "assistant", "content": reply}}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{port}/v1")
}

#[tokio::test]
async fn an_http_provider_is_asked_for_the_vocabulary_and_what_it_says_is_held_to_it() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let url = chat_stub(seen.clone()).await;
    let daemon = TestDaemon::start_configured(move |config| {
        config.extraction.provider = ExtractionProvider::Http;
        config.extraction.url = Some(url);
        config.extraction.model = Some("stub".into());
    })
    .await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", "free text the stub ignores", 1_000);

    mine(base, dir.path()).await;
    settle(base).await;

    // The model's own words were read into the closed vocabulary: `Human` is `other`, `hacks on` is `related_to`.
    assert_eq!(entity(base, "Ada").await["kind"], "other");
    let edges = relationships(base, "Ada", false).await;
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["predicate"], "related_to");
    assert_eq!(edges[0]["provenance"]["extractor"], "http");

    // The request carried the model, the text and the vocabulary to answer in.
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests[0]["model"], "stub");
    let system = requests[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("works_on") && system.contains("organization"),
        "{system}"
    );
    assert_eq!(
        requests[0]["messages"][1]["content"],
        "free text the stub ignores"
    );
    daemon.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_provider_is_run_with_the_texts_and_its_answer_becomes_the_graph() {
    let tools = tempfile::tempdir().unwrap();
    let script = tools.path().join("extract.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"extractions\":[{\"entities\":[{\"name\":\"Ada\",\"kind\":\"person\"},{\"name\":\"Parser\",\"kind\":\"project\"}],\"relations\":[{\"subject\":\"Ada\",\"predicate\":\"works_on\",\"object\":\"Parser\",\"confidence\":0.8}]}]}'\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let program = script.display().to_string();
    let daemon = TestDaemon::start_configured(move |config| {
        config.extraction.provider = ExtractionProvider::Command;
        config.extraction.command = vec![program];
        // One drawer per call, so the script's single answer matches the single text.
        config.extraction.batch_size = 1;
    })
    .await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.md", "whatever", 1_000);

    mine(base, dir.path()).await;
    settle(base).await;

    let edges = relationships(base, "Ada", false).await;
    assert_eq!(edges[0]["predicate"], "works_on");
    assert_eq!(edges[0]["provenance"]["extractor"], "command");
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_graph_route_answers_a_neighbourhood_and_an_overview_in_one_request() {
    let daemon = daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "team.md", TEAM, 1_000);
    write(dir.path(), "architecture.md", ARCHITECTURE, 1_001);
    mine(base, dir.path()).await;
    settle(base).await;
    let ada = entity(base, "Ada Lovelace").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let hood = get(base, &format!("/api/graph?entity={ada}")).await;

    let nodes = hood["nodes"].as_array().unwrap();
    assert_eq!(nodes[0]["name"], "Ada Lovelace", "the centre comes first");
    assert!(
        nodes.iter().any(|node| node["name"] == "MemCastle"),
        "{hood}"
    );
    let edges = hood["edges"].as_array().unwrap();
    assert!(
        edges.iter().any(|edge| edge["predicate"] == "works_on"),
        "{hood}"
    );
    assert!(
        edges.iter().all(|edge| {
            let known = |id: &Value| nodes.iter().any(|node| node["id"] == *id);
            known(&edge["from"]) && known(&edge["to"])
        }),
        "an edge never points at a node the answer does not hold: {hood}"
    );
    assert_eq!(hood["truncated"], false);

    // Without a centre it is an overview, bounded by `limit`, and says when it stopped short.
    let overview = get(base, "/api/graph").await;
    assert!(
        overview["nodes"].as_array().unwrap().len() > 1,
        "{overview}"
    );
    let capped = get(base, "/api/graph?limit=1").await;
    assert_eq!(capped["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(capped["truncated"], true, "{capped}");
    assert!(
        capped["edges"].as_array().unwrap().is_empty(),
        "one node has no edge between its members: {capped}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn the_graph_route_refuses_an_unknown_or_malformed_entity() {
    let daemon = daemon().await;
    let base = &daemon.base_url;

    let unknown = client()
        .get(format!(
            "{base}/api/graph?entity=00000000-0000-0000-0000-000000000000"
        ))
        .send()
        .await
        .unwrap();
    let malformed = client()
        .get(format!("{base}/api/graph?entity=not-an-id"))
        .send()
        .await
        .unwrap();

    assert_eq!(unknown.status(), 404);
    assert_eq!(malformed.status(), 400);
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_graph_route_is_a_read_and_a_disabled_session_gets_nothing() {
    let daemon = daemon().await;
    let base = &daemon.base_url;

    let response = client()
        .get(format!("{base}/api/graph"))
        .header("x-memcastle-mode", "disabled")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 403);
    daemon.shutdown().await;
}
