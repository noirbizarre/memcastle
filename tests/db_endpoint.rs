//! The database admin endpoint (`memcastle db start`), against an in-process daemon and a real
//! WebSocket (`docs/adr/015-database-admin-endpoint.md`).
//!
//! The Rust SDK is the `flatbuffers` client (the format SurrealDB's own tools use); a raw WebSocket
//! speaks JSON and CBOR and can set the `Origin` header the way a browser would.
//!
//! Behaviour that needs a separate process (the CLI, the SurrealKV file lock, the log) lives in
//! `tests/cli_daemon.rs` and `tests/auth_lifecycle.rs`.

mod common;

use common::TestDaemon;
use futures::{SinkExt, StreamExt};
use memcastle::config::Secret;
use reqwest::StatusCode;
use serde_json::{Value, json};
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

const SECRET: &str = "mc_a_shared_secret_for_the_tests_0123456789";

/// A daemon that requires a token, accepting `SECRET`.
async fn authenticated_daemon() -> TestDaemon {
    TestDaemon::start_configured(|config| {
        config.auth.enabled = true;
        config.auth.token = Some(Secret::new(SECRET));
    })
    .await
}

/// Ask the daemon to open the endpoint on an OS-chosen loopback port and return where it listens.
async fn open_endpoint(daemon: &TestDaemon, token: Option<&str>) -> String {
    let body = start_endpoint(daemon, token, json!({ "port": 0 })).await.1;
    body["addr"].as_str().expect("addr").to_string()
}

async fn start_endpoint(
    daemon: &TestDaemon,
    token: Option<&str>,
    request: Value,
) -> (StatusCode, Value) {
    let mut call = reqwest::Client::new()
        .post(format!("{}/api/db", daemon.base_url))
        .json(&request);
    if let Some(token) = token {
        call = call.bearer_auth(token);
    }
    let response = call.send().await.expect("request");
    (
        response.status(),
        response.json().await.unwrap_or(Value::Null),
    )
}

