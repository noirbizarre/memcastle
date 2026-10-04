//! Memory deduplication and entity resolution against a real daemon (docs/adr/025).
//!
//! The policy under test: an exact copy in the same room is not stored twice; a typo or a case-and-punctuation
//! variant is stored and linked, never merged; a similar-but-distinct memory is left alone. For entities, spelling
//! variants converge on one entity while every source keeps its own spelling, and an ambiguous name stays distinct and
//! says what it might be.

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::{TestDaemon, wait_for_job_status};
use memcastle::config::ExtractionProvider;
use memcastle::domain::{Job, JobStatus};
use serde_json::{Value, json};

const DECISION: &str = "We decided to use SurrealDB for storage because one engine covers documents, graph and vectors.";

fn client() -> reqwest::Client {
    reqwest::Client::new()
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

async fn post(base: &str, path: &str, body: &Value) -> (u16, Value) {
    let response = client()
        .post(format!("{base}{path}"))
        .json(body)
        .send()
        .await
        .expect("request");
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// Write one drawer through the REST API: the path an agent or a person uses.
async fn write_drawer(base: &str, content: &str) -> (u16, Value) {
    post(
        base,
        "/api/wings/notes/rooms/entries/drawers",
        &json!({ "content": content, "requested_by": "test" }),
    )
    .await
}

async fn drawers_in_room(base: &str) -> Vec<Value> {
    get(base, "/api/wings/notes/rooms/entries/drawers")
        .await
        .as_array()
        .expect("a list")
        .clone()
}

async fn duplicates(base: &str, drawer: &str) -> Vec<Value> {
    get(base, &format!("/api/drawers/{drawer}/duplicates")).await["similar"]
        .as_array()
        .expect("a list")
        .clone()
}

async fn checkpoint(base: &str, items: &[&str]) -> Job {
    let items: Vec<Value> = items
        .iter()
        .map(|content| {
            json!({
                "destination": "general",
                "wing": "notes",
                "content": content,
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "test" },
                "fact": null,
            })
        })
        .collect();
    let response = client()
        .post(format!("{base}/api/jobs"))
        .json(&json!({
            "type": "checkpoint",
            "payload": { "items": items },
            "requested_by": "test",
        }))
        .send()
        .await
        .expect("submit");
    assert!(response.status().is_success(), "{}", response.status());
    let job: Job = response.json().await.expect("job");
    wait_for_job_status(&client(), base, job.id, JobStatus::Completed).await
}

#[tokio::test]
async fn writing_the_same_memory_twice_stores_one_drawer_and_returns_the_first() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;

    let (first_status, first) = write_drawer(base, DECISION).await;
    let (second_status, second) = write_drawer(base, DECISION).await;

    assert_eq!(first_status, 201);
    assert_eq!(first["created"], true);
    assert_eq!(
        second_status, 200,
        "an existing record is a 200, as for any idempotent create"
    );
    assert_eq!(second["created"], false);
    assert_eq!(
        second["id"], first["id"],
        "the writer is handed the drawer that already holds it"
    );
    assert_eq!(drawers_in_room(base).await.len(), 1);
}

#[tokio::test]
async fn a_one_character_typo_is_kept_and_linked_to_the_original_for_later_resolution() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let (_, original) = write_drawer(base, DECISION).await;

    let (status, typo) = write_drawer(base, &DECISION.replace("storage", "storge")).await;

    assert_eq!(status, 201, "a typo is not an exact copy, so it is stored");
    assert_ne!(typo["id"], original["id"]);
    assert_eq!(drawers_in_room(base).await.len(), 2, "nothing is merged");
    let links = duplicates(base, typo["id"].as_str().unwrap()).await;
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["drawer"], original["id"]);
    assert_eq!(links[0]["side"], "older");
    assert_eq!(links[0]["kind"], "near");
    assert!(links[0]["similarity"].as_f64().unwrap() > 0.95);
    assert_eq!(links[0]["signals"]["same_hash"], false);
    // The same fact is readable from the original's side.
    let back = duplicates(base, original["id"].as_str().unwrap()).await;
    assert_eq!(back[0]["drawer"], typo["id"]);
    assert_eq!(back[0]["side"], "newer");
}

