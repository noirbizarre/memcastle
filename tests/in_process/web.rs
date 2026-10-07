//! The web UI's routes and the REST additions it relies on, against an in-process daemon and real HTTP
//! (docs/adr/035).
//!
//! The dashboard is opt-in, served from the runtime assets, and the one thing the authentication layer lets through
//! without a token, so these tests hold all three: a disabled daemon answers no `/ui`, an enabled one serves exactly
//! `web/dist`, and nothing but the static files is ever open.

use crate::common;

use std::path::Path;

use common::TestDaemon;
use memcastle::config::Secret;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const SECRET: &str = "mc_a_shared_secret_for_the_dashboard_tests_01";
const INDEX: &str =
    "<!doctype html><title>MemCastle</title><script src=\"/ui/assets/app-abc123.js\"></script>";

/// A tree shaped like a Vite build, under `root/web/dist`, plus a file outside it that must never be served.
fn built_dashboard(root: &Path) {
    let dist = root.join("web/dist");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    std::fs::write(dist.join("index.html"), INDEX).unwrap();
    std::fs::write(dist.join("assets/app-abc123.js"), "console.log('app')").unwrap();
    std::fs::write(dist.join("assets/app-abc123.css"), "body{}").unwrap();
    std::fs::write(dist.join("favicon.svg"), "<svg/>").unwrap();
    std::fs::write(root.join("secret.txt"), "outside the dashboard").unwrap();
}

/// A daemon that serves the dashboard from `root`, optionally requiring a token.
async fn dashboard(root: &Path, authenticated: bool) -> TestDaemon {
    let root = root.to_path_buf();
    TestDaemon::start_configured(move |config| {
        config.web.enable = true;
        config.assets.dir = Some(root);
        if authenticated {
            config.auth.enabled = true;
            config.auth.token = Some(Secret::new(SECRET));
        }
    })
    .await
}

fn client() -> reqwest::Client {
    // A redirect is something to assert on, not to follow.
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn get(daemon: &TestDaemon, path: &str) -> reqwest::Response {
    client()
        .get(format!("{}{path}", daemon.base_url))
        .send()
        .await
        .expect("request")
}

fn header<'a>(response: &'a reqwest::Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("no {name} header"))
        .to_str()
        .unwrap()
}

#[tokio::test]
async fn a_daemon_that_did_not_enable_the_dashboard_answers_no_ui() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    // The files are there; only the setting is missing.
    let daemon = TestDaemon::start_configured(|config| {
        config.assets.dir = Some(assets.path().to_path_buf());
    })
    .await;

    for path in ["/ui", "/ui/", "/ui/index.html", "/ui/assets/app-abc123.js"] {
        assert_eq!(
            get(&daemon, path).await.status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_enabled_dashboard_serves_the_index_with_its_security_headers() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), false).await;

    let response = get(&daemon, "/ui/").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, "content-type"),
        "text/html; charset=utf-8"
    );
    // The index names hashed files, so it must be revalidated or an upgrade would leave a page pointing at nothing.
    assert_eq!(header(&response, "cache-control"), "no-cache");
    assert!(header(&response, "content-security-policy").contains("frame-ancestors 'none'"));
    assert!(header(&response, "content-security-policy").contains("default-src 'self'"));
    assert_eq!(header(&response, "x-content-type-options"), "nosniff");
    assert_eq!(header(&response, "referrer-policy"), "no-referrer");
    assert_eq!(response.text().await.unwrap(), INDEX);
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_bare_ui_path_redirects_to_the_slash_so_relative_urls_resolve() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), false).await;

    let response = get(&daemon, "/ui").await;

    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(header(&response, "location"), "/ui/");
    daemon.shutdown().await;
}

