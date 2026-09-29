//! Proves memory mode over MCP is negotiated **per session**, via
//! `memcastle_set_mode`, not globally: one MCP session that calls
//! `memcastle_set_mode { mode: "disabled" }` must have every subsequent
//! gated tool call on *that* session rejected, while a second, independent
//! MCP session against the very same daemon — which never called
//! `memcastle_set_mode` — keeps its default `Full` behavior. This is the
//! integration-level proof that `McpTools`'s mode is scoped to its session, not
//! a shared/global flag (see `src/mcp/mod.rs`'s doc comment).

mod common;

use common::TestDaemon;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService, serve_client};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;

/// Connect a fresh MCP session (its own `initialize` handshake, and
/// therefore its own `mcp-session-id`) to `base_url`.
async fn connect(base_url: &str) -> RunningService<RoleClient, rmcp::model::ClientConfig> {
    let transport = StreamableHttpClientTransport::from_uri(format!("{base_url}/mcp"));
    serve_client(rmcp::model::ClientConfig::default(), transport)
        .await
        .expect("mcp session initializes")
}

/// Call a tool with no arguments beyond what's passed, returning whether
/// the call was reported as an error (`CallToolResult::is_error`) — a
/// blocked call is a successful JSON-RPC round trip whose *result* carries
/// the rejection, not a transport-level failure (see
/// `McpTools`'s handlers, which always return `Ok(CallToolResult::error(..))`
/// for an `AppServices` error).
async fn call(
    client: &RunningService<RoleClient, rmcp::model::ClientConfig>,
    name: &'static str,
    arguments: serde_json::Value,
) -> bool {
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("arguments must be a JSON object");
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await
        .expect("tool call round-trips");
    result.is_error.unwrap_or(false)
}

/// Like [`call`], but also returns the tool's text output — needed to
/// prove a permitted call actually returned the palace's content, not
/// merely that it was not rejected.
async fn call_text(
    client: &RunningService<RoleClient, rmcp::model::ClientConfig>,
    name: &'static str,
    arguments: serde_json::Value,
) -> (bool, String) {
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("arguments must be a JSON object");
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await
        .expect("tool call round-trips");
    let text = serde_json::to_value(&result.content)
        .expect("content serializes")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (result.is_error.unwrap_or(false), text)
}

async fn set_mode(client: &RunningService<RoleClient, rmcp::model::ClientConfig>, mode: &str) {
    let failed = call(
        client,
        "memcastle_set_mode",
        serde_json::json!({ "mode": mode }),
    )
    .await;
    assert!(!failed, "memcastle_set_mode {mode} itself must succeed");
}

fn checkpoint_args(emergency: bool) -> serde_json::Value {
    serde_json::json!({
        "emergency": emergency,
        "payload": {
            "items": [{
                "destination": "general",
                "content": "an mcp checkpoint",
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "test" },
                "fact": null,
            }],
        },
    })
}