#[tokio::test]
async fn a_similar_but_distinct_memory_is_stored_and_not_linked() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let (_, billing) = write_drawer(
        base,
        "We decided to use Postgres for the billing service because the team knows it well.",
    )
    .await;

    let (_, memory) = write_drawer(
        base,
        "We decided to use SurrealDB for the memory service because one engine covers more.",
    )
    .await;

    assert_eq!(drawers_in_room(base).await.len(), 2);
    assert!(
        duplicates(base, memory["id"].as_str().unwrap())
            .await
            .is_empty()
    );
    assert!(
        duplicates(base, billing["id"].as_str().unwrap())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_checkpoint_replayed_with_the_same_item_stores_it_once_and_reports_the_duplicate() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;

    let first = checkpoint(base, &[DECISION]).await;
    let second = checkpoint(
        base,
        &[DECISION, "A second, different memory worth keeping."],
    )
    .await;

    assert_eq!(first.result.as_ref().unwrap()["duplicates"], 0);
    assert_eq!(
        second.result.as_ref().unwrap()["duplicates"],
        1,
        "the job still completes and says what it did not store"
    );
    assert_eq!(second.result.as_ref().unwrap()["items"], 2);
    let stored = get(base, "/api/wings/notes/rooms/entries/drawers").await;
    assert_eq!(
        stored.as_array().unwrap().len(),
        2,
        "one for the decision, one for the new memory"
    );
}

#[tokio::test]
async fn the_same_text_in_another_room_is_a_different_memory() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    write_drawer(base, DECISION).await;

    let (status, other) = post(
        base,
        "/api/wings/notes/rooms/elsewhere/drawers",
        &json!({ "content": DECISION, "requested_by": "test" }),
    )
    .await;

    assert_eq!(status, 201);
    assert_eq!(other["created"], true);
}

#[tokio::test]
async fn an_unknown_drawer_has_no_duplicates_to_report() {
    let daemon = TestDaemon::start().await;
    let response = client()
        .get(format!(
            "{}/api/drawers/00000000-0000-4000-8000-000000000000/duplicates",
            daemon.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 404);
}

#[tokio::test]
async fn deduplication_can_be_turned_off() {
    let daemon = TestDaemon::start_configured(|config| config.dedup.enabled = false).await;
    let base = &daemon.base_url;

    write_drawer(base, DECISION).await;
    let (status, _) = write_drawer(base, DECISION).await;

    assert_eq!(status, 201);
    assert_eq!(drawers_in_room(base).await.len(), 2);
}

/// Put `content` at `name` under `dir`, with a modification time `seconds` after the epoch so the mining cursor
/// ordering never depends on how fast the test runs.
fn write_file(dir: &Path, name: &str, content: &str, seconds: u64) {
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
    let (status, job) = post(
        base,
        "/api/jobs",
        &json!({"type": "mine", "path": dir, "wing": "docs", "requested_by": "test"}),
    )
    .await;
    assert!((200..300).contains(&status), "{status}");
    let job: Job = serde_json::from_value(job).expect("job");
    wait_for_job_status(&client(), base, job.id, JobStatus::Completed).await
}

#[tokio::test]
async fn mining_keeps_identical_files_as_two_records_and_links_them() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "a.md", DECISION, 1_000);
    write_file(dir.path(), "b.md", DECISION, 1_001);

    let job = mine(base, dir.path()).await;

    let result = job.result.expect("a summary");
    assert_eq!(
        result["created"], 2,
        "two identical files are two records (docs/adr/023)"
    );
    assert_eq!(
        result["similar"], 1,
        "the later one is noted as a copy of the earlier"
    );
    let rooms = get(base, "/api/wings/docs").await;
    let room = rooms["rooms"][0]["name"].as_str().unwrap().to_string();
    let drawers = get(base, &format!("/api/wings/docs/rooms/{room}/drawers")).await;
    let drawers = drawers.as_array().unwrap();
    assert_eq!(drawers.len(), 2);
    let linked = duplicates(base, drawers[0]["id"].as_str().unwrap()).await;
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0]["kind"], "exact");
    assert_eq!(linked[0]["signals"]["same_hash"], true);
}

async fn extracting_daemon() -> TestDaemon {
    TestDaemon::start_configured(|config| {
        config.extraction.provider = ExtractionProvider::Heuristic
    })
    .await
}