#[tokio::test]
async fn hashed_assets_are_typed_and_cached_forever_and_other_files_are_not() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), false).await;

    let script = get(&daemon, "/ui/assets/app-abc123.js").await;
    let style = get(&daemon, "/ui/assets/app-abc123.css").await;
    let icon = get(&daemon, "/ui/favicon.svg").await;

    assert_eq!(script.status(), StatusCode::OK);
    assert_eq!(
        header(&script, "content-type"),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        header(&script, "cache-control"),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(header(&style, "content-type"), "text/css; charset=utf-8");
    assert_eq!(header(&icon, "content-type"), "image/svg+xml");
    assert_eq!(header(&icon, "cache-control"), "no-cache");
    assert_eq!(script.text().await.unwrap(), "console.log('app')");
    daemon.shutdown().await;
}

#[tokio::test]
async fn head_is_answered_without_a_body() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), false).await;

    let response = client()
        .head(format!("{}/ui/", daemon.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().is_empty());
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_client_side_route_gets_the_index_but_a_missing_file_is_a_404() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), false).await;

    let route = get(&daemon, "/ui/jobs/1234").await;
    let missing = get(&daemon, "/ui/assets/gone-ffff.js").await;

    assert_eq!(route.status(), StatusCode::OK);
    assert_eq!(route.text().await.unwrap(), INDEX);
    // A broken script URL must not come back as HTML that the browser then fails to parse as JavaScript.
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    daemon.shutdown().await;
}