async fn daemon_json(daemon: &TestDaemon, path: &str, token: Option<&str>) -> Value {
    let mut call = reqwest::Client::new().get(format!("{}{path}", daemon.base_url));
    if let Some(token) = token {
        call = call.bearer_auth(token);
    }
    call.send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A raw JSON-RPC connection (no subprotocol, which means JSON).
async fn json_socket(addr: &str) -> Socket {
    tokio_tungstenite::connect_async(format!("ws://{addr}/rpc"))
        .await
        .expect("connect")
        .0
}

/// Send one JSON request and return the whole JSON response.
async fn call(socket: &mut Socket, method: &str, params: Value) -> Value {
    let request = json!({ "id": 1, "method": method, "params": params });
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .expect("send");
    loop {
        match socket.next().await.expect("a response").expect("frame") {
            Message::Text(text) => return serde_json::from_str(&text).expect("json"),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
}

/// The result rows of the first statement of a `query` response.
fn rows(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    let first = &response["result"][0];
    assert_eq!(first["status"], "OK", "{response}");
    &first["result"]
}

async fn sdk_client(addr: &str) -> surrealdb::Surreal<Any> {
    any::connect(format!("ws://{addr}")).await.expect("connect")
}

#[tokio::test]
async fn a_daemon_that_was_only_started_has_no_database_endpoint() {
    let daemon = TestDaemon::start().await;

    let status = daemon_json(&daemon, "/api/db", None).await;

    assert_eq!(status["running"], false);
    assert_eq!(status["namespace"], "memcastle");
    assert_eq!(status["database"], "palace");
    daemon.shutdown().await;
}

#[tokio::test]
async fn starting_the_endpoint_listens_on_loopback_and_answers_the_probes() {
    let daemon = TestDaemon::start().await;

    let addr = open_endpoint(&daemon, None).await;

    assert!(addr.starts_with("127.0.0.1:"), "{addr}");
    let health = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let version = reqwest::get(format!("http://{addr}/version"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(version.starts_with("surrealdb-"), "{version}");
    let status = daemon_json(&daemon, "/api/db", None).await;
    assert_eq!(status["running"], true);
    assert_eq!(status["url"], format!("ws://{addr}"));
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_query_over_the_endpoint_sees_what_the_daemon_wrote() {
    let daemon = TestDaemon::start().await;
    // The daemon writes a job through its own API...
    let submitted = reqwest::Client::new()
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&json!({ "type": "audit", "requested_by": "test" }))
        .send()
        .await
        .unwrap();
    assert!(submitted.status().is_success(), "{submitted:?}");
    let job_id = submitted.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let addr = open_endpoint(&daemon, None).await;

    // ...and Studio, on the endpoint, reads it back from the same live database.
    let mut socket = json_socket(&addr).await;
    let response = call(
        &mut socket,
        "query",
        json!(["SELECT record::id(id) AS id FROM job"]),
    )
    .await;

    let ids: Vec<&str> = rows(&response)
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert!(ids.contains(&job_id.as_str()), "{ids:?} lacks {job_id}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_write_over_the_endpoint_is_visible_to_the_daemon() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    assert_ne!(
        daemon_json(&daemon, "/api/status", None).await["palace_name"],
        "From Studio"
    );

    let mut socket = json_socket(&addr).await;
    let response = call(
        &mut socket,
        "query",
        json!(["CREATE type::record('palace', '6f1b1c8e-0000-4000-8000-000000000001') SET name = 'From Studio', created_at = time::now()"]),
    )
    .await;
    assert_eq!(response["result"][0]["status"], "OK", "{response}");

    // The same instance, not a copy: the daemon's own status reads the row back.
    assert_eq!(
        daemon_json(&daemon, "/api/status", None).await["palace_name"],
        "From Studio"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_studio_session_cannot_move_the_daemon_to_another_database() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;

    let mut studio = json_socket(&addr).await;
    let used = call(&mut studio, "use", json!(["elsewhere", "elsewhere"])).await;
    assert!(used.get("error").is_none(), "{used}");
    call(&mut studio, "let", json!(["answer", 42])).await;
    let selected = call(&mut studio, "query", json!(["RETURN $answer"])).await;
    assert_eq!(rows(&selected), &json!(42));

    // The daemon still works against its own database...
    let status = daemon_json(&daemon, "/api/status", None).await;
    assert_eq!(status["datastore"]["ok"], true, "{status}");
    assert_eq!(status["datastore"]["pending"], json!([]), "{status}");
    // ...and the next connection starts from the palace, without Studio's variable.
    let mut fresh = json_socket(&addr).await;
    let selected = call(&mut fresh, "query", json!(["RETURN $answer"])).await;
    assert_eq!(rows(&selected), &Value::Null);
    let selected = call(
        &mut fresh,
        "query",
        json!(["SELECT version FROM migration_state:state"]),
    )
    .await;
    assert_eq!(
        rows(&selected).as_array().map(Vec::len),
        Some(1),
        "a new session starts in memcastle/palace: {selected}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_rust_sdk_speaks_the_flatbuffers_format_to_the_endpoint() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;

    let db = sdk_client(&addr).await;
    db.use_ns("memcastle").use_db("palace").await.unwrap();
    // Open endpoint: Studio's login form still works with the advertised user.
    db.signin(Root {
        username: "memcastle".to_string(),
        password: "memcastle".to_string(),
    })
    .await
    .unwrap();
    let mut response = db
        .query("SELECT version FROM migration_state:state")
        .await
        .unwrap();
    let rows: Vec<surrealdb::types::Value> = response.take(0).unwrap();
    assert_eq!(rows.len(), 1);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_wrong_username_or_password_is_refused_even_without_authentication() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;

    // A client that never signs in, such as a script, can query an open endpoint.
    let open = call(&mut socket, "query", json!(["RETURN 1"])).await;
    assert_eq!(rows(&open), &json!(1));

    // `admin`/`admin` is what a person tries first; neither half may be guessed.
    for credentials in [
        json!({ "user": "admin", "pass": "admin" }),
        json!({ "user": "studio", "pass": "memcastle" }),
        json!({ "user": "memcastle", "pass": "admin" }),
        json!({ "user": "memcastle" }),
    ] {
        let refused = call(&mut socket, "signin", json!([credentials])).await;
        assert!(refused.get("result").is_none(), "{credentials}: {refused}");
    }
    // A refused sign-in leaves the session without access, as on an authenticated daemon.
    let locked = call(&mut socket, "query", json!(["RETURN 1"])).await;
    assert!(locked.get("result").is_none(), "{locked}");

    let accepted = call(
        &mut socket,
        "signin",
        json!([{ "user": "memcastle", "pass": "memcastle" }]),
    )
    .await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    let allowed = call(&mut socket, "query", json!(["RETURN 1"])).await;
    assert_eq!(rows(&allowed), &json!(1));

    // The long field names are accepted too.
    let mut other = json_socket(&addr).await;
    let accepted = call(
        &mut other,
        "signin",
        json!([{ "username": "memcastle", "password": "memcastle" }]),
    )
    .await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_status_names_the_user_to_sign_in_with() {
    let daemon = TestDaemon::start().await;
    let (status, body) = start_endpoint(&daemon, None, json!({ "port": 0 })).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["user"], "memcastle");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_cbor_client_gets_cbor_back() {
    use surrealdb::types::{Array, Value as Surreal, object};
    use surrealdb_core::rpc::format::cbor;

    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut request = format!("ws://{addr}/rpc").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("sec-websocket-protocol", HeaderValue::from_static("cbor"));
    let (mut socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(
        response.headers()["sec-websocket-protocol"],
        "cbor",
        "the endpoint agrees to the format the client asked for"
    );

    let ping = Surreal::Object(object! {
        id: 1,
        method: "version",
        params: Surreal::Array(Array::new()),
    });
    socket
        .send(Message::Binary(cbor::encode(ping).unwrap().into()))
        .await
        .unwrap();
    let Message::Binary(bytes) = socket.next().await.unwrap().unwrap() else {
        panic!("a CBOR connection answers in binary frames");
    };
    let Surreal::Object(answer) = cbor::decode(&bytes, 100).unwrap() else {
        panic!("an object");
    };
    let Some(Surreal::String(version)) = answer.get("result") else {
        panic!("a version string in {answer:?}");
    };
    assert!(version.starts_with("surrealdb-"));
    daemon.shutdown().await;
}

#[tokio::test]
async fn live_queries_say_they_are_not_supported_instead_of_hanging() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;

    let response = call(&mut socket, "live", json!(["job"])).await;

    assert!(response.get("result").is_none(), "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|m| m.to_lowercase().contains("live")),
        "{response}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_method_the_endpoint_does_not_offer_points_at_query() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;

    let response = call(&mut socket, "select", json!(["job"])).await;

    let message = response["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("select") && message.contains("query"),
        "{response}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_request_that_is_not_json_is_answered_with_an_error_and_the_connection_survives() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;

    socket
        .send(Message::Text("this is not json".into()))
        .await
        .unwrap();
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
        panic!("a text answer");
    };
    assert!(
        serde_json::from_str::<Value>(&text)
            .unwrap()
            .get("error")
            .is_some()
    );

    let ping = call(&mut socket, "ping", json!([])).await;
    assert!(ping.get("error").is_none(), "{ping}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_page_from_another_site_cannot_open_the_endpoint() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;

    let mut request = format!("ws://{addr}/rpc").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", HeaderValue::from_static("https://evil.example"));
    let refused = tokio_tungstenite::connect_async(request).await.unwrap_err();

    assert!(
        matches!(&refused, tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 403),
        "{refused:?}"
    );
    // The probes are refused too, so a foreign page cannot even tell it is there.
    let probe = reqwest::Client::new()
        .get(format!("http://{addr}/health"))
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(probe.status(), StatusCode::FORBIDDEN);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_local_page_and_an_explicitly_allowed_site_can_connect() {
    let daemon = TestDaemon::start().await;
    let started = start_endpoint(
        &daemon,
        None,
        json!({ "port": 0, "allowed_origins": ["https://app.surrealdb.com"] }),
    )
    .await
    .1;
    let addr = started["addr"].as_str().unwrap();

    // The desktop app's own origin needs no flag: it is not a web page.
    for origin in [
        "http://localhost:3000",
        "https://app.surrealdb.com",
        "app://surrealdb-studio",
    ] {
        let mut request = format!("ws://{addr}/rpc").into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", HeaderValue::from_str(origin).unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .unwrap_or_else(|e| panic!("{origin}: {e}"));
        assert!(
            call(&mut socket, "ping", json!([]))
                .await
                .get("error")
                .is_none()
        );
    }
    // A browser's CORS probe for the allowed site is answered with that site, never `*`.
    let preflight = reqwest::Client::new()
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/health"))
        .header("origin", "https://app.surrealdb.com")
        .send()
        .await
        .unwrap();
    assert_eq!(
        preflight.headers()["access-control-allow-origin"],
        "https://app.surrealdb.com"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_endpoint_beyond_loopback_is_refused_without_the_opt_in_and_without_authentication() {
    let daemon = TestDaemon::start().await;

    let (status, body) =
        start_endpoint(&daemon, None, json!({ "bind": "0.0.0.0", "port": 0 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "memcastle::db::unsafe_bind");
    assert!(body["help"].as_str().unwrap().contains("--allow-remote"));

    // The opt-in alone is not enough: an open daemon would expose a writable palace.
    let (status, body) = start_endpoint(
        &daemon,
        None,
        json!({ "bind": "0.0.0.0", "port": 0, "allow_remote": true }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("authentication"));
    assert_eq!(
        daemon_json(&daemon, "/api/db", None).await["running"],
        false
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_authenticated_daemon_may_listen_beyond_loopback_once_asked_to() {
    let daemon = authenticated_daemon().await;

    let (status, body) = start_endpoint(
        &daemon,
        Some(SECRET),
        json!({ "bind": "0.0.0.0", "port": 0, "allow_remote": true }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["remote"], true);
    assert_eq!(body["auth_required"], true);
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn starting_the_endpoint_twice_reports_it_is_already_running() {
    let daemon = TestDaemon::start().await;
    let (status, first) = start_endpoint(&daemon, None, json!({ "port": 0 })).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["already_running"], false);

    // `port: 0` means "any free port", which the running endpoint is.
    let (status, second) = start_endpoint(&daemon, None, json!({ "port": 0 })).await;

    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["already_running"], true);
    assert_eq!(second["addr"], first["addr"]);
    assert_eq!(second["url"], first["url"]);
    // A status read is not a start, so it never claims to be a repeat.
    assert_eq!(
        daemon_json(&daemon, "/api/db", None).await["already_running"],
        false
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn starting_the_endpoint_again_without_any_setting_reports_it_is_already_running() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;

    let (status, body) = start_endpoint(&daemon, None, json!({})).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["already_running"], true);
    assert_eq!(body["addr"], addr.as_str());
    daemon.shutdown().await;
}

#[tokio::test]
async fn starting_the_endpoint_again_on_another_port_is_a_conflict() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let running: u16 = addr.rsplit(':').next().unwrap().parse().unwrap();
    let other = if running == 65000 { 65001 } else { 65000 };

    let (status, body) = start_endpoint(&daemon, None, json!({ "port": other })).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "memcastle::db::already_running");
    assert!(body["error"].as_str().unwrap().contains(&addr));
    daemon.shutdown().await;
}

#[tokio::test]
async fn starting_the_endpoint_again_with_a_new_origin_is_a_conflict() {
    let daemon = TestDaemon::start().await;
    let (_, first) = start_endpoint(
        &daemon,
        None,
        json!({ "port": 0, "allowed_origins": ["https://studio.example"] }),
    )
    .await;

    let (status, known) = start_endpoint(
        &daemon,
        None,
        json!({ "allowed_origins": ["https://studio.example"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{known}");
    assert_eq!(known["addr"], first["addr"]);

    let (status, body) = start_endpoint(
        &daemon,
        None,
        json!({ "allowed_origins": ["https://other.example"] }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "memcastle::db::already_running");
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_authenticated_endpoint_refuses_queries_until_the_token_is_presented() {
    let daemon = authenticated_daemon().await;
    let addr = open_endpoint(&daemon, Some(SECRET)).await;
    let mut socket = json_socket(&addr).await;

    // The handshake methods work, the data does not.
    assert!(
        call(&mut socket, "version", json!([]))
            .await
            .get("error")
            .is_none()
    );
    let refused = call(&mut socket, "query", json!(["SELECT * FROM job"])).await;
    assert!(refused.get("result").is_none(), "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("authentication")
    );

    // A wrong token does not unlock it, and is not echoed back.
    let wrong = call(
        &mut socket,
        "signin",
        json!([{ "user": "memcastle", "pass": "mc_not_the_token" }]),
    )
    .await;
    assert!(wrong.get("result").is_none(), "{wrong}");
    assert!(!wrong.to_string().contains("mc_not_the_token"));
    let still_refused = call(&mut socket, "query", json!(["SELECT * FROM job"])).await;
    assert!(still_refused.get("result").is_none());

    // The right token under another username does not, and is not echoed back.
    let wrong_user = call(
        &mut socket,
        "signin",
        json!([{ "user": "studio", "pass": SECRET }]),
    )
    .await;
    assert!(wrong_user.get("result").is_none(), "{wrong_user}");
    assert!(!wrong_user.to_string().contains(SECRET));

    // The right one, as the password of the right user, does.
    let signed_in = call(
        &mut socket,
        "signin",
        json!([{ "user": "memcastle", "pass": SECRET }]),
    )
    .await;
    assert!(signed_in.get("error").is_none(), "{signed_in}");
    let allowed = call(&mut socket, "query", json!(["RETURN 1"])).await;
    assert_eq!(rows(&allowed), &json!(1));
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_token_a_signin_returns_authenticates_a_later_connection() {
    let daemon = authenticated_daemon().await;
    let addr = open_endpoint(&daemon, Some(SECRET)).await;

    let mut first = json_socket(&addr).await;
    let signed_in = call(
        &mut first,
        "signin",
        json!([{ "user": "memcastle", "pass": SECRET }]),
    )
    .await;
    let token = signed_in["result"].as_str().expect("a token").to_string();

    let mut second = json_socket(&addr).await;
    let accepted = call(&mut second, "authenticate", json!([token])).await;
    assert!(accepted.get("error").is_none(), "{accepted}");
    let allowed = call(&mut second, "query", json!(["RETURN 1"])).await;
    assert_eq!(rows(&allowed), &json!(1));
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn a_bearer_header_on_the_upgrade_authenticates_a_non_browser_client() {
    let daemon = authenticated_daemon().await;
    let addr = open_endpoint(&daemon, Some(SECRET)).await;

    let mut request = format!("ws://{addr}/rpc").into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {SECRET}")).unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    let allowed = call(&mut socket, "query", json!(["RETURN 1"])).await;
    assert_eq!(rows(&allowed), &json!(1));
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn repeated_wrong_tokens_close_the_connection() {
    let daemon = authenticated_daemon().await;
    let addr = open_endpoint(&daemon, Some(SECRET)).await;
    let mut socket = json_socket(&addr).await;

    for _ in 0..5 {
        call(
            &mut socket,
            "signin",
            json!([{ "user": "memcastle", "pass": "mc_wrong" }]),
        )
        .await;
    }

    // Whatever arrives next, it is the end of the stream and never a data answer.
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the endpoint closed the connection");
    assert!(
        matches!(next, None | Some(Ok(Message::Close(_)) | Err(_))),
        "{next:?}"
    );
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_admin_routes_are_refused_without_a_token_on_an_authenticated_daemon() {
    let daemon = authenticated_daemon().await;

    let (status, _) = start_endpoint(&daemon, None, json!({ "port": 0 })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let status = reqwest::get(format!("{}/api/db", daemon.base_url))
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let status = reqwest::Client::new()
        .delete(format!("{}/api/db", daemon.base_url))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn stopping_the_endpoint_closes_its_connections_and_its_port() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;
    assert!(
        call(&mut socket, "ping", json!([]))
            .await
            .get("error")
            .is_none()
    );

    let stopped = reqwest::Client::new()
        .delete(format!("{}/api/db", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(stopped["running"], false);

    let next = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
        .await
        .expect("the open connection was closed");
    assert!(
        matches!(next, None | Some(Ok(Message::Close(_)) | Err(_))),
        "{next:?}"
    );
    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/rpc"))
            .await
            .is_err(),
        "the port is closed"
    );
    // And the daemon itself is untouched.
    assert_eq!(
        daemon_json(&daemon, "/api/status", None).await["datastore"]["ok"],
        true
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn shutting_the_daemon_down_takes_the_endpoint_with_it() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;
    assert!(
        call(&mut socket, "ping", json!([]))
            .await
            .get("error")
            .is_none()
    );

    // `shutdown` also proves the daemon exits even with a Studio tab still open.
    daemon.shutdown().await;

    assert!(
        tokio_tungstenite::connect_async(format!("ws://{addr}/rpc"))
            .await
            .is_err(),
        "the port is closed"
    );
}

#[tokio::test]
async fn a_live_select_inside_a_query_answers_instead_of_hanging_the_connection() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        call(&mut socket, "query", json!(["LIVE SELECT * FROM job"])),
    )
    .await
    .expect("the statement was answered");

    // Whatever it answers, the connection is still usable afterwards.
    assert!(response.get("result").is_some() || response.get("error").is_some());
    let ping = call(&mut socket, "ping", json!([])).await;
    assert!(ping.get("error").is_none(), "{ping}");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_transaction_block_inside_one_query_runs_and_rolls_back_on_cancel() {
    let daemon = TestDaemon::start().await;
    let addr = open_endpoint(&daemon, None).await;
    let mut socket = json_socket(&addr).await;

    let committed = call(
        &mut socket,
        "query",
        json!([
            "BEGIN; CREATE type::record('scratch', 'kept') SET n = 1; COMMIT; \
                SELECT n FROM scratch:kept"
        ]),
    )
    .await;
    // One result per statement, as SurrealDB's own server reports them.
    assert_eq!(
        committed["result"][3]["result"],
        json!([{ "n": 1 }]),
        "{committed}"
    );
    call(
        &mut socket,
        "query",
        json!([
            "BEGIN; CREATE type::record('scratch', 'dropped') SET n = 2; CANCEL; \
                SELECT n FROM scratch:dropped"
        ]),
    )
    .await;
    let kept = call(&mut socket, "query", json!(["SELECT n FROM scratch:kept"])).await;
    assert_eq!(rows(&kept), &json!([{ "n": 1 }]));
    let dropped = call(
        &mut socket,
        "query",
        json!(["SELECT n FROM scratch:dropped"]),
    )
    .await;
    assert_eq!(rows(&dropped), &json!([]));
    daemon.shutdown().await;
}