/// Wait until every queued or running job (the mine, then the extraction sweep it queues) has finished.
async fn settle(base: &str) {
    for _ in 0..400 {
        let jobs = get(base, "/api/jobs").await;
        let jobs = jobs.as_array().unwrap();
        let busy = jobs
            .iter()
            .any(|job| matches!(job["status"].as_str(), Some("queued" | "running")));
        let swept = jobs.iter().any(|job| job["kind"]["type"] == "extract");
        if swept && !busy {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the extraction sweep never finished");
}

async fn people(base: &str) -> Vec<Value> {
    get(base, "/api/entities?kind=person")
        .await
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn entity_variants_from_different_documents_converge_and_each_keeps_its_own_spelling() {
    let daemon = extracting_daemon().await;
    let base = &daemon.base_url;
    let dir = tempfile::tempdir().unwrap();
    write_file(dir.path(), "a.md", "Ada works on MemCastle.", 1_000);
    write_file(dir.path(), "b.md", "ADA works on MemCastle.", 1_001);
    mine(base, dir.path()).await;
    settle(base).await;

    let people = people(base).await;
    assert_eq!(people.len(), 1, "Ada and ADA are one person: {people:?}");
    let ada = &people[0];
    assert_eq!(
        ada["name"], "Ada",
        "the canonical name is the first spelling seen, not rewritten"
    );
    assert_eq!(ada["aliases"], json!(["ADA"]));

    // Provenance and source observations stay queryable after resolution.
    let id = ada["id"].as_str().unwrap();
    let mentions = get(base, &format!("/api/entities/{id}/mentions")).await;
    let mentions = mentions.as_array().unwrap();
    assert_eq!(
        mentions.len(),
        2,
        "both documents still vouch for the entity"
    );
    let mut spellings: Vec<&str> = mentions
        .iter()
        .map(|m| m["observation"]["name"].as_str().unwrap())
        .collect();
    spellings.sort_unstable();
    assert_eq!(spellings, ["ADA", "Ada"]);
    let ada_variant = mentions
        .iter()
        .find(|m| m["observation"]["name"] == "ADA")
        .unwrap();
    assert_eq!(ada_variant["observation"]["rule"], "normalized");
    assert_eq!(ada_variant["provenance"]["extractor"], "heuristic");
    assert!(ada_variant["provenance"]["origin"]["document"].is_string());
}

async fn mention(base: &str, drawer: &str, name: &str) -> Value {
    let (status, link) = post(
        base,
        &format!("/api/drawers/{drawer}/mentions"),
        &json!({ "name": name, "kind": "tool" }),
    )
    .await;
    assert!((200..300).contains(&status), "{status}: {link}");
    link
}

#[tokio::test]
async fn a_unique_typo_converges_but_an_ambiguous_name_stays_distinct_and_exposes_its_candidates() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let (_, drawer) = write_drawer(base, "Notes about the tools we compared.").await;
    let drawer = drawer["id"].as_str().unwrap();

    let surreal = mention(base, drawer, "SurrealDB").await;
    let typo = mention(base, drawer, "SurealDB").await;
    assert_eq!(
        typo["entity"]["id"], surreal["entity"]["id"],
        "one edit, one candidate: the same tool"
    );

    // Two tools two edits apart, and a name one edit from each: nothing says which one it is.
    let first = mention(base, drawer, "Katarina").await;
    let second = mention(base, drawer, "Katerino").await;
    assert_ne!(first["entity"]["id"], second["entity"]["id"]);
    let ambiguous = mention(base, drawer, "Katerina").await;
    let ambiguous_id = ambiguous["entity"]["id"].as_str().unwrap();
    assert_ne!(ambiguous["entity"]["id"], first["entity"]["id"]);
    assert_ne!(ambiguous["entity"]["id"], second["entity"]["id"]);

    let candidates = get(base, &format!("/api/entities/{ambiguous_id}/candidates")).await;
    let candidates = candidates.as_array().unwrap();
    assert_eq!(
        candidates.len(),
        2,
        "both resemblances are exposed for later resolution"
    );
    let mut named: Vec<&str> = candidates
        .iter()
        .map(|c| c["entity"]["name"].as_str().unwrap())
        .collect();
    named.sort_unstable();
    assert_eq!(named, ["Katarina", "Katerino"]);
    assert!(candidates.iter().all(|c| c["side"] == "older"));
}

#[tokio::test]
async fn an_alias_added_by_hand_settles_the_names_that_follow() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let (_, drawer) = write_drawer(base, "Notes about the project.").await;
    let drawer = drawer["id"].as_str().unwrap();
    let castle = mention(base, drawer, "MemCastle").await;
    let id = castle["entity"]["id"].as_str().unwrap();

    let (status, entity) = post(
        base,
        &format!("/api/entities/{id}/aliases"),
        &json!({ "alias": "the castle" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(entity["aliases"], json!(["the castle"]));

    let again = mention(base, drawer, "The Castle").await;
    assert_eq!(again["entity"]["id"], castle["entity"]["id"]);

    let (blank, _) = post(
        base,
        &format!("/api/entities/{id}/aliases"),
        &json!({ "alias": "  " }),
    )
    .await;
    assert_eq!(blank, 400, "a blank alias is the caller's mistake");
}

#[tokio::test]
async fn disabling_fuzzy_entity_matching_keeps_typos_apart_but_still_converges_casing() {
    let daemon = TestDaemon::start_configured(|config| config.dedup.entity_fuzzy = false).await;
    let base = &daemon.base_url;
    let (_, drawer) = write_drawer(base, "Notes about the tools we compared.").await;
    let drawer = drawer["id"].as_str().unwrap();

    let surreal = mention(base, drawer, "SurrealDB").await;
    let typo = mention(base, drawer, "SurealDB").await;
    let casing = mention(base, drawer, "surrealdb").await;

    assert_ne!(typo["entity"]["id"], surreal["entity"]["id"]);
    assert_eq!(casing["entity"]["id"], surreal["entity"]["id"]);
}
