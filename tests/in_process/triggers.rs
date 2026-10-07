//! Source triggers against a real daemon (docs/adr/043): what asks for a mining run on its own, and the promise that
//! nothing does until the user says so.
//!
//! What is tested here is what the daemon does for any trigger: it is created disabled, enabling it checks what it
//! needs, a webhook is authenticated and opens no port until one is enabled, duplicate and bursty requests do not
//! multiply jobs, a failure is visible and recovers, and a restart neither forgets nor overrides what the user chose.
//! Every wait polls for the condition instead of sleeping a fixed time, so a slow machine is slow and not wrong.

use crate::common;

use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use common::{TestDaemon, mcp, wait_for_registry};
use hmac::{Hmac, KeyInit, Mac};
use memcastle::config::{Config, StoreConfig};
use memcastle::store::StoreSync;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::process::Command;

const SECRET: &str = "s3cret-value-for-the-webhook";

async fn request(
    daemon: &TestDaemon,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    request_at(&daemon.base_url, method, path, body).await
}

async fn request_at(
    base_url: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = reqwest::Client::new().request(method, format!("{base_url}{path}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.expect("request");
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn get(daemon: &TestDaemon, path: &str) -> (StatusCode, Value) {
    request(daemon, Method::GET, path, None).await
}

async fn post(daemon: &TestDaemon, path: &str) -> (StatusCode, Value) {
    request(daemon, Method::POST, path, None).await
}

async fn put_miner(daemon: &TestDaemon, name: &str, locator: &Path) {
    let (status, body) = request(
        daemon,
        Method::PUT,
        &format!("/api/miners/{name}"),
        Some(json!({ "source": "directory", "locator": locator })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// `PUT /api/triggers/{name}`.
async fn put_trigger(daemon: &TestDaemon, name: &str, body: Value) -> (StatusCode, Value) {
    request(
        daemon,
        Method::PUT,
        &format!("/api/triggers/{name}"),
        Some(body),
    )
    .await
}

async fn trigger(daemon: &TestDaemon, name: &str) -> Value {
    let (status, body) = get(daemon, &format!("/api/triggers/{name}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// A directory with a note in it, to mine.
fn notes_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("notes.txt"),
        "How do I rotate the signing keys?\n",
    )
    .unwrap();
    dir
}

/// Poll `check` until it returns something, for as long as a slow CI runner might need.
async fn eventually<T, F, Fut>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    for _ in 0..400 {
        if let Some(found) = check().await {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("gave up waiting for {what}");
}

/// Every job the daemon has, as JSON.
async fn jobs(daemon: &TestDaemon) -> Vec<Value> {
    let (_, body) = get(daemon, "/api/jobs").await;
    body.as_array().cloned().unwrap_or_default()
}

/// The mining jobs a trigger asked for.
async fn trigger_jobs(daemon: &TestDaemon, name: &str) -> Vec<Value> {
    let by = format!("trigger:{name}");
    jobs(daemon)
        .await
        .into_iter()
        .filter(|job| {
            job["kind"]["type"] == "mine"
                && job["requested_by"]
                    .as_str()
                    .is_some_and(|r| r.starts_with(&by))
        })
        .collect()
}

/// A port that was free a moment ago, for a daemon that must be told which one to listen on.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

async fn port_is_open(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

/// A daemon with the webhook listener allowed, on a port the test knows, and the shared secret in a file.
async fn webhook_daemon() -> (TestDaemon, u16, tempfile::TempDir) {
    let port = free_port();
    let daemon = TestDaemon::start_configured(|config| {
        config.webhook.enable = true;
        config.webhook.port = port;
    })
    .await;
    let secrets = tempfile::tempdir().expect("tempdir");
    std::fs::write(secrets.path().join("hook-secret"), format!("{SECRET}\n")).unwrap();
    (daemon, port, secrets)
}

fn webhook_body(secrets: &Path, miner: &str) -> Value {
    json!({
        "miner": miner, "type": "webhook",
        "credential": { "type": "file", "path": secrets.join("hook-secret") },
        "settings": { "delivery_header": "x-delivery-id" },
    })
}

fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256={hex}")
}

/// Deliver `body` to the webhook trigger `name` on `port`, signed with `secret` (or not at all).
async fn deliver(
    port: u16,
    name: &str,
    secret: Option<&str>,
    delivery_id: Option<&str>,
) -> (StatusCode, Value) {
    let body = br#"{"event":"changed"}"#;
    let mut request = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/hooks/{name}"))
        .body(body.to_vec());
    if let Some(secret) = secret {
        request = request.header("x-hub-signature-256", signature(secret, body));
    }
    if let Some(id) = delivery_id {
        request = request.header("x-delivery-id", id);
    }
    let response = request.send().await.expect("delivery");
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// Occupy the daemon's only job slot, so every job asked for meanwhile stays queued where it can be counted.
async fn occupy_the_scheduler(daemon: &TestDaemon) -> String {
    let (status, job) = request(
        daemon,
        Method::POST,
        "/api/jobs",
        Some(json!({ "type": "demo", "steps": 400, "requested_by": "test" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    let id = job["id"].as_str().expect("a job id").to_string();
    eventually("the occupier to run", || async {
        let (_, job) = get(daemon, &format!("/api/jobs/{id}")).await;
        (job["status"] == "running").then_some(())
    })
    .await;
    id
}

async fn release_the_scheduler(daemon: &TestDaemon, id: &str) {
    let _ = post(daemon, &format!("/api/jobs/{id}/cancel")).await;
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("-")
}

// ---- disabled by default -----------------------------------------------------------------------------------------

#[tokio::test]
async fn a_defined_trigger_starts_nothing_until_it_is_enabled() {
    let (daemon, port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;

    let (status, created) = put_trigger(
        &daemon,
        "poll-docs",
        json!({ "miner": "docs", "type": "poll", "settings": { "every": "1s" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(created["trigger"]["enabled"], false, "a new trigger is off");
    assert_eq!(created["trigger"]["status"], "disabled");
    let (status, hook) = put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    assert_eq!(status, StatusCode::OK, "{hook}");
    assert_eq!(hook["trigger"]["enabled"], false);

    // Long enough for a 1s poll to have fired several times if it were running.
    tokio::time::sleep(Duration::from_millis(2500)).await;

    assert!(
        jobs(&daemon).await.is_empty(),
        "a disabled trigger asks for nothing"
    );
    assert!(
        !port_is_open(port).await,
        "no webhook listener is opened for a trigger that is off"
    );
    let (_, report) = get(&daemon, "/api/triggers").await;
    assert_eq!(
        report["webhook"]["enabled"], true,
        "the listener is allowed"
    );
    assert!(
        report["webhook"]["listening"].is_null(),
        "but nothing is listening"
    );
    assert_eq!(trigger(&daemon, "poll-docs").await["fired"], 0);
    daemon.shutdown().await;
}

#[tokio::test]
async fn installing_a_source_or_defining_a_miner_starts_no_trigger_and_no_listener() {
    let (daemon, port, _secrets) = webhook_daemon().await;
    let notes = notes_dir();

    put_miner(&daemon, "docs", notes.path()).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    let (_, report) = get(&daemon, "/api/triggers").await;
    assert_eq!(report["triggers"], json!([]));
    assert!(!port_is_open(port).await);
    assert!(jobs(&daemon).await.is_empty());
    daemon.shutdown().await;
}

// ---- activation --------------------------------------------------------------------------------------------------

#[tokio::test]
async fn enabling_says_what_is_missing_and_nothing_is_started_or_written() {
    // The default configuration: `[webhook]` is off.
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;

    // A webhook cannot be switched on while the listener is off, and the answer says how to turn it on.
    let (status, hook) = put_trigger(
        &daemon,
        "hook",
        json!({
            "miner": "docs", "type": "webhook", "enabled": true,
            "credential": { "type": "env", "name": "MEMCASTLE_TEST_HOOK_SECRET_UNSET" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{hook}");
    assert_eq!(code(&hook), "memcastle::trigger::not_activatable");
    assert!(
        hook["error"].as_str().unwrap().contains("[webhook]"),
        "{hook}"
    );
    assert_eq!(
        get(&daemon, "/api/triggers").await.1["triggers"],
        json!([]),
        "nothing was written"
    );

    // A watch on a path that is not there.
    let (status, watch) = put_trigger(
        &daemon,
        "watch",
        json!({
            "miner": "docs", "type": "watch", "enabled": true,
            "settings": { "path": notes.path().join("absent") },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{watch}");
    assert!(
        watch["error"].as_str().unwrap().contains("does not exist"),
        "{watch}"
    );

    // A miner that is not there.
    let (status, orphan) = put_trigger(
        &daemon,
        "orphan",
        json!({ "miner": "nobody", "type": "poll", "enabled": true, "settings": { "every": "1h" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{orphan}");

    // Written disabled, the same definitions are accepted, and `get` says what each still needs.
    let (status, saved) = put_trigger(
        &daemon,
        "hook",
        json!({
            "miner": "docs", "type": "webhook",
            "credential": { "type": "env", "name": "MEMCASTLE_TEST_HOOK_SECRET_UNSET" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let shown = trigger(&daemon, "hook").await;
    assert_eq!(shown["status"], "disabled");
    assert!(
        shown["setup"][0].as_str().unwrap().contains("[webhook]"),
        "{shown}"
    );
    assert_eq!(
        shown["credential"],
        json!({ "kind": "env", "available": false })
    );
    let (status, refused) = post(&daemon, "/api/triggers/hook/enable").await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(trigger(&daemon, "hook").await["enabled"], false);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_definition_with_a_typo_or_a_missing_setting_is_refused_before_anything_is_written() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;

    for (patch, wants) in [
        (json!({ "miner": "docs", "type": "poll" }), "needs `every`"),
        (
            json!({ "miner": "docs", "type": "poll", "settings": { "evr": "1h" } }),
            "evr",
        ),
        (
            json!({ "miner": "docs", "type": "schedule", "settings": { "every": "90m", "at": "03:30" } }),
            "whole days",
        ),
        (
            json!({ "miner": "docs", "type": "watch", "settings": { "path": "relative" } }),
            "absolute",
        ),
        (
            json!({ "miner": "docs", "type": "poll", "settings": { "every": "1h", "api_key": "x" } }),
            "looks like a secret",
        ),
        (json!({ "type": "poll" }), "--miner"),
    ] {
        let (status, body) = put_trigger(&daemon, "t", patch.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{patch}: {body}");
        assert_eq!(code(&body), "memcastle::trigger::invalid", "{body}");
        assert!(
            body["error"].as_str().unwrap().contains(wants),
            "{patch}: {body}"
        );
    }
    let written = std::fs::read_to_string(&daemon.config_path).unwrap_or_default();
    assert!(
        !written.contains("[[triggers]]"),
        "a refused definition writes nothing: {written}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_source_lists_what_can_trigger_it_without_anything_being_activated() {
    let daemon = TestDaemon::start().await;

    let (_, sources) = get(&daemon, "/api/sources").await;
    let directory = sources["adapters"]
        .as_array()
        .and_then(|all| all.iter().find(|a| a["name"] == "directory"))
        .unwrap_or_else(|| panic!("the directory source: {sources}"));
    let kinds: Vec<&str> = directory["triggers"]
        .as_array()
        .expect("triggers")
        .iter()
        .filter_map(|t| t["kind"].as_str())
        .collect();
    assert_eq!(kinds, ["schedule", "poll", "webhook", "watch"]);
    assert_eq!(get(&daemon, "/api/triggers").await.1["triggers"], json!([]));
    daemon.shutdown().await;
}

// ---- one path for every way of asking ---------------------------------------------------------------------------

#[tokio::test]
async fn firing_by_hand_asks_for_a_mining_job_and_says_who_asked() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;

    // A disabled trigger is not fired, by hand or otherwise.
    put_trigger(
        &daemon,
        "t",
        json!({ "miner": "docs", "type": "poll", "settings": { "every": "1h" } }),
    )
    .await;
    let (status, refused) = post(&daemon, "/api/triggers/t/fire").await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(code(&refused), "memcastle::trigger::disabled");
    assert!(jobs(&daemon).await.is_empty());

    post(&daemon, "/api/triggers/t/enable").await;
    let (status, fired) = post(&daemon, "/api/triggers/t/fire").await;
    assert_eq!(status, StatusCode::OK, "{fired}");
    assert_eq!(fired["outcome"], "queued");
    let job_id = fired["job"].as_str().expect("a job").to_string();
    let (_, job) = get(&daemon, &format!("/api/jobs/{job_id}")).await;
    assert_eq!(job["kind"]["type"], "mine");
    assert_eq!(job["requested_by"], "trigger:t:manual");
    assert_eq!(trigger(&daemon, "t").await["last_job"], job_id.as_str());
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_burst_of_requests_while_a_run_waits_is_one_job() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(
        &daemon,
        "t",
        json!({ "miner": "docs", "type": "poll", "enabled": true, "settings": { "every": "1h" } }),
    )
    .await;
    let occupier = occupy_the_scheduler(&daemon).await;

    let answers =
        futures::future::join_all((0..25).map(|_| post(&daemon, "/api/triggers/t/fire"))).await;

    let queued = answers
        .iter()
        .filter(|(_, a)| a["outcome"] == "queued")
        .count();
    let joined = answers
        .iter()
        .filter(|(_, a)| a["outcome"] == "coalesced")
        .count();
    assert!(
        answers.iter().all(|(s, _)| *s == StatusCode::OK),
        "{answers:?}"
    );
    assert_eq!(
        (queued, joined),
        (1, 24),
        "one run is queued and the rest join it"
    );
    assert_eq!(trigger_jobs(&daemon, "t").await.len(), 1);
    let view = trigger(&daemon, "t").await;
    assert_eq!(
        (view["fired"].as_u64(), view["coalesced"].as_u64()),
        (Some(1), Some(24))
    );
    release_the_scheduler(&daemon, &occupier).await;
    daemon.shutdown().await;
}

// ---- poll and schedule -------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_enabled_poll_fires_on_its_own_after_one_interval_and_never_on_the_spot() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(
        &daemon,
        "poll-docs",
        json!({ "miner": "docs", "type": "poll", "settings": { "every": "2s" } }),
    )
    .await;

    let (status, enabled) = post(&daemon, "/api/triggers/poll-docs/enable").await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["trigger"]["status"], "active");
    assert_eq!(
        trigger(&daemon, "poll-docs").await["fired"],
        0,
        "enabling does not fire"
    );
    assert!(trigger_jobs(&daemon, "poll-docs").await.is_empty());

    eventually("the poll to fire", || async {
        (!trigger_jobs(&daemon, "poll-docs").await.is_empty()).then_some(())
    })
    .await;
    let view = trigger(&daemon, "poll-docs").await;
    assert!(view["fired"].as_u64().unwrap() >= 1, "{view}");
    assert!(
        view["next_due"].is_string(),
        "the next time is shown: {view}"
    );
    assert_eq!(view["status"], "active");

    // Switched off, it stops: nothing new is asked for.
    post(&daemon, "/api/triggers/poll-docs/disable").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let asked = trigger_jobs(&daemon, "poll-docs").await.len();
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert_eq!(trigger_jobs(&daemon, "poll-docs").await.len(), asked);
    daemon.shutdown().await;
}

// ---- webhook -----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_webhook_opens_its_listener_only_while_enabled_and_closes_it_after() {
    let (daemon, port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    let (status, saved) = put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(
        !port_is_open(port).await,
        "saved, not enabled: still closed"
    );

    let (status, enabled) = post(&daemon, "/api/triggers/hook/enable").await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    eventually("the listener to open", || async {
        port_is_open(port).await.then_some(())
    })
    .await;
    let shown = trigger(&daemon, "hook").await;
    assert_eq!(shown["running"], true, "{shown}");
    assert_eq!(
        shown["endpoint"],
        format!("http://127.0.0.1:{port}/hooks/hook").as_str()
    );
    assert_eq!(
        shown["credential"],
        json!({ "kind": "file", "available": true })
    );
    assert!(
        !shown.to_string().contains(SECRET),
        "the secret is never shown"
    );

    post(&daemon, "/api/triggers/hook/disable").await;
    eventually("the listener to close", || async {
        (!port_is_open(port).await).then_some(())
    })
    .await;
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_webhook_delivery_must_prove_itself_and_an_authentic_one_asks_for_a_run() {
    let (daemon, port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    put_trigger(&daemon, "quiet", webhook_body(secrets.path(), "docs")).await;
    post(&daemon, "/api/triggers/hook/enable").await;
    eventually("the listener to open", || async {
        port_is_open(port).await.then_some(())
    })
    .await;

    // Whatever is wrong is the same answer, so the listener does not say which triggers exist.
    let missing = deliver(port, "hook", None, None).await;
    let wrong_secret = deliver(port, "hook", Some("not-the-secret"), None).await;
    let unknown = deliver(port, "nobody", Some(SECRET), None).await;
    let disabled = deliver(port, "quiet", Some(SECRET), None).await;
    for (status, _) in [&missing, &wrong_secret, &unknown, &disabled] {
        assert_eq!(*status, StatusCode::UNAUTHORIZED);
    }
    assert!(
        trigger_jobs(&daemon, "hook").await.is_empty(),
        "an unauthentic delivery asks for nothing"
    );

    let (status, accepted) = deliver(port, "hook", Some(SECRET), Some("d-1")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert!(accepted["job"].is_string());
    eventually("the run to be asked for", || async {
        (!trigger_jobs(&daemon, "hook").await.is_empty()).then_some(())
    })
    .await;
    assert_eq!(
        trigger_jobs(&daemon, "hook").await[0]["requested_by"],
        "trigger:hook"
    );

    // The daemon's own routes are not where deliveries go, and its token is not what proves one.
    let (status, _) = request(&daemon, Method::POST, "/hooks/hook", None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the API's port does not serve the listener's routes"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_repeated_delivery_is_answered_but_runs_once() {
    let (daemon, port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    post(&daemon, "/api/triggers/hook/enable").await;
    eventually("the listener to open", || async {
        port_is_open(port).await.then_some(())
    })
    .await;
    let occupier = occupy_the_scheduler(&daemon).await;

    let first = deliver(port, "hook", Some(SECRET), Some("evt-42")).await;
    let again = deliver(port, "hook", Some(SECRET), Some("evt-42")).await;
    let third = deliver(port, "hook", Some(SECRET), Some("evt-42")).await;

    assert_eq!(first.1["status"], "queued", "{first:?}");
    for (status, answer) in [&again, &third] {
        assert_eq!(
            *status,
            StatusCode::ACCEPTED,
            "a repeat is acknowledged so the sender stops retrying"
        );
        assert_eq!(answer["status"], "duplicate");
    }
    assert_eq!(trigger_jobs(&daemon, "hook").await.len(), 1);
    assert_eq!(trigger(&daemon, "hook").await["duplicates"], 2);
    release_the_scheduler(&daemon, &occupier).await;
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_burst_of_webhook_deliveries_becomes_one_waiting_run() {
    let (daemon, port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    post(&daemon, "/api/triggers/hook/enable").await;
    eventually("the listener to open", || async {
        port_is_open(port).await.then_some(())
    })
    .await;
    let occupier = occupy_the_scheduler(&daemon).await;

    let answers = futures::future::join_all((0..120).map(|i| async move {
        deliver(port, "hook", Some(SECRET), Some(&format!("evt-{i}"))).await
    }))
    .await;

    // The listener never falls over: every delivery is accepted, or told to retry because it was busy.
    for (status, answer) in &answers {
        assert!(
            *status == StatusCode::ACCEPTED || *status == StatusCode::TOO_MANY_REQUESTS,
            "{status}: {answer}"
        );
    }
    let accepted = answers
        .iter()
        .filter(|(s, _)| *s == StatusCode::ACCEPTED)
        .count();
    assert!(accepted > 0);
    assert_eq!(
        trigger_jobs(&daemon, "hook").await.len(),
        1,
        "{accepted} accepted deliveries are one waiting run, not {accepted} jobs"
    );
    release_the_scheduler(&daemon, &occupier).await;
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_secret_that_cannot_be_read_refuses_every_delivery() {
    let (daemon, port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    post(&daemon, "/api/triggers/hook/enable").await;
    eventually("the listener to open", || async {
        port_is_open(port).await.then_some(())
    })
    .await;

    // Rotated away from under the daemon: nobody can prove themselves, and nothing runs.
    std::fs::write(secrets.path().join("hook-secret"), "").unwrap();
    let (status, _) = deliver(port, "hook", Some(SECRET), None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(trigger_jobs(&daemon, "hook").await.is_empty());
    daemon.shutdown().await;
}

// ---- watch -------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_change_under_a_watched_directory_asks_for_one_run_however_many_files_changed() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    let (status, saved) = put_trigger(
        &daemon,
        "watch-docs",
        json!({
            "miner": "docs", "type": "watch", "enabled": true,
            "settings": { "path": notes.path(), "debounce": "400ms" },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    eventually("the watcher to be running", || async {
        (trigger(&daemon, "watch-docs").await["running"] == true).then_some(())
    })
    .await;
    assert!(
        trigger_jobs(&daemon, "watch-docs").await.is_empty(),
        "starting to watch asks for nothing"
    );

    for i in 0..40 {
        std::fs::write(
            notes.path().join(format!("note-{i}.txt")),
            format!("note {i}\n"),
        )
        .unwrap();
    }

    eventually("the change to ask for a run", || async {
        (!trigger_jobs(&daemon, "watch-docs").await.is_empty()).then_some(())
    })
    .await;
    // Let the burst's debounce and any straggler event settle.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let asked = trigger_jobs(&daemon, "watch-docs").await.len();
    assert!(
        asked <= 3,
        "40 files written in a burst asked for {asked} runs"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_watched_path_that_disappears_is_reported_and_recovers_when_it_returns() {
    let daemon = TestDaemon::start().await;
    let parent = tempfile::tempdir().expect("tempdir");
    let watched = parent.path().join("notes");
    std::fs::create_dir(&watched).unwrap();
    std::fs::write(watched.join("a.txt"), "a note\n").unwrap();
    put_miner(&daemon, "docs", &watched).await;
    put_trigger(
        &daemon,
        "w",
        json!({ "miner": "docs", "type": "watch", "enabled": true, "settings": { "path": watched } }),
    )
    .await;
    eventually("the watcher to run", || async {
        (trigger(&daemon, "w").await["running"] == true).then_some(())
    })
    .await;

    std::fs::remove_dir_all(&watched).unwrap();
    let unavailable = eventually("the trigger to report the missing path", || async {
        let view = trigger(&daemon, "w").await;
        (view["status"] != "active").then_some(view)
    })
    .await;
    assert!(
        unavailable["reason"]
            .as_str()
            .is_some_and(|r| r.contains("does not exist") || r.contains("no longer exists")),
        "{unavailable}"
    );
    assert_eq!(
        unavailable["enabled"], true,
        "the user's choice is untouched by the failure"
    );

    std::fs::create_dir(&watched).unwrap();
    std::fs::write(watched.join("a.txt"), "a note\n").unwrap();
    eventually("the trigger to recover", || async {
        let view = trigger(&daemon, "w").await;
        (view["status"] == "active" && view["running"] == true).then_some(())
    })
    .await;
    daemon.shutdown().await;
}

// ---- restart -----------------------------------------------------------------------------------------------------

/// A palace and a configuration file that outlive the daemons started on them, for tests about what survives a restart.
struct Restartable {
    dir: tempfile::TempDir,
}

struct Running {
    base_url: String,
    handle: tokio::task::JoinHandle<memcastle::Result<()>>,
}

impl Restartable {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    async fn start(&self) -> Running {
        let palace_path = self.dir.path().join("palace");
        let mut config = Config::default();
        config.palace.path = palace_path.clone();
        config.jobs.max_concurrency = 1;
        config.server.bind = "127.0.0.1".parse().unwrap();
        config.server.port = 0;
        config.mining.registries.clear();
        config.store = StoreConfig::Embedded {
            sync: StoreSync::Never,
        };
        config.config_file = Some(self.dir.path().join("config.toml"));
        let handle = tokio::spawn(memcastle::server::run(config));
        let info = wait_for_registry(&palace_path).await;
        Running {
            base_url: format!("http://{}", info.bind_addr),
            handle,
        }
    }
}

impl Running {
    async fn stop(self) {
        let _ = reqwest::Client::new()
            .post(format!("{}/api/shutdown", self.base_url))
            .send()
            .await;
        tokio::time::timeout(Duration::from_secs(15), self.handle)
            .await
            .expect("the daemon stops")
            .expect("no panic")
            .expect("no error");
    }
}

#[tokio::test]
async fn a_restart_keeps_what_the_user_chose_and_what_the_trigger_remembered_and_fires_nothing() {
    let machine = Restartable::new();
    let notes = notes_dir();

    let first = machine.start().await;
    let url = first.base_url.clone();
    request_at(
        &url,
        Method::PUT,
        "/api/miners/docs",
        Some(json!({ "source": "directory", "locator": notes.path() })),
    )
    .await;
    for (name, enabled) in [("on", true), ("off", false)] {
        let (status, body) = request_at(
            &url,
            Method::PUT,
            &format!("/api/triggers/{name}"),
            Some(json!({ "miner": "docs", "type": "poll", "enabled": enabled, "settings": { "every": "1h" } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (_, fired) = request_at(&url, Method::POST, "/api/triggers/on/fire", None).await;
    assert_eq!(fired["outcome"], "queued");
    let (_, before) = request_at(&url, Method::GET, "/api/triggers/on", None).await;
    eventually("the timetable to be remembered", || async {
        let (_, view) = request_at(&url, Method::GET, "/api/triggers/on", None).await;
        view["next_due"].is_string().then_some(())
    })
    .await;
    let (_, before_due) = request_at(&url, Method::GET, "/api/triggers/on", None).await;
    first.stop().await;

    let second = machine.start().await;
    let url = second.base_url.clone();
    let (_, after) = request_at(&url, Method::GET, "/api/triggers/on", None).await;
    assert_eq!(after["enabled"], true);
    assert_eq!(after["fired"], before["fired"], "the counters survived");
    assert_eq!(after["last_job"], before["last_job"]);
    assert_eq!(
        after["next_due"], before_due["next_due"],
        "the wait was remembered, not restarted"
    );
    let (_, off) = request_at(&url, Method::GET, "/api/triggers/off", None).await;
    assert_eq!(
        off["enabled"], false,
        "a restart never switches a trigger on"
    );
    assert_eq!(off["status"], "disabled");

    // Neither trigger is due for an hour, and the disabled one never is: a restart asks for nothing by itself.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let (_, all) = request_at(&url, Method::GET, "/api/jobs", None).await;
    let triggered = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| {
            job["requested_by"]
                .as_str()
                .is_some_and(|r| r.starts_with("trigger:"))
        })
        .count();
    assert_eq!(triggered, 1, "only the firing made before the restart");
    second.stop().await;
}

// ---- CLI ---------------------------------------------------------------------------------------------------------

fn memcastle(daemon: &TestDaemon) -> Command {
    let mut command = Command::new(cargo_bin("memcastle"));
    command
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .env_remove("MEMCASTLE_MODE")
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdin(Stdio::null());
    command
}

async fn cli(daemon: &TestDaemon, args: &[&str]) -> (bool, Value, String) {
    let output = memcastle(daemon)
        .args(args)
        .output()
        .await
        .expect("run memcastle");
    let stdout = String::from_utf8_lossy(&output.stdout);
    (
        output.status.success(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[tokio::test]
async fn the_cli_defines_enables_and_fires_triggers_through_the_same_routes() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;

    let (ok, created, stderr) = cli(
        &daemon,
        &[
            "trigger",
            "set",
            "daily",
            "--miner",
            "docs",
            "--type",
            "schedule",
            "--setting",
            "every=1d",
            "--setting",
            "at=03:30",
        ],
    )
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(created["created"], true);
    assert_eq!(
        created["trigger"]["enabled"], false,
        "the CLI never enables on its own"
    );

    let (_, rest) = get(&daemon, "/api/triggers/daily").await;
    assert_eq!(rest["settings"], json!({ "every": "1d", "at": "03:30" }));
    let (ok, listed, stderr) = cli(&daemon, &["trigger", "list"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(
        listed,
        get(&daemon, "/api/triggers").await.1,
        "CLI and REST are one model"
    );

    let (ok, _, stderr) = cli(&daemon, &["trigger", "fire", "daily"]).await;
    assert!(!ok, "a disabled trigger is not fired");
    assert!(stderr.contains("memcastle::trigger::disabled"), "{stderr}");

    let (ok, enabled, stderr) = cli(&daemon, &["trigger", "enable", "daily"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(enabled["trigger"]["status"], "active");
    let (ok, fired, stderr) = cli(&daemon, &["trigger", "fire", "daily"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(fired["outcome"], "queued");

    // The config file holds the trigger and its choice, and nothing secret.
    let text = std::fs::read_to_string(&daemon.config_path).unwrap();
    assert!(
        text.contains("[[triggers]]") && text.contains("enabled = true"),
        "{text}"
    );
    let (ok, _, _) = cli(&daemon, &["trigger", "remove", "daily", "--yes"]).await;
    assert!(ok);
    assert_eq!(get(&daemon, "/api/triggers").await.1["triggers"], json!([]));
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_hand_edit_that_enables_a_trigger_is_noticed_and_one_that_breaks_the_file_keeps_the_last_good_triggers()
 {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(
        &daemon,
        "t",
        json!({ "miner": "docs", "type": "poll", "settings": { "every": "1h" } }),
    )
    .await;

    let text = std::fs::read_to_string(&daemon.config_path).unwrap();
    assert!(
        text.contains("enabled = false"),
        "the file says plainly that it is off: {text}"
    );
    std::fs::write(
        &daemon.config_path,
        text.replace("enabled = false", "enabled = true"),
    )
    .unwrap();
    let (_, shown) = get(&daemon, "/api/triggers/t").await;
    assert_eq!(shown["enabled"], true, "the file is the truth: {shown}");

    std::fs::write(
        &daemon.config_path,
        "[[triggers]]\nname = \"t\"\nminer = \"docs\"\ntype = \"poll\"\n",
    )
    .unwrap();
    let (status, report) = get(&daemon, "/api/triggers").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "reading never fails because the file does"
    );
    assert_eq!(report["triggers"][0]["name"], "t");
    assert!(
        report["error"].as_str().unwrap().contains("every"),
        "{report}"
    );
    daemon.shutdown().await;
}

// ---- the CLI's client -----------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_clients_trigger_methods_speak_the_same_routes_the_cli_prints() {
    use memcastle::app::TriggerPatch;
    use memcastle::domain::{FireOutcome, TriggerMechanism, TriggerStatus};

    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    let bind = daemon
        .base_url
        .trim_start_matches("http://")
        .parse()
        .unwrap();
    let client = memcastle::client::DaemonClient::discover(&daemon.palace_path, bind);

    assert!(client.list_triggers().await.unwrap().triggers.is_empty());

    let created = client
        .set_trigger(
            "t",
            &TriggerPatch {
                miner: Some("docs".to_string()),
                kind: Some(TriggerMechanism::Poll),
                settings: json!({ "every": "1h" }).as_object().cloned().unwrap(),
                ..TriggerPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(created.created && !created.trigger.enabled);
    assert_eq!(
        client.show_trigger("t").await.unwrap().status,
        TriggerStatus::Disabled
    );

    let refused = client.fire_trigger("t").await.unwrap_err();
    assert!(refused.to_string().contains("disabled"), "{refused}");

    let enabled = client.set_trigger_enabled("t", true).await.unwrap();
    assert_eq!(enabled.trigger.status, TriggerStatus::Active);
    assert!(matches!(
        client.fire_trigger("t").await.unwrap(),
        FireOutcome::Queued { .. }
    ));
    assert!(matches!(
        client.fire_trigger("t").await.unwrap(),
        FireOutcome::Coalesced { .. }
    ));
    let off = client.set_trigger_enabled("t", false).await.unwrap();
    assert!(!off.trigger.enabled);

    assert!(client.reload_triggers().await.unwrap().diff.is_empty());
    client.remove_trigger("t").await.unwrap();
    assert!(
        client
            .show_trigger("t")
            .await
            .unwrap_err()
            .to_string()
            .contains("no trigger")
    );
    daemon.shutdown().await;
}

// ---- the read-only agent surface --------------------------------------------------------------------------------------

#[tokio::test]
async fn the_event_stream_tells_the_dashboard_when_a_trigger_fires() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(
        &daemon,
        "t",
        json!({ "miner": "docs", "type": "poll", "settings": { "every": "1h" } }),
    )
    .await;

    let mut events = reqwest::Client::new()
        .get(format!("{}/api/events", daemon.base_url))
        .send()
        .await
        .expect("the event stream");
    // The `open` frame first, so what follows is what happened after the stream was live.
    let mut seen = String::new();
    while !seen.contains("event: open") {
        seen.push_str(&String::from_utf8_lossy(
            &events.chunk().await.unwrap().expect("a frame"),
        ));
    }

    post(&daemon, "/api/triggers/t/enable").await;
    post(&daemon, "/api/triggers/t/fire").await;
    while !(seen.contains("event: trigger") && seen.contains("\"status\":\"queued\"")) {
        seen.push_str(&String::from_utf8_lossy(
            &events.chunk().await.unwrap().expect("a frame"),
        ));
    }
    assert!(
        seen.contains("\"kind\":\"trigger\"") && seen.contains("\"id\":\"t\""),
        "{seen}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn mcp_and_rest_show_the_same_triggers_with_the_same_errors_and_the_same_memory_mode_gates() {
    let (daemon, _port, secrets) = webhook_daemon().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(&daemon, "hook", webhook_body(secrets.path(), "docs")).await;
    put_trigger(
        &daemon,
        "poll",
        json!({ "miner": "docs", "type": "poll", "settings": { "every": "1h" } }),
    )
    .await;

    let session = mcp::connect(&daemon.base_url).await;
    let (_, rest_list) = get(&daemon, "/api/triggers").await;
    let listed = mcp::call(&session, "memcastle_trigger_list", json!({}))
        .await
        .ok();
    assert_eq!(listed, rest_list);
    assert!(
        !listed.to_string().contains(SECRET),
        "a secret is never in an answer"
    );
    assert!(
        !listed.to_string().contains("hook-secret"),
        "nor the path it is kept at"
    );
    let (_, rest_one) = get(&daemon, "/api/triggers/poll").await;
    assert_eq!(
        mcp::call(&session, "memcastle_trigger_get", json!({ "name": "poll" }))
            .await
            .ok(),
        rest_one
    );

    let missing = mcp::call(
        &session,
        "memcastle_trigger_get",
        json!({ "name": "nobody" }),
    )
    .await;
    assert_eq!(missing.error_code(), "memcastle::trigger::not_found");

    mcp::set_mode(&session, "disabled").await;
    for (tool, args) in [
        ("memcastle_trigger_list", json!({})),
        ("memcastle_trigger_get", json!({ "name": "poll" })),
    ] {
        assert_eq!(
            mcp::call(&session, tool, args).await.error_code(),
            "memcastle::mode::forbidden",
            "{tool}"
        );
    }
    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn firing_is_a_write_and_reading_follows_the_memory_mode() {
    let daemon = TestDaemon::start().await;
    let notes = notes_dir();
    put_miner(&daemon, "docs", notes.path()).await;
    put_trigger(
        &daemon,
        "t",
        json!({ "miner": "docs", "type": "poll", "enabled": true, "settings": { "every": "1h" } }),
    )
    .await;
    let with_mode = |method: Method, path: &str, mode: &str| {
        reqwest::Client::new()
            .request(method, format!("{}{path}", daemon.base_url))
            .header("X-MemCastle-Mode", mode)
            .send()
    };

    let read_only_read = with_mode(Method::GET, "/api/triggers", "read_only")
        .await
        .unwrap();
    assert_eq!(read_only_read.status(), StatusCode::OK);
    let read_only_fire = with_mode(Method::POST, "/api/triggers/t/fire", "read_only")
        .await
        .unwrap();
    assert_eq!(
        read_only_fire.status(),
        StatusCode::FORBIDDEN,
        "firing files drawers, so it is a write"
    );
    let disabled_read = with_mode(Method::GET, "/api/triggers/t", "disabled")
        .await
        .unwrap();
    assert_eq!(disabled_read.status(), StatusCode::FORBIDDEN);
    assert!(
        jobs(&daemon).await.is_empty(),
        "a refused firing asked for nothing"
    );
    daemon.shutdown().await;
}
