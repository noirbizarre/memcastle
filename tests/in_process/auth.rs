//! Optional bearer-token authentication, against an in-process daemon and real HTTP
//! (`docs/adr/014-optional-token-authentication.md`).
//!
//! What restarting does to the verifier, and what the CLI and the daemon's log show, need a separate process
//! (SurrealKV's file lock outlives an in-process restart) and live in `tests/auth_lifecycle.rs`.

use crate::common;

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
        // The JSON forms of search and recall, which carry query vectors.
        (Method::POST, "/api/search"),
        (Method::POST, "/api/recall"),
        (Method::GET, "/api/wake-up?agent_identity=a"),
        (Method::GET, "/api/diary"),
        (Method::POST, "/api/diary"),
        (Method::POST, "/api/notes"),
        (Method::GET, "/api/jobs"),
        (Method::POST, "/api/jobs"),
        (Method::GET, "/api/jobs/x"),
        (Method::POST, "/api/jobs/x/pause"),
        (Method::POST, "/api/jobs/x/resume"),
        (Method::POST, "/api/jobs/x/cancel"),
        (Method::POST, "/api/jobs/x/retry"),
        // The mining sources: what was mined and where each run stopped.
        (Method::GET, "/api/sources"),
        // Installing, enabling and removing sources decides what code the daemon runs: every method is guarded.
        (Method::POST, "/api/source-packages"),
        (Method::GET, "/api/source-packages/x"),
        (Method::DELETE, "/api/source-packages/x"),
        (Method::POST, "/api/source-packages/x/enable"),
        (Method::POST, "/api/source-packages/x/disable"),
        // Signing a source in with OAuth lets installed code act on an account (docs/adr/039): both routes are guarded.
        (Method::POST, "/api/source-packages/x/auth"),
        (Method::POST, "/api/source-packages/x/auth/y/wait"),
        // Registries make the daemon fetch code from elsewhere and install it (docs/adr/033): every route is guarded.
        (Method::GET, "/api/source-registry/search"),
        (Method::GET, "/api/source-registry/sources/x"),
        (Method::POST, "/api/source-registry/install"),
        (Method::GET, "/api/source-registry/updates"),
        (Method::POST, "/api/source-registry/update"),
        // Miner configuration decides what the daemon mines: reading and every change are guarded (docs/adr/037).
        (Method::GET, "/api/miners"),
        (Method::POST, "/api/miners/reload"),
        (Method::GET, "/api/miners/x"),
        (Method::PUT, "/api/miners/x"),
        (Method::DELETE, "/api/miners/x"),
        (Method::POST, "/api/miners/x/enable"),
        (Method::POST, "/api/miners/x/disable"),
        (Method::POST, "/api/miners/x/run"),
        (Method::POST, "/api/shutdown"),
        (Method::POST, "/api/auth/token"),
        (Method::DELETE, "/api/auth/token"),
        // Opening a database console onto the palace is the most sensitive admin
        // operation there is: all three methods are guarded like the rest.
        (Method::GET, "/api/db"),
        (Method::POST, "/api/db"),
        (Method::DELETE, "/api/db"),
        // The hierarchy: reads, creates and the destructive deletes alike.
        (Method::GET, "/api/wings"),
        (Method::POST, "/api/wings"),
        (Method::GET, "/api/wings/w"),
        (Method::DELETE, "/api/wings/w"),
        (Method::GET, "/api/wings/w/rooms"),
        (Method::POST, "/api/wings/w/rooms"),
        (Method::GET, "/api/wings/w/rooms/r"),
        (Method::DELETE, "/api/wings/w/rooms/r"),
        (Method::GET, "/api/wings/w/rooms/r/drawers"),
        (Method::POST, "/api/wings/w/rooms/r/drawers"),
        (Method::GET, "/api/wings/w/rooms/r/drawers/d"),
        (Method::DELETE, "/api/wings/w/rooms/r/drawers/d"),
        // Corrective and derived writes on one drawer, by id.
        (Method::POST, "/api/drawers/x/supersede"),
        (Method::PUT, "/api/drawers/x/embedding"),
        (Method::POST, "/api/drawers/x/mentions"),
        (Method::GET, "/api/drawers/x/duplicates"),
        // History returns every version of a drawer's chain, so it exposes content like a read of the drawer.
        (Method::GET, "/api/drawers/x/history"),
        // The knowledge graph: read-only apart from settling a name by hand.
        (Method::GET, "/api/entities"),
        // The dashboard's additions: the graph in one answer and the configuration in effect.
        (Method::GET, "/api/graph"),
        (Method::GET, "/api/config"),
        (Method::GET, "/api/jobs?kind=mine&limit=5"),
        // `/ui` is guarded unless the dashboard is enabled (see `web.rs` for the enabled case): a daemon with no
        // dashboard must not answer it anonymously, nor reveal that it has none.
        (Method::GET, "/ui"),
        (Method::GET, "/ui/"),
        (Method::GET, "/ui/index.html"),
        (Method::GET, "/api/entities/x/relationships"),
        (Method::GET, "/api/entities/x/mentions"),
        (Method::GET, "/api/entities/x/candidates"),
        (Method::POST, "/api/entities/x/aliases"),
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
async fn mcp_offers_no_way_to_open_the_database_console() {
    let daemon = TestDaemon::start().await;

    let names = tool_names(&daemon, None).await;

    // Like credentials, the admin endpoint (`docs/adr/015`) is REST and CLI only:
    // an agent integration must not be able to expose the palace's database.
    let forbidden = ["db", "database", "surreal", "console", "endpoint", "sql"];
    for name in &names {
        assert!(
            !forbidden.iter().any(|word| name.contains(word)),
            "`{name}` looks like database access, which must never be an MCP tool"
        );
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn mcp_offers_no_way_to_install_or_change_a_mining_source() {
    let daemon = TestDaemon::start().await;

    let names = tool_names(&daemon, None).await;

    // Installing a source decides what code the daemon runs and what that code may read (`docs/adr/026`): like
    // credentials and the database console, it is REST and CLI only, so an agent cannot widen its own reach.
    let forbidden = [
        "install",
        "package",
        "enable",
        "disable",
        "uninstall",
        "permission",
        "consent",
        // Registries (docs/adr/033): an agent must not make the daemon fetch code, look for it or update it.
        "registry",
        "update",
        "trust",
        "sign",
        // Signing a source in with OAuth (docs/adr/039): an agent must not start a sign-in either.
        "oauth",
    ];
    for name in &names {
        assert!(
            !forbidden.iter().any(|word| name.contains(word)),
            "`{name}` looks like source management, which must never be an MCP tool"
        );
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn mcp_can_read_miners_but_offers_no_way_to_change_one() {
    let daemon = TestDaemon::start().await;

    let names = tool_names(&daemon, None).await;

    // What the daemon mines is the user's decision, like what code it runs (`docs/adr/037`): an agent may read the
    // miners, so it can say what is configured, but nothing it can call adds, changes, enables, removes or runs one.
    let mut miner_tools: Vec<&String> = names.iter().filter(|n| n.contains("miner")).collect();
    miner_tools.sort();
    assert_eq!(
        miner_tools,
        ["memcastle_miner_get", "memcastle_miner_list"],
        "only the two read-only miner tools may exist"
    );
    for name in &names {
        assert!(
            ![
                "miner_set",
                "miner_add",
                "miner_enable",
                "miner_disable",
                "miner_remove",
                "miner_run",
                "miner_reload"
            ]
            .iter()
            .any(|word| name.contains(word)),
            "`{name}` looks like miner management, which must never be an MCP tool"
        );
    }
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