#[tokio::test]
async fn disabling_one_mcp_session_does_not_affect_another_session_on_the_same_daemon() {
    let daemon = TestDaemon::start().await;

    let disabled_session = connect(&daemon.base_url).await;
    let untouched_session = connect(&daemon.base_url).await;

    // Only the first session opts into `disabled`.
    let set_mode_failed = call(
        &disabled_session,
        "memcastle_set_mode",
        serde_json::json!({ "mode": "disabled" }),
    )
    .await;
    assert!(!set_mode_failed, "memcastle_set_mode itself must succeed");

    // Every gated tool on the disabled session is now rejected.
    let search_failed = call(
        &disabled_session,
        "memcastle_search",
        serde_json::json!({ "query": "anything" }),
    )
    .await;
    assert!(
        search_failed,
        "memcastle_search must be rejected on the disabled session"
    );

    let checkpoint_failed = call(
        &disabled_session,
        "memcastle_checkpoint",
        serde_json::json!({
            "payload": {
                "items": [{
                    "destination": "general",
                    "content": "should never be queued",
                    "tags": [],
                    "source": { "kind": "manual", "uri": null, "agent": "test" },
                    "fact": null,
                }],
            },
        }),
    )
    .await;
    assert!(
        checkpoint_failed,
        "memcastle_checkpoint must be rejected on the disabled session"
    );

    // The second session, which never called memcastle_set_mode, keeps its
    // default `Full` behavior on the very same daemon.
    let untouched_search_failed = call(
        &untouched_session,
        "memcastle_search",
        serde_json::json!({ "query": "anything" }),
    )
    .await;
    assert!(
        !untouched_search_failed,
        "an independent session that never called memcastle_set_mode must default to full"
    );

    disabled_session.cancel().await.expect("close session");
    untouched_session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_session_is_rejected_by_recall_wake_up_and_both_diary_tools() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    set_mode(&session, "disabled").await;

    for (tool, args) in [
        (
            "memcastle_recall",
            serde_json::json!({ "query": "anything" }),
        ),
        (
            "memcastle_wake_up",
            serde_json::json!({ "agent_identity": "agent-a" }),
        ),
        (
            "memcastle_diary_write",
            serde_json::json!({ "agent_identity": "agent-a", "wing": "w", "content": "c" }),
        ),
        (
            "memcastle_diary_read",
            serde_json::json!({ "agent_identity": "agent-a", "wing": "w" }),
        ),
    ] {
        assert!(
            call(&session, tool, args).await,
            "{tool} must be rejected on a disabled session"
        );
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_session_can_read_a_diary_entry_but_not_write_one() {
    let daemon = TestDaemon::start().await;
    let writer = connect(&daemon.base_url).await;
    let reader = connect(&daemon.base_url).await;

    let (failed, _) = call_text(
        &writer,
        "memcastle_diary_write",
        serde_json::json!({
            "agent_identity": "agent-a", "wing": "project-x", "content": "written over mcp",
        }),
    )
    .await;
    assert!(
        !failed,
        "a full-mode session must be able to write the diary"
    );

    set_mode(&reader, "read_only").await;

    let (failed, text) = call_text(
        &reader,
        "memcastle_diary_read",
        serde_json::json!({ "agent_identity": "agent-a", "wing": "project-x" }),
    )
    .await;
    assert!(!failed, "a read-only session may read the diary");
    assert!(
        text.contains("written over mcp"),
        "diary_read must return the stored entry verbatim, got: {text}"
    );

    let (failed, _) = call_text(
        &reader,
        "memcastle_diary_write",
        serde_json::json!({
            "agent_identity": "agent-a", "wing": "project-x", "content": "must be rejected",
        }),
    )
    .await;
    assert!(failed, "a read-only session must not write the diary");

    // The rejected write must not have reached storage.
    let (_, text) = call_text(
        &writer,
        "memcastle_diary_read",
        serde_json::json!({ "agent_identity": "agent-a", "wing": "project-x" }),
    )
    .await;
    assert!(
        !text.contains("must be rejected"),
        "a rejected diary write must not be persisted, got: {text}"
    );

    writer.cancel().await.expect("close session");
    reader.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_read_only_session_can_recall_and_wake_up() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    set_mode(&session, "read_only").await;

    for (tool, args) in [
        (
            "memcastle_recall",
            serde_json::json!({ "query": "anything" }),
        ),
        (
            "memcastle_wake_up",
            serde_json::json!({ "agent_identity": "agent-a" }),
        ),
    ] {
        assert!(
            !call(&session, tool, args).await,
            "{tool} is a read and must succeed on a read-only session"
        );
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn emergency_checkpoint_over_mcp_follows_the_sessions_mode() {
    let daemon = TestDaemon::start().await;
    let full = connect(&daemon.base_url).await;
    let read_only = connect(&daemon.base_url).await;
    set_mode(&read_only, "read_only").await;

    assert!(
        call(&read_only, "memcastle_checkpoint", checkpoint_args(true)).await,
        "an emergency checkpoint is a write and must be rejected on a read-only session"
    );
    assert!(
        !call(&full, "memcastle_checkpoint", checkpoint_args(true)).await,
        "an emergency checkpoint must be accepted on a full session"
    );

    full.cancel().await.expect("close session");
    read_only.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_mcp_job_list_and_mine_tools_follow_the_sessions_mode() {
    let daemon = TestDaemon::start().await;
    let disabled = connect(&daemon.base_url).await;
    let read_only = connect(&daemon.base_url).await;
    set_mode(&disabled, "disabled").await;
    set_mode(&read_only, "read_only").await;

    assert!(
        call(&disabled, "memcastle_jobs_list", serde_json::json!({})).await,
        "a disabled session must not read job records, which carry checkpoint content"
    );
    assert!(
        !call(&read_only, "memcastle_jobs_list", serde_json::json!({})).await,
        "a read-only session may read job records"
    );
    assert!(
        call(
            &read_only,
            "memcastle_mine",
            serde_json::json!({ "path": "/tmp" })
        )
        .await,
        "mining files drawers, so a read-only session must not submit it"
    );

    disabled.cancel().await.expect("close session");
    read_only.cancel().await.expect("close session");
    daemon.shutdown().await;
}

/// A session's chosen mode dies with the session: nothing about it lingers in
/// the daemon for a later session to inherit, however long the daemon lives.
#[tokio::test]
async fn a_closed_sessions_mode_does_not_carry_over_to_the_next_session() {
    let daemon = TestDaemon::start().await;

    let first = connect(&daemon.base_url).await;
    set_mode(&first, "disabled").await;
    assert!(
        call(
            &first,
            "memcastle_search",
            serde_json::json!({ "query": "x" })
        )
        .await
    );
    first.cancel().await.expect("close session");

    let second = connect(&daemon.base_url).await;
    assert!(
        !call(
            &second,
            "memcastle_search",
            serde_json::json!({ "query": "x" })
        )
        .await,
        "a new session must start as full, not as whatever a closed one chose"
    );

    second.cancel().await.expect("close session");
    daemon.shutdown().await;
}
