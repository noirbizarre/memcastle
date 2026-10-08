//! Temporal retrieval over MCP (docs/adr/032): an agent searches the past, then follows a hit to how that
//! knowledge evolved, with the same query model REST and the CLI use and no second path to the data.

use std::time::Duration;

use crate::common;

use common::TestDaemon;
use common::mcp::{call, connect};
use serde_json::{Value, json};

async fn write(base: &str, content: &str) -> String {
    let drawer: Value = reqwest::Client::new()
        .post(format!("{base}/api/wings/w/rooms/r/drawers"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .expect("create drawer")
        .json()
        .await
        .expect("json");
    drawer["id"].as_str().expect("an id").to_string()
}

async fn correct(base: &str, id: &str, content: &str) -> String {
    let outcome: Value = reqwest::Client::new()
        .post(format!("{base}/api/drawers/{id}/supersede"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .expect("supersede")
        .json()
        .await
        .expect("json");
    outcome["replacement"]["id"]
        .as_str()
        .expect("a replacement")
        .to_string()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn tick() {
    tokio::time::sleep(Duration::from_millis(60)).await;
}

fn contents(hits: &Value) -> Vec<&str> {
    let mut found: Vec<&str> = hits
        .as_array()
        .expect("hits")
        .iter()
        .map(|hit| hit["content"].as_str().expect("content"))
        .collect();
    found.sort_unstable();
    found
}

#[tokio::test]
async fn an_agent_can_search_the_past_and_follow_a_hit_to_its_history() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let first = write(base, "we run the database on postgres").await;
    tick().await;
    let then = now();
    tick().await;
    let second = correct(base, &first, "we run the database on surrealdb").await;
    tick().await;
    let later = now();
    let session = connect(base).await;

    // Now: only the latest belief.
    let current = call(&session, "memcastle_search", json!({ "query": "database" }))
        .await
        .ok();
    assert_eq!(contents(&current), ["we run the database on surrealdb"]);

    // As of an earlier instant: the belief held then, and its hit carries the id history needs.
    let past = call(
        &session,
        "memcastle_search",
        json!({ "query": "database", "as_of": then }),
    )
    .await
    .ok();
    assert_eq!(contents(&past), ["we run the database on postgres"]);
    let id = past[0]["id"].as_str().expect("a hit carries its id");

    // An interval across the correction: both beliefs, whichever tool asks.
    for tool in ["memcastle_search", "memcastle_recall"] {
        let across = call(
            &session,
            tool,
            json!({ "query": "database", "from": then, "until": later }),
        )
        .await
        .ok();
        assert_eq!(
            contents(&across),
            [
                "we run the database on postgres",
                "we run the database on surrealdb"
            ],
            "{tool}"
        );
    }

    // History from the old hit gives the whole evolution, oldest first, with validity and provenance.
    let history = call(&session, "memcastle_history", json!({ "drawer_id": id }))
        .await
        .ok();
    assert_eq!(history["drawer"], id);
    let versions = history["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["content"], "we run the database on postgres");
    assert_eq!(versions[1]["content"], "we run the database on surrealdb");
    assert_eq!(versions[1]["id"], second.as_str());
    assert_eq!(versions[0]["valid_to"], versions[1]["valid_from"]);
    assert!(versions[1]["valid_to"].is_null());
    assert!(versions[0]["provenance"].is_object());

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_bad_temporal_argument_or_history_id_is_rejected_with_an_actionable_error() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;

    for arguments in [
        json!({ "query": "x", "from": "2026-01-01" }),
        json!({ "query": "x", "from": "2026-02-01", "until": "2026-01-01" }),
        json!({ "query": "x", "as_of": "2026-01-01", "from": "2025-01-01", "until": "2027-01-01" }),
        json!({ "query": "x", "as_of": "someday" }),
    ] {
        let outcome = call(&session, "memcastle_search", arguments.clone()).await;
        assert_eq!(
            outcome.error_code(),
            "memcastle::input::invalid",
            "{arguments}"
        );
    }

    let malformed = call(
        &session,
        "memcastle_history",
        json!({ "drawer_id": "nope" }),
    )
    .await;
    assert_eq!(malformed.error_code(), "memcastle::input::invalid");
    let unknown = call(
        &session,
        "memcastle_history",
        json!({ "drawer_id": uuid::Uuid::new_v4().to_string() }),
    )
    .await;
    assert_eq!(unknown.error_code(), "memcastle::palace::drawer_not_found");

    for (arguments, code) in [
        (
            json!({"relationship_id": "not-a-uuid"}),
            "memcastle::input::invalid",
        ),
        (
            json!({"relationship_id": uuid::Uuid::new_v4(), "as_of": "not-a-date"}),
            "memcastle::input::invalid",
        ),
        (
            json!({"relationship_id": uuid::Uuid::new_v4()}),
            "memcastle::graph::relationship_not_found",
        ),
    ] {
        let result = call(&session, "memcastle_fact_history", arguments).await;
        assert_eq!(result.error_code(), code);
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_session_can_read_history_and_the_tool_changes_nothing() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let id = write(base, "kept as written").await;
    let session = connect(base).await;
    common::mcp::set_mode(&session, "read_only").await;

    let history = call(&session, "memcastle_history", json!({ "drawer_id": id }))
        .await
        .ok();

    assert_eq!(history["versions"].as_array().expect("versions").len(), 1);
    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}
