//! End-to-end retrieval: semantic, hybrid, scoped, temporal and graph-aware
//! search against a real daemon, with embeddings from a local OpenAI-compatible
//! stub (no model, no network).
//!
//! The stub embeds a text as a bag of hashed words, so two texts that share
//! vocabulary are close and unrelated ones are orthogonal: enough to prove
//! ranking follows the vectors without a model.

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::routing::post;
use memcastle::config::EmbeddingProvider;
use serde_json::{Value, json};

mod common;

use common::TestDaemon;

const DIMENSION: usize = 768;

/// A deterministic unit vector for `text`: each word lights one slot.
fn embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIMENSION];
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        // A tiny FNV-1a: stable across runs and platforms, unlike `DefaultHasher`.
        let mut hash: u32 = 0x811c_9dc5;
        for byte in word.to_lowercase().bytes() {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        v[(hash as usize) % DIMENSION] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    } else {
        v[0] = 1.0;
    }
    v
}

/// Serve `/v1/embeddings` on an ephemeral port; returns its base URL.
async fn embedding_stub() -> String {
    let app = Router::new().route(
        "/v1/embeddings",
        post(|Json(body): Json<Value>| async move {
            let data: Vec<Value> = body["input"]
                .as_array()
                .expect("input array")
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    json!({ "index": index, "embedding": embed(text.as_str().unwrap_or_default()) })
                })
                .collect();
            Json(json!({ "data": data }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{port}/v1")
}

async fn daemon_with_embeddings(url: &str) -> TestDaemon {
    let url = url.to_string();
    TestDaemon::start_configured(move |config| {
        config.embeddings.provider = EmbeddingProvider::Http;
        config.embeddings.url = Some(url);
        config.embeddings.model = Some("stub".into());
    })
    .await
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// File `content` as a drawer and return it.
async fn write(base: &str, wing: &str, room: &str, content: &str) -> Value {
    let response = client()
        .post(format!("{base}/api/wings/{wing}/rooms/{room}/drawers"))
        .json(&json!({ "content": content }))
        .send()
        .await
        .expect("create drawer");
    assert!(response.status().is_success(), "{}", response.status());
    response.json().await.expect("drawer json")
}

async fn search(base: &str, params: &[(&str, &str)]) -> (u16, Value) {
    let response = client()
        .get(format!("{base}/api/search"))
        .query(params)
        .send()
        .await
        .expect("search");
    let status = response.status().as_u16();
    (status, response.json().await.expect("json"))
}

fn contents(hits: &Value) -> Vec<String> {
    hits.as_array()
        .expect("a JSON array of hits")
        .iter()
        .map(|hit| hit["content"].as_str().expect("content").to_string())
        .collect()
}

/// Poll `ranking=semantic` until `expected` shows up: the embedding sweep runs
/// in the background after a write.
async fn wait_for_semantic(base: &str, query: &str, expected: &str) {
    for _ in 0..300 {
        let (status, hits) = search(base, &[("q", query), ("ranking", "semantic")]).await;
        if status == 200 && contents(&hits).iter().any(|c| c == expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("`{expected}` never became semantically searchable for `{query}`");
}

#[tokio::test]
async fn a_new_drawer_becomes_semantically_searchable_without_anyone_asking() {
    let daemon = daemon_with_embeddings(&embedding_stub().await).await;
    let base = &daemon.base_url;
    write(
        base,
        "w",
        "r",
        "the quarterly budget was approved by finance",
    )
    .await;
    write(base, "w", "r", "bring snacks for the garden party").await;

    wait_for_semantic(
        base,
        "finance approved budget",
        "the quarterly budget was approved by finance",
    )
    .await;

    // The stub shares no words with the garden drawer, so it ranks below.
    let (_, hits) = search(
        base,
        &[("q", "finance approved budget"), ("ranking", "semantic")],
    )
    .await;
    assert_eq!(
        contents(&hits)[0],
        "the quarterly budget was approved by finance"
    );
    // A vector never goes over the wire; the score and its evidence do.
    let first = &hits[0];
    assert!(first.get("embedding").is_none_or(Value::is_null), "{first}");
    assert!(
        first["signals"]["semantic"].as_f64().unwrap() > 0.5,
        "{first}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn auto_ranking_is_hybrid_when_a_provider_exists_and_finds_what_words_alone_would_miss() {
    let daemon = daemon_with_embeddings(&embedding_stub().await).await;
    let base = &daemon.base_url;
    write(base, "w", "r", "deploys happen on tuesday afternoons").await;
    for i in 0..4 {
        write(base, "w", "r", &format!("unrelated filler number {i}")).await;
    }
    wait_for_semantic(
        base,
        "tuesday deploys",
        "deploys happen on tuesday afternoons",
    )
    .await;

    let (status, hits) = search(base, &[("q", "tuesday deploys")]).await;
    assert_eq!(status, 200);
    let first = &hits[0];
    assert_eq!(first["content"], "deploys happen on tuesday afternoons");
    assert!(
        first["signals"]["lexical"].is_number() && first["signals"]["semantic"].is_number(),
        "auto must use both legs: {first}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_semantic_search_without_any_vector_is_refused_and_auto_falls_back_to_lexical() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    write(base, "w", "r", "plain lexical memory about lighthouses").await;

    let (status, body) = search(base, &[("q", "lighthouses"), ("ranking", "semantic")]).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "memcastle::search::semantic_unavailable");
    assert!(
        body["help"]
            .as_str()
            .is_some_and(|h| h.contains("query_embedding"))
    );

    let (status, hits) = search(base, &[("q", "lighthouses")]).await;
    assert_eq!(status, 200);
    assert_eq!(contents(&hits), ["plain lexical memory about lighthouses"]);

    daemon.shutdown().await;
}

#[tokio::test]
async fn caller_supplied_vectors_work_against_a_daemon_with_no_provider() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let drawer = write(base, "w", "r", "vectors supplied by the client").await;
    let id = drawer["id"].as_str().expect("drawer id");

    let put = client()
        .put(format!("{base}/api/drawers/{id}/embedding"))
        .json(&json!({ "embedding": embed("client side model") }))
        .send()
        .await
        .expect("put embedding");
    assert!(put.status().is_success(), "{}", put.status());

    let hits: Value = client()
        .post(format!("{base}/api/search"))
        .json(&json!({
            "text": "anything at all",
            "ranking": "semantic",
            "query_embedding": embed("client side model"),
        }))
        .send()
        .await
        .expect("post search")
        .json()
        .await
        .expect("json");
    assert_eq!(contents(&hits), ["vectors supplied by the client"]);

    // A wrong-length vector is the caller's mistake and says what length is expected.
    let bad = client()
        .put(format!("{base}/api/drawers/{id}/embedding"))
        .json(&json!({ "embedding": [1.0, 2.0] }))
        .send()
        .await
        .expect("bad put");
    assert_eq!(bad.status().as_u16(), 400);
    let body: Value = bad.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("768"), "{body}");

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_provider_that_is_down_degrades_auto_to_lexical_but_fails_an_explicit_semantic_search() {
    // Nothing listens on port 1: every embedding call is refused.
    let daemon = daemon_with_embeddings("http://127.0.0.1:1/v1").await;
    let base = &daemon.base_url;
    write(base, "w", "r", "resilience matters for lexical fallback").await;

    let (status, hits) = search(base, &[("q", "resilience")]).await;
    assert_eq!(status, 200, "{hits}");
    assert_eq!(contents(&hits), ["resilience matters for lexical fallback"]);

    let (status, body) = search(base, &[("q", "resilience"), ("ranking", "semantic")]).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["code"], "memcastle::embed::failed");

    daemon.shutdown().await;
}

#[tokio::test]
async fn superseding_a_drawer_changes_what_current_search_finds_but_history_keeps_it() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let old = write(base, "w", "r", "the release train leaves on friday").await;
    let id = old["id"].as_str().unwrap();
    // Filler so BM25 has something to discriminate against.
    for i in 0..5 {
        write(base, "w", "r", &format!("unrelated filler number {i}")).await;
    }
    let before = chrono_now_minus(1);
    // An instant strictly between the drawer's creation and its supersession.
    tokio::time::sleep(Duration::from_millis(60)).await;
    let between = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    tokio::time::sleep(Duration::from_millis(60)).await;

    let outcome: Value = client()
        .post(format!("{base}/api/drawers/{id}/supersede"))
        .json(&json!({ "content": "the release train leaves on monday" }))
        .send()
        .await
        .expect("supersede")
        .json()
        .await
        .expect("json");
    assert!(outcome["superseded"]["valid_to"].is_string(), "{outcome}");
    assert_eq!(
        outcome["superseded"]["content"], "the release train leaves on friday",
        "the old content is never rewritten"
    );

    let (_, current) = search(base, &[("q", "release train")]).await;
    assert_eq!(contents(&current), ["the release train leaves on monday"]);

    let (_, everything) = search(
        base,
        &[("q", "release train"), ("include_historical", "true")],
    )
    .await;
    assert_eq!(contents(&everything).len(), 2);

    // A point in time before the correction sees the old belief...
    let (_, then) = search(base, &[("q", "release train"), ("as_of", &between)]).await;
    assert_eq!(contents(&then), ["the release train leaves on friday"]);
    // ...and before the drawer existed there was nothing to find.
    let (_, earlier) = search(base, &[("q", "release train"), ("as_of", &before)]).await;
    assert!(contents(&earlier).is_empty(), "{earlier}");

    // Superseding again is a conflict that says why.
    let again = client()
        .post(format!("{base}/api/drawers/{id}/supersede"))
        .json(&json!({}))
        .send()
        .await
        .expect("again");
    assert_eq!(again.status().as_u16(), 409);
    let body: Value = again.json().await.unwrap();
    assert_eq!(body["code"], "memcastle::palace::drawer_superseded");

    daemon.shutdown().await;
}

/// An RFC 3339 instant `minutes` in the past, in the one form the API parses.
fn chrono_now_minus(minutes: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(minutes))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[tokio::test]
async fn expansion_adds_drawers_related_through_a_shared_entity_after_the_direct_hits() {
    let daemon = TestDaemon::start().await;
    let base = &daemon.base_url;
    let seed = write(base, "w", "r", "zebrafish keeper rota for the aquarium").await;
    let related = write(base, "w", "r", "supplier invoice for the new tank").await;
    for i in 0..4 {
        write(base, "w", "r", &format!("unrelated filler number {i}")).await;
    }
    for drawer in [&seed, &related] {
        let id = drawer["id"].as_str().unwrap();
        let link = client()
            .post(format!("{base}/api/drawers/{id}/mentions"))
            .json(&json!({ "name": "Aquarium", "kind": "place" }))
            .send()
            .await
            .expect("mention");
        assert!(link.status().is_success(), "{}", link.status());
    }

    let (_, plain) = search(base, &[("q", "zebrafish")]).await;
    assert_eq!(contents(&plain), ["zebrafish keeper rota for the aquarium"]);

    let (_, expanded) = search(base, &[("q", "zebrafish"), ("expand", "true")]).await;
    assert_eq!(
        contents(&expanded),
        [
            "zebrafish keeper rota for the aquarium",
            "supplier invoice for the new tank"
        ],
        "the direct hit leads and the graph-reached drawer follows"
    );
    assert_eq!(expanded[1]["via"], json!(["Aquarium"]));
    assert!(expanded[1]["signals"]["graph"].as_f64().unwrap() >= 1.0);
    // The related drawer is returned verbatim, as stored.
    assert_eq!(expanded[1]["content"], "supplier invoice for the new tank");

    daemon.shutdown().await;
}

#[tokio::test]
async fn an_embedding_job_can_be_requested_and_a_daemon_without_a_provider_refuses_it() {
    let plain = TestDaemon::start().await;
    let refused = client()
        .post(format!("{}/api/jobs", plain.base_url))
        .json(&json!({ "type": "embed" }))
        .send()
        .await
        .expect("submit");
    assert_eq!(refused.status().as_u16(), 400);
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["code"], "memcastle::embed::not_configured");
    plain.shutdown().await;

    let daemon = daemon_with_embeddings(&embedding_stub().await).await;
    let base = &daemon.base_url;
    write(base, "w", "r", "sweep me please").await;
    let job: Value = client()
        .post(format!("{base}/api/jobs"))
        .json(&json!({ "type": "embed", "wing": "w" }))
        .send()
        .await
        .expect("submit")
        .json()
        .await
        .expect("json");
    assert_eq!(job["kind"]["type"], "embed");
    wait_for_semantic(base, "sweep me please", "sweep me please").await;
    daemon.shutdown().await;
}
