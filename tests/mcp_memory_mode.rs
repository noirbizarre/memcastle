//! Proves memory mode over MCP is negotiated **per session**, via
//! `memcastle_set_mode`, not globally: one MCP session that calls
//! `memcastle_set_mode { mode: "disabled" }` must have every subsequent
//! gated tool call on *that* session rejected, while a second, independent
//! MCP session against the very same daemon — which never called
//! `memcastle_set_mode` — keeps its default `Full` behavior. This is the
//! integration-level proof that `McpTools::modes` is keyed per session, not
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
