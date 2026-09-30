//! Optional bearer-token authentication, against an in-process daemon and real HTTP
//! (`docs/adr/014-optional-token-authentication.md`).
//!
//! What restarting does to the verifier, and what the CLI and the daemon's log show, need a separate process
//! (SurrealKV's file lock outlives an in-process restart) and live in `tests/auth_lifecycle.rs`.

mod common;

use common::TestDaemon;
use memcastle::config::Secret;
use reqwest::{Method, StatusCode};
use rmcp::model::ClientConfig;
use rmcp::service::serve_client;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use serde_json::Value;

const SECRET: &str = "mc_a_shared_secret_for_the_tests_0123456789";

/// A daemon that requires a token, accepting `SECRET` as the configured shared secret.
async fn authenticated_daemon() -> TestDaemon {
    TestDaemon::start_configured(|config| {
        config.auth.enabled = true;
        config.auth.token = Some(Secret::new(SECRET));
    })
    .await
}

async fn send(
    daemon: &TestDaemon,
    method: Method,
    path: &str,
    token: Option<&str>,
) -> (StatusCode, reqwest::header::HeaderMap, Value) {
    let mut request = reqwest::Client::new().request(method, format!("{}{path}", daemon.base_url));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.expect("request");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, headers, body)
}

#[tokio::test]
async fn a_daemon_without_authentication_needs_no_token() {
    let daemon = TestDaemon::start().await;

    let (status, _, body) = send(&daemon, Method::GET, "/api/status", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_enabled"], false);
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_enabled_daemon_refuses_a_request_with_no_token_and_says_why() {
    let daemon = authenticated_daemon().await;

    let (status, headers, body) = send(&daemon, Method::GET, "/api/status", None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "memcastle::auth::unauthorized");
    assert!(
        body["help"]
            .as_str()
            .is_some_and(|h| h.contains("MEMCASTLE_AUTH_TOKEN"))
    );
    assert_eq!(headers["www-authenticate"], "Bearer");
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn a_wrong_or_malformed_token_is_refused_and_never_echoed_back() {
    let daemon = authenticated_daemon().await;

    let (status, _, body) = send(&daemon, Method::GET, "/api/status", Some("mc_wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.to_string().contains("mc_wrong"), "{body}");

    let response = reqwest::Client::new()
        .get(format!("{}/api/status", daemon.base_url))
        .header("authorization", "Basic dXNlcjpwYXNz")
        .send()
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.expect("json");
    assert!(
        body["error"].as_str().unwrap().contains("malformed"),
        "{body}"
    );

    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_configured_shared_secret_is_accepted() {
    let daemon = authenticated_daemon().await;

    let (status, _, body) = send(&daemon, Method::GET, "/api/status", Some(SECRET)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auth_enabled"], true);
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_liveness_probe_stays_open_so_an_unreachable_daemon_is_not_confused_with_a_refused_one()
{
    let daemon = authenticated_daemon().await;

    let (status, _, body) = send(&daemon, Method::GET, "/api/health", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn every_other_route_is_guarded_including_ones_that_do_not_exist() {
    let daemon = authenticated_daemon().await;
    let routes = [
        (Method::GET, "/api/status"),
        (Method::GET, "/api/search?q=x"),
        (Method::GET, "/api/recall?q=x"),
        (Method::GET, "/api/wake-up?agent_identity=a"),
        (Method::GET, "/api/diary"),
        (Method::POST, "/api/diary"),
        (Method::GET, "/api/jobs"),
        (Method::POST, "/api/jobs"),
        (Method::GET, "/api/jobs/x"),
        (Method::POST, "/api/jobs/x/pause"),
        (Method::POST, "/api/jobs/x/resume"),
        (Method::POST, "/api/jobs/x/cancel"),
        (Method::POST, "/api/jobs/x/retry"),
        (Method::POST, "/api/shutdown"),
        (Method::POST, "/api/auth/token"),
        (Method::DELETE, "/api/auth/token"),
        (Method::POST, "/mcp"),
        // A path no route serves must not answer 404 to an anonymous caller,
        // or the router's shape could be probed without a token.
        (Method::GET, "/api/not-a-route"),
        // Only `GET /api/health` is open: another method on it is not.
        (Method::POST, "/api/health"),
    ];

    for (method, path) in routes {
        let (status, _, _) = send(&daemon, method.clone(), path, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} was not guarded"
        );
    }

    // Nothing above was allowed through, so the daemon is still up to be stopped.
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn an_anonymous_shutdown_request_cannot_stop_an_authenticated_daemon() {
    let daemon = authenticated_daemon().await;

    let (status, _, _) = send(&daemon, Method::POST, "/api/shutdown", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send(&daemon, Method::GET, "/api/health", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the refused shutdown must not have stopped it"
    );
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn while_disabled_anyone_can_generate_a_token_and_it_is_returned_once_and_uncached() {
    let daemon = TestDaemon::start().await;

    let (status, headers, body) = send(&daemon, Method::POST, "/api/auth/token", None).await;

    assert_eq!(status, StatusCode::OK);
    let token = body["token"]
        .as_str()
        .expect("the token is in the response");
    assert!(token.starts_with("mc_"), "{token}");
    assert_eq!(token.len(), "mc_".len() + 64);
    assert_eq!(headers["cache-control"], "no-store");

    // It is not repeated anywhere afterwards.
    let (_, _, status_body) = send(&daemon, Method::GET, "/api/status", None).await;
    assert!(!status_body.to_string().contains(token));
    daemon.shutdown().await;
}

#[tokio::test]
async fn generating_a_token_needs_a_valid_token_once_authentication_is_enabled() {
    let daemon = authenticated_daemon().await;

    let (status, _, _) = send(&daemon, Method::POST, "/api/auth/token", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, body) = send(&daemon, Method::POST, "/api/auth/token", Some(SECRET)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["token"].as_str().is_some());
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn a_generated_token_works_rotation_replaces_it_and_revocation_ends_it() {
    let daemon = authenticated_daemon().await;

    let (_, _, first) = send(&daemon, Method::POST, "/api/auth/token", Some(SECRET)).await;
    let first = first["token"].as_str().unwrap().to_string();
    let (status, _, _) = send(&daemon, Method::GET, "/api/status", Some(&first)).await;
    assert_eq!(status, StatusCode::OK, "a generated token is accepted");

    // Rotation: authenticate with the old one to mint the new one.
    let (status, _, second) = send(&daemon, Method::POST, "/api/auth/token", Some(&first)).await;
    assert_eq!(status, StatusCode::OK);
    let second = second["token"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    let (status, _, _) = send(&daemon, Method::GET, "/api/status", Some(&first)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the rotated-out token must stop working"
    );
    let (status, _, _) = send(&daemon, Method::GET, "/api/status", Some(&second)).await;
    assert_eq!(status, StatusCode::OK);

    // Revocation takes effect at once, with no restart.
    let (status, _, body) = send(&daemon, Method::DELETE, "/api/auth/token", Some(&second)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revoked"], true);
    let (status, _, _) = send(&daemon, Method::GET, "/api/status", Some(&second)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The configured secret lives outside the daemon's state and is unaffected.
    let (status, _, _) = send(&daemon, Method::GET, "/api/status", Some(SECRET)).await;
    assert_eq!(status, StatusCode::OK);
    daemon.shutdown_as(Some(SECRET)).await;
}

/// The names of every tool an MCP session against `daemon` can see.
async fn tool_names(daemon: &TestDaemon, token: Option<&str>) -> Vec<String> {
    let mut config =
        StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", daemon.base_url));
    if let Some(token) = token {
        config = config.auth_header(token);
    }
    let session = serve_client(
        ClientConfig::default(),
        StreamableHttpClientTransport::from_config(config),
    )
    .await
    .expect("mcp session initializes");
    let tools = session.peer().list_all_tools().await.expect("tools/list");
    session.cancel().await.expect("close session");
    tools.iter().map(|tool| tool.name.to_string()).collect()
}

#[tokio::test]
async fn mcp_with_a_valid_token_works_and_offers_no_credential_management() {
    let daemon = authenticated_daemon().await;

    let names = tool_names(&daemon, Some(SECRET)).await;

    assert!(names.iter().any(|n| n == "memcastle_status"), "{names:?}");
    // The exclusion is structural, not a convention: no tool may so much as
    // mention credentials, whatever it is called.
    let forbidden = ["auth", "token", "credential", "secret", "revoke", "rotate"];
    for name in &names {
        assert!(
            !forbidden.iter().any(|word| name.contains(word)),
            "`{name}` looks like credential management, which must never be an MCP tool"
        );
    }
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn mcp_offers_no_credential_management_when_authentication_is_off_either() {
    let daemon = TestDaemon::start().await;

    let names = tool_names(&daemon, None).await;

    assert!(
        !names
            .iter()
            .any(|n| n.contains("auth") || n.contains("token")),
        "{names:?}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn mcp_without_a_token_cannot_even_initialize() {
    let daemon = authenticated_daemon().await;

    let session = serve_client(
        ClientConfig::default(),
        StreamableHttpClientTransport::from_uri(format!("{}/mcp", daemon.base_url)),
    )
    .await;

    assert!(session.is_err(), "an anonymous MCP session must be refused");
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn mcp_with_a_wrong_token_is_refused_too() {
    let daemon = authenticated_daemon().await;

    let config = StreamableHttpClientTransportConfig::with_uri(format!("{}/mcp", daemon.base_url))
        .auth_header("mc_not_the_token");
    let session = serve_client(
        ClientConfig::default(),
        StreamableHttpClientTransport::from_config(config),
    )
    .await;

    assert!(
        session.is_err(),
        "a wrong token must not open an MCP session"
    );
    daemon.shutdown_as(Some(SECRET)).await;
}
