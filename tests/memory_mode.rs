//! Proves `MemoryMode` is enforced per-request over HTTP via the
//! `X-MemCastle-Mode` header — three simulated clients (`full`/`read_only`/
//! `disabled`) against one daemon, per the issue's test brief. The policy
//! itself (`allows_read`/`allows_write`, `Error::ModeForbidden`) is unit
//! tested in `src/domain/memory_mode.rs` and `src/app/mod.rs`; this only
//! exercises the wire boundary — header parsing and the resulting status
//! codes/response bodies — plus that a rejected write never actually lands.

mod common;

use common::TestDaemon;
use reqwest::StatusCode;

const HEADER: &str = "X-MemCastle-Mode";

fn checkpoint_body(content: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "checkpoint",
        "payload": {
            "items": [{
                "destination": "general",
                "content": content,
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "test" },
                "fact": null,
            }],
        },
        "requested_by": "test",
    })
}

async fn read_diary(
    client: &reqwest::Client,
    base_url: &str,
    agent_identity: &str,
    wing: &str,
) -> Vec<memcastle::domain::Drawer> {
    client
        .get(format!(
            "{base_url}/api/diary?agent_identity={agent_identity}&wing={wing}"
        ))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn a_full_mode_client_can_read_and_write() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    // No `X-MemCastle-Mode` header at all — the default-to-`Full` path
    // existing clients rely on.
    let write = client
        .post(format!("{}/api/diary", daemon.base_url))
        .json(&serde_json::json!({
            "agent_identity": "agent-a",
            "wing": "project-x",
            "content": "full mode works",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(write.status(), StatusCode::OK);

    let search = client
        .get(format!("{}/api/search?q=full", daemon.base_url))
        .send()
        .await
        .expect("request");
    assert_eq!(search.status(), StatusCode::OK);

    let recall = client
        .get(format!("{}/api/recall?q=full", daemon.base_url))
        .send()
        .await
        .expect("request");
    assert_eq!(recall.status(), StatusCode::OK);

    let wake_up = client
        .get(format!(
            "{}/api/wake-up?agent_identity=agent-a&wing=project-x",
            daemon.base_url
        ))
        .send()
        .await
        .expect("request");
    assert_eq!(wake_up.status(), StatusCode::OK);

    let checkpoint = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&checkpoint_body("a full-mode checkpoint"))
        .send()
        .await
        .expect("request");
    assert_eq!(checkpoint.status(), StatusCode::OK);

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_client_can_read_but_writes_are_rejected_with_403() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    // Every read-gated endpoint must still succeed.
    for url in [
        format!("{}/api/search?q=x", daemon.base_url),
        format!("{}/api/recall?q=x", daemon.base_url),
        format!("{}/api/wake-up?agent_identity=agent-a", daemon.base_url),
        format!(
            "{}/api/diary?agent_identity=agent-a&wing=project-x",
            daemon.base_url
        ),
    ] {
        let response = client
            .get(&url)
            .header(HEADER, "read_only")
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET {url} should succeed in read_only mode"
        );
    }

    // Both write-gated endpoints must be rejected.
    let diary_write = client
        .post(format!("{}/api/diary", daemon.base_url))
        .header(HEADER, "read_only")
        .json(&serde_json::json!({
            "agent_identity": "agent-a",
            "wing": "project-x",
            "content": "should be rejected",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(diary_write.status(), StatusCode::FORBIDDEN);

    let checkpoint = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .header(HEADER, "read_only")
        .json(&checkpoint_body("should never be queued"))
        .send()
        .await
        .expect("request");
    assert_eq!(checkpoint.status(), StatusCode::FORBIDDEN);

    // The rejected write must never have been persisted.
    let entries = read_diary(&client, &daemon.base_url, "agent-a", "project-x").await;
    assert!(
        entries.is_empty(),
        "a rejected read_only write must not persist anything, found {entries:?}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_client_gets_no_reads_and_no_writes() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    // A `Full`-mode client writes something real first, so the assertions
    // below prove a disabled client sees *nothing* — not just that its own
    // writes fail, but that an unrelated client's existing data never
    // reaches it either (the context-isolation guarantee, not just "no
    // mutation").
    let seed = client
        .post(format!("{}/api/diary", daemon.base_url))
        .json(&serde_json::json!({
            "agent_identity": "agent-a",
            "wing": "project-x",
            "content": "visible to full mode only",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(seed.status(), StatusCode::OK);

    for url in [
        format!("{}/api/search?q=visible", daemon.base_url),
        format!("{}/api/recall?q=visible", daemon.base_url),
        format!(
            "{}/api/wake-up?agent_identity=agent-a&wing=project-x",
            daemon.base_url
        ),
        format!(
            "{}/api/diary?agent_identity=agent-a&wing=project-x",
            daemon.base_url
        ),
    ] {
        let response = client
            .get(&url)
            .header(HEADER, "disabled")
            .send()
            .await
            .expect("request");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "GET {url} must be rejected in disabled mode"
        );
        let body = response.text().await.expect("body");
        assert!(
            !body.contains("visible to full mode only"),
            "a disabled response must never carry drawer content, got {body}"
        );
    }

    let diary_write = client
        .post(format!("{}/api/diary", daemon.base_url))
        .header(HEADER, "disabled")
        .json(&serde_json::json!({
            "agent_identity": "agent-a",
            "wing": "project-x",
            "content": "should never be written",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(diary_write.status(), StatusCode::FORBIDDEN);

    let checkpoint = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .header(HEADER, "disabled")
        .json(&checkpoint_body("should never be queued"))
        .send()
        .await
        .expect("request");
    assert_eq!(checkpoint.status(), StatusCode::FORBIDDEN);

    // A follow-up `Full`-mode read confirms the disabled client never
    // wrote anything — only the earlier seed write exists.
    let entries = read_diary(&client, &daemon.base_url, "agent-a", "project-x").await;
    assert_eq!(
        entries.len(),
        1,
        "only the full-mode seed write should exist, found {entries:?}"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn an_invalid_mode_header_value_is_rejected_not_defaulted_to_full() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/api/search?q=x", daemon.base_url))
        .header(HEADER, "bogus")
        .send()
        .await
        .expect("request");
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "an unparsable mode header must be a 400, never silently treated as full"
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_missing_mode_header_defaults_to_full() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/api/search?q=x", daemon.base_url))
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::OK);

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_client_cannot_read_checkpointed_content_through_the_job_endpoints() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();

    let submitted: memcastle::domain::Job = client
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&checkpoint_body("content only a full session may see"))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");

    let list = client
        .get(format!("{}/api/jobs", daemon.base_url))
        .header(HEADER, "disabled")
        .send()
        .await
        .expect("request");
    assert_eq!(list.status(), StatusCode::FORBIDDEN);
    let body = list.text().await.expect("body");
    assert!(
        !body.contains("only a full session"),
        "the rejection must not echo palace content: {body}"
    );

    let show = client
        .get(format!("{}/api/jobs/{}", daemon.base_url, submitted.id))
        .header(HEADER, "disabled")
        .send()
        .await
        .expect("request");
    assert_eq!(show.status(), StatusCode::FORBIDDEN);

    // The same job stays visible to a read-only client, which may already
    // read that content through search.
    let read_only = client
        .get(format!("{}/api/jobs/{}", daemon.base_url, submitted.id))
        .header(HEADER, "read_only")
        .send()
        .await
        .expect("request");
    assert_eq!(read_only.status(), StatusCode::OK);

    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_client_cannot_mine_or_apply_a_repair_but_can_dry_run_one() {
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let submit = |body: serde_json::Value| {
        client
            .post(format!("{}/api/jobs", daemon.base_url))
            .header(HEADER, "read_only")
            .json(&body)
            .send()
    };

    let mine =
        submit(serde_json::json!({ "type": "mine", "path": "/tmp", "requested_by": "test" }))
            .await
            .expect("request");
    assert_eq!(mine.status(), StatusCode::FORBIDDEN);

    let apply =
        submit(serde_json::json!({ "type": "repair", "dry_run": false, "requested_by": "test" }))
            .await
            .expect("request");
    assert_eq!(apply.status(), StatusCode::FORBIDDEN);

    let dry_run =
        submit(serde_json::json!({ "type": "repair", "dry_run": true, "requested_by": "test" }))
            .await
            .expect("request");
    assert_eq!(dry_run.status(), StatusCode::OK);

    daemon.shutdown().await;
}
