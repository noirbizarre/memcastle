//! `POST /api/notes` against a real daemon (docs/adr/031): a note is stored verbatim as an ordinary drawer of kind
//! `note`, so it is searchable, deduplicated and read for entities like any other memory, and no second store exists.

use crate::common;

use std::time::Duration;

use common::TestDaemon;
use memcastle::config::ExtractionProvider;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn call(
    daemon: &TestDaemon,
    method: Method,
    path: &str,
    body: Option<Value>,
    mode: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = reqwest::Client::new().request(method, format!("{}{path}", daemon.base_url));
    if let Some(body) = body {
        request = request.json(&body);
    }
    if let Some(mode) = mode {
        request = request.header("x-memcastle-mode", mode);
    }
    let response = request.send().await.expect("request");
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn note(daemon: &TestDaemon, body: Value) -> (StatusCode, Value) {
    call(daemon, Method::POST, "/api/notes", Some(body), None).await
}

fn a_note(content: &str) -> Value {
    json!({ "wing": "keep", "room": "notes", "content": content, "uri": "/work/keep", "requested_by": "cli" })
}

#[tokio::test]
async fn a_note_is_stored_verbatim_with_its_provenance_and_a_stable_id() {
    let daemon = TestDaemon::start().await;
    let text = "Remember:\n  - the milk\n  - the   spacing\n";

    let (status, stored) = note(&daemon, a_note(text)).await;

    assert_eq!(status, StatusCode::CREATED, "{stored}");
    assert_eq!(stored["created"], true);
    assert_eq!(
        stored["content"], text,
        "the original text is kept as typed"
    );
    assert_eq!(stored["source"]["kind"], "note");
    assert_eq!(stored["source"]["uri"], "/work/keep");
    assert_eq!(stored["provenance"]["requested_by"], "cli");
    assert!(stored["created_at"].is_string() && stored["valid_from"].is_string());

    // The id it was confirmed with finds the same drawer later.
    let id = stored["id"].as_str().unwrap();
    let (status, shown) = call(
        &daemon,
        Method::GET,
        &format!("/api/wings/keep/rooms/notes/drawers/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["content"], text);
    assert_eq!(shown["source"]["kind"], "note");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_note_is_found_by_search_and_narrowed_by_the_note_source_kind() {
    let daemon = TestDaemon::start().await;
    note(&daemon, a_note("the zebra crossing is on Main Street")).await;
    call(
        &daemon,
        Method::POST,
        "/api/wings/keep/rooms/notes/drawers",
        Some(json!({ "content": "a zebra drawn by hand" })),
        None,
    )
    .await;

    let (_, all) = call(&daemon, Method::GET, "/api/search?q=zebra", None, None).await;
    assert_eq!(all.as_array().unwrap().len(), 2, "{all}");

    let (status, only_notes) = call(
        &daemon,
        Method::GET,
        "/api/search?q=zebra&source_kind=note",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{only_notes}");
    let hits = only_notes.as_array().unwrap();
    assert_eq!(hits.len(), 1, "{only_notes}");
    assert_eq!(hits[0]["content"], "the zebra crossing is on Main Street");
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_same_note_twice_is_found_and_not_stored_again() {
    let daemon = TestDaemon::start().await;
    let (_, first) = note(&daemon, a_note("call the plumber")).await;

    let (status, second) = note(&daemon, a_note("call the plumber")).await;

    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["created"], false);
    assert_eq!(second["id"], first["id"]);
    let (_, drawers) = call(
        &daemon,
        Method::GET,
        "/api/wings/keep/rooms/notes/drawers",
        None,
        None,
    )
    .await;
    assert_eq!(drawers.as_array().unwrap().len(), 1, "{drawers}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_blank_note_or_an_unusable_name_is_refused_and_nothing_is_stored() {
    let daemon = TestDaemon::start().await;

    let (status, body) = note(&daemon, a_note("  \n ")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "memcastle::input::invalid");

    let (status, body) = note(
        &daemon,
        json!({ "wing": "a/b", "room": "notes", "content": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "memcastle::palace::path_invalid");

    let (_, wings) = call(&daemon, Method::GET, "/api/wings", None, None).await;
    assert!(wings.as_array().unwrap().is_empty(), "{wings}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_note_without_a_stated_channel_is_recorded_as_http() {
    let daemon = TestDaemon::start().await;
    let (_, stored) = note(
        &daemon,
        json!({ "wing": "keep", "room": "notes", "content": "x" }),
    )
    .await;
    assert_eq!(stored["provenance"]["requested_by"], "http");
    assert_eq!(stored["source"]["uri"], Value::Null);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_or_disabled_session_cannot_write_a_note() {
    let daemon = TestDaemon::start().await;
    for mode in ["read_only", "disabled"] {
        let (status, body) = call(
            &daemon,
            Method::POST,
            "/api/notes",
            Some(a_note("never stored")),
            Some(mode),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{mode}: {body}");
        assert_eq!(body["code"], "memcastle::mode::forbidden");
    }
    let (_, wings) = call(&daemon, Method::GET, "/api/wings", None, None).await;
    assert!(wings.as_array().unwrap().is_empty(), "{wings}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_note_is_read_for_entities_although_it_was_not_mined() {
    let daemon = TestDaemon::start_configured(|config| {
        config.extraction.provider = ExtractionProvider::Heuristic;
    })
    .await;
    let (_, stored) = note(
        &daemon,
        a_note("Ada Lovelace works on MemCastle. MemCastle depends on `surrealdb`."),
    )
    .await;
    let drawer = stored["id"].as_str().unwrap();

    // Extraction is a background sweep queued by the write: wait for the entity to appear.
    let mut found = Value::Null;
    for _ in 0..200 {
        let (_, entities) = call(
            &daemon,
            Method::GET,
            "/api/entities?name=Ada%20Lovelace",
            None,
            None,
        )
        .await;
        if let Some(entity) = entities.as_array().and_then(|all| all.first()) {
            found = entity.clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found.is_object(), "the note was never extracted");

    // The fact points back at the note, and the note is untouched.
    let id = found["id"].as_str().unwrap();
    let (_, edges) = call(
        &daemon,
        Method::GET,
        &format!("/api/entities/{id}/relationships"),
        None,
        None,
    )
    .await;
    let edge = edges
        .as_array()
        .unwrap()
        .iter()
        .find(|edge| edge["predicate"] == "works_on")
        .unwrap_or_else(|| panic!("no works_on edge: {edges}"));
    assert_eq!(edge["provenance"]["drawer"], drawer);
    assert_eq!(
        edge["provenance"]["origin"],
        Value::Null,
        "a note has no document"
    );
    // The fact holds from when the note was captured.
    assert_eq!(edge["valid_from"], stored["valid_from"]);
    daemon.shutdown().await;
}