#[tokio::test]
async fn no_path_leaves_the_dashboard_directory() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), false).await;

    for path in [
        "/ui/../secret.txt",
        "/ui/%2e%2e/secret.txt",
        "/ui/..%2fsecret.txt",
        "/ui/assets/../../secret.txt",
        "/ui/%2e%2e/%2e%2e/etc/passwd",
        "/ui//etc/passwd",
    ] {
        let response = get(&daemon, path).await;
        let body = response.text().await.unwrap();
        assert!(
            !body.contains("outside the dashboard") && !body.contains("root:"),
            "{path} served a file from outside web/dist: {body}"
        );
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_enabled_dashboard_without_a_build_says_so_and_the_daemon_still_works() {
    // Assets exist but carry no `web/` tree: the state of a checkout where nobody ran `mise run web:build`.
    let assets = tempfile::tempdir().unwrap();
    let daemon = dashboard(assets.path(), false).await;

    let page = get(&daemon, "/ui/").await;
    let route = get(&daemon, "/ui/jobs").await;
    let file = get(&daemon, "/ui/assets/app.js").await;
    let health = get(&daemon, "/api/health").await;
    let config: Value = get(&daemon, "/api/config").await.json().await.unwrap();

    // A 503, so a monitor sees a service that is not ready, with a page that names the remedy.
    assert_eq!(page.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header(&page, "cache-control"), "no-store");
    let text = page.text().await.unwrap();
    assert!(
        text.contains("web/dist/index.html") && text.contains("mise run web:build"),
        "{text}"
    );
    assert_eq!(route.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(file.status(), StatusCode::NOT_FOUND);
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(config["web"], json!({"enabled": true, "built": false}));
    daemon.shutdown().await;
}

#[tokio::test]
async fn with_authentication_the_static_files_are_open_and_everything_else_is_still_guarded() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), true).await;

    for path in [
        "/ui",
        "/ui/",
        "/ui/index.html",
        "/ui/assets/app-abc123.js",
        "/ui/jobs",
    ] {
        let status = get(&daemon, path).await.status();
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{path} must load without a token"
        );
    }

    // The page's own calls are guarded exactly as before.
    for (method, path) in [
        (Method::GET, "/api/status"),
        (Method::GET, "/api/config"),
        (Method::GET, "/api/graph"),
        (Method::GET, "/api/jobs"),
        (Method::GET, "/api/wings"),
        (Method::POST, "/api/jobs"),
        (Method::POST, "/api/shutdown"),
        // Neither a write method on `/ui`, nor a sibling that merely starts with it, nor a way back out of it.
        (Method::POST, "/ui/"),
        (Method::DELETE, "/ui/index.html"),
        (Method::GET, "/uix"),
        (Method::GET, "/ui-admin/index.html"),
        (Method::GET, "/ui/../api/status"),
        (Method::POST, "/mcp"),
    ] {
        let response = client()
            .request(method.clone(), format!("{}{path}", daemon.base_url))
            .send()
            .await
            .unwrap();
        let status = response.status();
        // `/ui/../api/status` is the one that may legitimately be served the index (the path is never decoded into
        // the API), but it must never be the API's answer.
        if path == "/ui/../api/status" {
            let body = response.text().await.unwrap();
            assert!(
                !body.contains("drawer_count"),
                "{path} reached the API: {body}"
            );
            continue;
        }
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} was not guarded"
        );
    }
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_token_the_login_page_sends_is_checked_by_the_same_layer_as_every_route() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let daemon = dashboard(assets.path(), true).await;
    let call = |token: &'static str| {
        let url = format!("{}/api/status", daemon.base_url);
        async move {
            client()
                .get(url)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status()
        }
    };

    assert_eq!(call(SECRET).await, StatusCode::OK);
    assert_eq!(call("not-the-token").await, StatusCode::UNAUTHORIZED);
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_configuration_route_reports_the_settings_and_never_a_secret() {
    let assets = tempfile::tempdir().unwrap();
    built_dashboard(assets.path());
    let root = assets.path().to_path_buf();
    let daemon = TestDaemon::start_configured(move |config| {
        config.web.enable = true;
        config.assets.dir = Some(root);
        config.auth.enabled = true;
        config.auth.token = Some(Secret::new(SECRET));
        config.embeddings.api_key = Some(Secret::new("embeddings-key-0123456789"));
        config.embeddings.url = Some("http://user:pw@embeddings.example/v1".into());
    })
    .await;

    let response = client()
        .get(format!("{}/api/config", daemon.base_url))
        .bearer_auth(SECRET)
        .send()
        .await
        .unwrap();
    let text = response.text().await.unwrap();
    let config: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(config["auth_enabled"], true);
    assert_eq!(config["web"], json!({"enabled": true, "built": true}));
    assert_eq!(config["assets"]["source"], "override");
    assert_eq!(config["backend"], "embedded");
    assert!(
        config["bind_addr"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
    for secret in [
        SECRET,
        "embeddings-key-0123456789",
        "embeddings.example",
        "user:pw",
    ] {
        assert!(
            !text.contains(secret),
            "{secret} leaked into /api/config: {text}"
        );
    }
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn the_configuration_route_is_daemon_information_and_open_to_every_memory_mode() {
    let daemon = TestDaemon::start().await;

    let response = client()
        .get(format!("{}/api/config", daemon.base_url))
        .header("x-memcastle-mode", "disabled")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let config: Value = response.json().await.unwrap();
    assert_eq!(config["web"]["enabled"], false, "the dashboard is opt-in");
    daemon.shutdown().await;
}

async fn submit_demo(daemon: &TestDaemon) {
    let response = client()
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&json!({"type": "demo", "steps": 1, "requested_by": "test"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
}

async fn job_count(daemon: &TestDaemon, query: &str) -> usize {
    let response = get(daemon, &format!("/api/jobs{query}")).await;
    assert_eq!(response.status(), StatusCode::OK, "{query}");
    response.json::<Vec<Value>>().await.unwrap().len()
}

#[tokio::test]
async fn the_jobs_listing_can_be_narrowed_by_kind_and_bounded_by_limit() {
    let daemon = TestDaemon::start().await;
    for _ in 0..3 {
        submit_demo(&daemon).await;
    }

    // Unchanged for existing callers: no parameter means every job.
    assert_eq!(job_count(&daemon, "").await, 3);
    assert_eq!(job_count(&daemon, "?limit=2").await, 2);
    assert_eq!(job_count(&daemon, "?kind=demo").await, 3);
    assert_eq!(job_count(&daemon, "?kind=demo&limit=1").await, 1);
    assert_eq!(job_count(&daemon, "?kind=mine").await, 0);
    assert_eq!(job_count(&daemon, "?kind=demo&status=failed").await, 0);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_session_cannot_page_through_jobs() {
    let daemon = TestDaemon::start().await;
    submit_demo(&daemon).await;

    let response = client()
        .get(format!("{}/api/jobs?limit=5", daemon.base_url))
        .header("x-memcastle-mode", "disabled")
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    daemon.shutdown().await;
}
