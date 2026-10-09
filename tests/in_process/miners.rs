//! Persistent miner configuration against a real daemon: the `[[miners]]` section of its configuration file, managed
//! over REST, the CLI and (read-only) MCP, with one set of rules (docs/adr/037).
//!
//! What is tested here is what the daemon does for any miner: validation before anything is written, hand edits being
//! noticed, option changes never broadening silently, and a source's cursor surviving every change to the miner that
//! points at it. What a particular source does with an option is that source's own test.

use crate::common;

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use assert_cmd::cargo::cargo_bin;
use common::{TestDaemon, mcp, wait_for_job_status};
use memcastle::config::{Config, Overrides};
use memcastle::domain::{Job, JobStatus};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::process::Command;

const MODE_HEADER: &str = "X-MemCastle-Mode";

async fn request(
    daemon: &TestDaemon,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = reqwest::Client::new().request(method, format!("{}{path}", daemon.base_url));
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

/// `PUT /api/miners/{name}`.
async fn put(daemon: &TestDaemon, name: &str, body: Value) -> (StatusCode, Value) {
    request(
        daemon,
        Method::PUT,
        &format!("/api/miners/{name}"),
        Some(body),
    )
    .await
}

async fn post(daemon: &TestDaemon, path: &str) -> (StatusCode, Value) {
    request(daemon, Method::POST, path, None).await
}

/// An absolute, existing directory to mine, holding one note whose modification time is fixed so the cursor ordering
/// never depends on how fast the test runs.
fn notes_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("notes.txt");
    std::fs::write(
        &file,
        "How do I rotate the signing keys?\nRun the rotation script.\n",
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000))
        .unwrap();
    dir
}

fn directory(path: &Path) -> Value {
    json!({ "source": "directory", "locator": path })
}

fn names(list: &Value) -> Vec<String> {
    list["miners"]
        .as_array()
        .expect("a miner list")
        .iter()
        .map(|m| m["name"].as_str().expect("a name").to_string())
        .collect()
}

fn code(body: &Value) -> &str {
    body["code"].as_str().unwrap_or("-")
}

#[tokio::test]
async fn several_miners_coexist_with_their_own_settings_and_survive_in_the_file() {
    let daemon = TestDaemon::start().await;
    let (docs, notes) = (notes_dir(), notes_dir());

    let (status, created) = put(
        &daemon,
        "docs",
        json!({
            "source": "directory", "locator": docs.path(), "wing": "documentation",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["created"], true);
    assert_eq!(created["miner"]["state"], "ready");

    put(&daemon, "notes", directory(notes.path())).await;

    let (_, list) = get(&daemon, "/api/miners").await;
    assert_eq!(names(&list), ["docs", "notes"]);
    assert_eq!(list["miners"][0]["wing"], "documentation");
    assert_eq!(
        list["miners"][1]["wing"],
        Value::Null,
        "one miner's settings never leak into another"
    );

    // A daemon that restarted would read exactly this: the file is the miners' only home.
    let reloaded = Config::load(
        Some(&daemon.config_path),
        &Overrides {
            palace: Some(daemon.palace_path.clone()),
            ..Overrides::default()
        },
    )
    .expect("the file the daemon wrote loads");
    assert_eq!(reloaded.miners.len(), 2);
    assert_eq!(reloaded.miners[0].name, "docs");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_miner_can_be_changed_disabled_enabled_and_removed() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;

    // Only what the request names changes.
    let (status, changed) = put(&daemon, "docs", json!({ "wing": "w" })).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(
        (changed["created"].clone(), changed["changed"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(changed["miner"]["source"], "directory");
    assert_eq!(changed["miner"]["locator"], json!(dir.path()));

    // Idempotent: the same request again writes nothing.
    let (_, again) = put(&daemon, "docs", json!({ "wing": "w" })).await;
    assert_eq!(again["changed"], false);

    // A field is cleared by name, never by sending nothing.
    let (_, cleared) = put(&daemon, "docs", json!({ "unset": ["wing"] })).await;
    assert_eq!(cleared["miner"]["wing"], Value::Null);

    let (_, off) = post(&daemon, "/api/miners/docs/disable").await;
    assert_eq!(
        (
            off["miner"]["enabled"].clone(),
            off["miner"]["state"].clone()
        ),
        (json!(false), json!("disabled"))
    );
    let (_, on) = post(&daemon, "/api/miners/docs/enable").await;
    assert_eq!(on["miner"]["state"], "ready");

    let (status, removed) = request(&daemon, Method::DELETE, "/api/miners/docs", None).await;
    assert_eq!(
        (status, removed["removed"].clone()),
        (StatusCode::OK, json!("docs"))
    );
    let (status, missing) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(
        (status, code(&missing)),
        (StatusCode::NOT_FOUND, "memcastle::miner::not_found")
    );
    let (status, missing) = post(&daemon, "/api/miners/docs/enable").await;
    assert_eq!(
        (status, code(&missing)),
        (StatusCode::NOT_FOUND, "memcastle::miner::not_found")
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_invalid_miner_is_refused_before_anything_is_written() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    let relative = json!({ "source": "directory", "locator": "relative/path" });
    let cases = [
        (
            "an unknown source, enabled",
            "chat",
            json!({ "source": "no-such-source" }),
            "unknown source",
        ),
        ("a relative directory", "rel", relative, "relative"),
        (
            "a directory without a locator",
            "bare",
            json!({ "source": "directory" }),
            "needs a `locator`",
        ),
        (
            "a directory with an unsupported option",
            "scoped",
            json!({ "source": "directory", "locator": dir.path(), "options": { "groups": ["a"] } }),
            "unknown option `groups`",
        ),
        (
            "a credential that does not resolve",
            "cred",
            json!({
                "source": "directory", "locator": dir.path(),
                "credential": { "type": "env", "name": "MEMCASTLE_TEST_UNSET_CREDENTIAL" },
            }),
            "MEMCASTLE_TEST_UNSET_CREDENTIAL",
        ),
        (
            "a secret pasted into a setting",
            "leak",
            json!({ "source": "directory", "locator": dir.path(), "options": { "api_key": "hunter2" } }),
            "looks like a secret",
        ),
        (
            "a bad name",
            "Bad Name",
            json!({ "source": "directory" }),
            "not a valid miner name",
        ),
        (
            "an unknown key",
            "typo",
            json!({ "source": "directory", "enable": false }),
            "enable",
        ),
        (
            "no source for a new miner",
            "nosource",
            json!({ "locator": "/x" }),
            "needs a `source`",
        ),
        (
            "a bad wing",
            "wing",
            json!({ "source": "directory", "locator": dir.path(), "wing": "a/b" }),
            "wing",
        ),
    ];
    for (what, name, body, fragment) in cases {
        let (status, error) = put(&daemon, name, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {error}");
        let text = error.to_string();
        assert!(
            text.contains(fragment),
            "{what} should say `{fragment}`: {text}"
        );
        assert!(
            error["help"].as_str().is_some_and(|h| !h.is_empty()),
            "{what} needs a help line: {error}"
        );
    }
    let (_, list) = get(&daemon, "/api/miners").await;
    assert!(
        names(&list).is_empty(),
        "a refused miner must not be half-created"
    );
    assert!(
        !daemon.config_path.exists(),
        "a refused change must not even create the file"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_miner_may_name_a_source_that_is_not_installed_yet() {
    let daemon = TestDaemon::start().await;
    let (status, created) = put(
        &daemon,
        "signal-personal",
        json!({ "source": "signal", "enabled": false, "options": { "contacts": ["+336"] } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["miner"]["state"], "disabled");

    // Enabling is where it is held to account: the source has to exist first.
    let (status, error) = post(&daemon, "/api/miners/signal-personal/enable").await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::BAD_REQUEST, "memcastle::miner::invalid")
    );
    assert!(
        error["error"].as_str().unwrap().contains("unknown source"),
        "{error}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_change_that_may_widen_options_needs_to_say_so() {
    let daemon = TestDaemon::start().await;
    put(
        &daemon,
        "chat",
        json!({ "source": "chat", "enabled": false, "options": { "groups": ["a", "b"], "contacts": ["alice"] } }),
    )
    .await;

    for (what, body) in [
        (
            "a value added to a list",
            json!({ "options": { "groups": ["a", "b", "c"] } }),
        ),
        ("a filter dropped", json!({ "unset_options": ["contacts"] })),
    ] {
        let (status, error) = put(&daemon, "chat", body).await;
        assert_eq!(
            (status, code(&error)),
            (StatusCode::CONFLICT, "memcastle::miner::scope_broadened"),
            "{what}: {error}"
        );
    }
    let (_, unchanged) = get(&daemon, "/api/miners/chat").await;
    assert_eq!(
        unchanged["options"]["groups"],
        json!(["a", "b"]),
        "a refused change leaves the options as they were"
    );

    // Without the source's semantics, even a seemingly narrower change needs acknowledgement.
    let (status, narrowed) = put(
        &daemon,
        "chat",
        json!({ "options": { "groups": ["a"], "since": ["2024"] }, "allow_broaden": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{narrowed}");

    // And a widening that is meant is allowed, once.
    let (status, widened) = put(
        &daemon,
        "chat",
        json!({ "unset_options": ["since"], "allow_broaden": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{widened}");
    assert!(widened["miner"]["options"].get("since").is_none());
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_hand_edit_is_noticed_without_a_restart_and_reported_by_reload() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;

    // The user edits the file by hand: a comment, a second miner, and options on a disabled one.
    let mut text = std::fs::read_to_string(&daemon.config_path).unwrap();
    text.push_str("\n# written by hand\n[[miners]]\nname = \"chat\"\nsource = \"chat\"\nenabled = false\n[miners.options]\ngroups = [\"a\"]\n");
    std::fs::write(&daemon.config_path, text).unwrap();

    let (_, list) = get(&daemon, "/api/miners").await;
    assert_eq!(
        names(&list),
        ["docs", "chat"],
        "the next read sees the edit"
    );
    assert_eq!(list["error"], Value::Null);

    // A second edit, this one widening `chat`: applied as written (the file is the user's), and reported.
    let edited = std::fs::read_to_string(&daemon.config_path)
        .unwrap()
        .replace("groups = [\"a\"]", "groups = [\"a\", \"b\"]");
    std::fs::write(&daemon.config_path, edited).unwrap();
    let (status, reload) = post(&daemon, "/api/miners/reload").await;
    assert_eq!(status, StatusCode::OK, "{reload}");
    assert_eq!(reload["miners"], 2);
    assert_eq!(reload["changed"], json!(["chat"]));
    assert_eq!(reload["broadened"], json!(["chat"]));
    assert_eq!(reload["added"], json!([]));

    // A change made through the daemon afterwards keeps the hand-written comment.
    put(&daemon, "docs", json!({ "wing": "w" })).await;
    assert!(
        std::fs::read_to_string(&daemon.config_path)
            .unwrap()
            .contains("# written by hand")
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_invalid_hand_edit_keeps_the_last_good_miners_and_blocks_writes_until_fixed() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;
    let good = std::fs::read_to_string(&daemon.config_path).unwrap();

    std::fs::write(
        &daemon.config_path,
        format!("{good}\n[[miners]]\nname = \"Bad Name\"\nsource = \"x\"\n"),
    )
    .unwrap();

    let (status, list) = get(&daemon, "/api/miners").await;
    assert_eq!(status, StatusCode::OK, "a stale view is still a view");
    assert_eq!(names(&list), ["docs"], "the last good miners stay in force");
    assert!(
        list["error"]
            .as_str()
            .unwrap()
            .contains("not a valid miner name"),
        "{list}"
    );

    let (status, error) = post(&daemon, "/api/miners/reload").await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::CONFLICT, "memcastle::miner::config_file")
    );
    // Writing would overwrite the very file the user is in the middle of fixing.
    let (status, error) = put(&daemon, "docs", json!({ "wing": "w" })).await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::CONFLICT, "memcastle::miner::config_file")
    );
    assert!(
        std::fs::read_to_string(&daemon.config_path)
            .unwrap()
            .contains("Bad Name"),
        "the broken edit is untouched"
    );

    std::fs::write(&daemon.config_path, &good).unwrap();
    let (status, ok) = put(&daemon, "docs", json!({ "wing": "w" })).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    let (_, list) = get(&daemon, "/api/miners").await;
    assert_eq!(list["error"], Value::Null);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_daemon_without_a_configuration_file_reads_miners_but_refuses_to_change_them() {
    let daemon = TestDaemon::start_configured(|config| config.config_file = None).await;
    let (status, list) = get(&daemon, "/api/miners").await;
    assert_eq!(status, StatusCode::OK);
    assert!(names(&list).is_empty());
    let (status, error) = put(
        &daemon,
        "docs",
        json!({ "source": "directory", "locator": "/tmp" }),
    )
    .await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::CONFLICT, "memcastle::miner::config_file")
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_miners_cursor_survives_every_change_that_keeps_its_source_and_locator() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;

    let run = |daemon: &TestDaemon| {
        let base = daemon.base_url.clone();
        async move {
            let client = reqwest::Client::new();
            let job: Job = client
                .post(format!("{base}/api/miners/docs/run"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .expect("a job");
            wait_for_job_status(&client, &base, job.id, JobStatus::Completed).await
        }
    };
    let first = run(&daemon).await;
    assert_eq!(first.result.as_ref().unwrap()["created"], 1);
    assert_eq!(first.requested_by, "http");

    let (_, mined) = get(&daemon, "/api/miners/docs").await;
    let source_id = mined["source_id"].clone();
    assert!(
        source_id.is_string(),
        "mining the miner's locator creates the source it points at: {mined}"
    );
    assert_eq!(mined["documents"], 1);
    let (_, sources) = get(&daemon, "/api/sources").await;
    let cursor = sources["sources"][0]["cursor"].clone();
    assert!(!cursor.is_null());

    // Everything about the miner except what it points at.
    put(&daemon, "docs", json!({ "wing": "other" })).await;
    post(&daemon, "/api/miners/docs/disable").await;
    post(&daemon, "/api/miners/docs/enable").await;
    request(&daemon, Method::DELETE, "/api/miners/docs", None).await;
    let (_, recreated) = put(&daemon, "docs", directory(dir.path())).await;
    assert_eq!(recreated["identity_changed"], false);

    let (_, after) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(
        after["source_id"], source_id,
        "the same source, so the same cursor"
    );
    assert_eq!(after["documents"], 1);
    let (_, sources) = get(&daemon, "/api/sources").await;
    assert_eq!(
        sources["sources"][0]["cursor"], cursor,
        "no change touched the cursor"
    );

    let second = run(&daemon).await;
    assert_eq!(
        second.result.as_ref().unwrap()["documents"],
        0,
        "it continues from the cursor, reading nothing twice"
    );

    // Pointing the miner somewhere else *is* a different source, and the answer says so.
    let other = notes_dir();
    let (_, moved) = put(
        &daemon,
        "docs",
        json!({ "locator": other.path(), "allow_broaden": true }),
    )
    .await;
    assert_eq!(moved["identity_changed"], true);
    assert_eq!(
        moved["miner"]["documents"], 0,
        "the new place has its own cursor, not the old one's"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_miner_that_cannot_run_says_why_and_is_not_run() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;
    post(&daemon, "/api/miners/docs/disable").await;
    let (status, error) = post(&daemon, "/api/miners/docs/run").await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::CONFLICT, "memcastle::miner::disabled")
    );

    // A hand edit naming an undeclared option cannot be silently ignored by the source.
    let text = std::fs::read_to_string(&daemon.config_path).unwrap();
    std::fs::write(
        &daemon.config_path,
        format!("{text}\n[miners.options]\ngroups = [\"a\"]\n")
            .replace("enabled = false", "enabled = true"),
    )
    .unwrap();
    let (_, shown) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(shown["state"], "unavailable", "{shown}");
    let (status, error) = post(&daemon, "/api/miners/docs/run").await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::CONFLICT, "memcastle::miner::not_runnable")
    );
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("unknown option `groups`"),
        "{error}"
    );

    let (status, error) = post(&daemon, "/api/miners/nobody/run").await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::NOT_FOUND, "memcastle::miner::not_found")
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_miners_saved_options_are_the_options_of_its_run() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    // `since` is the one option the directory source declares, so that key is a filter it applies; the
    // file `notes_dir` writes is dated sixteen minutes into 1970.
    let (status, created) = put(
        &daemon,
        "docs",
        json!({ "source": "directory", "locator": dir.path(), "options": { "since": "1970-01" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");

    let client = reqwest::Client::new();
    let job: Job = client
        .post(format!("{}/api/miners/docs/run", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("a job");
    let done = wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await;
    let wire = serde_json::to_value(&done.kind).unwrap();
    assert_eq!(
        wire["options"],
        json!({ "since": "1970-01" }),
        "the run passes the option on, as `memcastle mine directory <path> since=1970-01` would: {wire}"
    );
    assert_eq!(
        done.result.as_ref().unwrap()["created"],
        1,
        "the file is dated 1970-01-01 00:16, after the option's date, so it is still mined"
    );

    // A date in the far future filters the same file out: the option is applied, not just carried. A second miner
    // keeps the examples independent, so its cursor cannot affect the first miner's run.
    let (status, created) = put(
        &daemon,
        "later",
        json!({ "source": "directory", "locator": dir.path(), "options": { "since": "2999-01" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let job: Job = client
        .post(format!("{}/api/miners/later/run", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .expect("a job");
    let done = wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await;
    assert_eq!(
        done.result.as_ref().unwrap()["documents"],
        0,
        "{:?}",
        done.result
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn temporary_options_are_checked_and_do_not_rewrite_the_miner() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    let (_, created) = put(
        &daemon,
        "docs",
        json!({
            "source": "directory", "locator": dir.path(), "options": {"since": "1970-01"}
        }),
    )
    .await;
    assert_eq!(created["miner"]["state"], "ready", "{created}");
    let original = std::fs::read_to_string(&daemon.config_path).unwrap();

    let (status, error) = request(
        &daemon,
        Method::POST,
        "/api/miners/docs/run",
        Some(json!({"options": {"since": "1969-01"}})),
    )
    .await;
    assert_eq!(
        (status, code(&error)),
        (StatusCode::CONFLICT, "memcastle::miner::scope_broadened")
    );

    let (status, job) = request(
        &daemon,
        Method::POST,
        "/api/miners/docs/run",
        Some(json!({"options": {"since": "2999-01"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["kind"]["options"], json!({"since": "2999-01"}));
    let (_, saved) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(saved["options"], json!({"since": "1970-01"}));
    assert_eq!(
        std::fs::read_to_string(&daemon.config_path).unwrap(),
        original
    );

    let (status, job) = request(
        &daemon,
        Method::POST,
        "/api/miners/docs/run",
        Some(json!({"options": {"since": "1969-01"}, "allow_broaden": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["kind"]["options"], json!({"since": "1969-01"}));
    daemon.shutdown().await;
}

#[tokio::test]
async fn legacy_miner_fields_are_refused_with_a_migration_hint() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;
    let (status, error) = put(&daemon, "docs", json!({"scope": {"since": "1970-01"}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    let text = std::fs::read_to_string(&daemon.config_path).unwrap();
    std::fs::write(
        &daemon.config_path,
        format!("{text}\n[miners.scope]\nsince = \"1970-01\"\n"),
    )
    .unwrap();
    let startup = Config::load(Some(&daemon.config_path), &Overrides::default())
        .expect_err("a new daemon must refuse a legacy table");
    assert!(
        startup.to_string().contains("[miners.options]"),
        "{startup}"
    );
    let (_, report) = get(&daemon, "/api/miners").await;
    assert!(
        report["error"]
            .as_str()
            .unwrap_or("")
            .contains("[miners.options]"),
        "{report}"
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_credential_is_checked_but_never_shown_or_stored() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    let secret_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(secret_file.path(), "hunter2\n").unwrap();

    let (status, created) = put(
        &daemon,
        "docs",
        json!({
            "source": "directory", "locator": dir.path(),
            "credential": { "type": "file", "path": secret_file.path() },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(
        created["miner"]["credential"],
        json!({ "kind": "file", "available": true })
    );

    // Nothing a client can read names where the daemon keeps its secrets, and the value is never read.
    let (_, list) = get(&daemon, "/api/miners").await;
    let (_, config) = get(&daemon, "/api/config").await;
    for body in [list.to_string(), created.to_string(), config.to_string()] {
        assert!(
            !body.contains(secret_file.path().to_str().unwrap()),
            "the path leaked: {body}"
        );
        assert!(!body.contains("hunter2"), "the secret leaked: {body}");
    }
    // The file holds the reference, which is the point: the secret lives elsewhere.
    let stored = std::fs::read_to_string(&daemon.config_path).unwrap();
    assert!(stored.contains("type = \"file\""), "{stored}");
    assert!(!stored.contains("hunter2"));

    // A credential that disappears makes the miner unavailable, with the fix, rather than failing a run later.
    drop(secret_file);
    let (_, shown) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(shown["state"], "unavailable");
    assert_eq!(shown["credential"]["available"], false);
    daemon.shutdown().await;
}

#[tokio::test]
async fn reading_follows_the_memory_mode_and_running_is_a_write_while_configuring_is_neither() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;
    let client = reqwest::Client::new();
    let with_mode = |method: Method, path: &str, mode: &str| {
        client
            .request(method, format!("{}{path}", daemon.base_url))
            .header(MODE_HEADER, mode)
            .send()
    };

    let off = with_mode(Method::GET, "/api/miners", "disabled")
        .await
        .unwrap();
    assert_eq!(
        off.status(),
        StatusCode::FORBIDDEN,
        "a disabled session reads no miners"
    );
    assert_eq!(
        with_mode(Method::GET, "/api/miners", "read_only")
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // Mining files drawers, so a read-only session cannot start it.
    assert_eq!(
        with_mode(Method::POST, "/api/miners/docs/run", "read_only")
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // The mode governs memory, not what the daemon is configured to mine: the administrator's calls are not gated by it.
    assert_eq!(
        with_mode(Method::POST, "/api/miners/docs/disable", "read_only")
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    daemon.shutdown().await;
}

#[tokio::test]
async fn mcp_and_rest_show_the_same_miners_and_mcp_cannot_change_them() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    put(&daemon, "docs", directory(dir.path())).await;
    put(
        &daemon,
        "chat",
        json!({ "source": "chat", "enabled": false, "options": { "groups": ["a"] } }),
    )
    .await;

    let session = mcp::connect(&daemon.base_url).await;
    let (_, rest_list) = get(&daemon, "/api/miners").await;
    assert_eq!(
        mcp::call(&session, "memcastle_miner_list", json!({}))
            .await
            .ok(),
        rest_list
    );
    let (_, rest_one) = get(&daemon, "/api/miners/chat").await;
    assert_eq!(
        mcp::call(&session, "memcastle_miner_get", json!({ "name": "chat" }))
            .await
            .ok(),
        rest_one
    );

    // The same error, with the same code, as REST.
    let missing = mcp::call(&session, "memcastle_miner_get", json!({ "name": "nobody" })).await;
    assert_eq!(missing.error_code(), "memcastle::miner::not_found");

    // The same memory-mode rules as REST reads.
    mcp::set_mode(&session, "disabled").await;
    assert_eq!(
        mcp::call(&session, "memcastle_miner_list", json!({}))
            .await
            .error_code(),
        "memcastle::mode::forbidden"
    );
    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

/// The `memcastle` binary pointed at `daemon`'s palace, so it finds the daemon through its registry file.
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
async fn the_cli_creates_changes_and_reads_miners_through_the_same_routes() {
    let daemon = TestDaemon::start().await;
    let dir = notes_dir();
    let locator = dir.path().to_str().unwrap();

    let (ok, created, stderr) = cli(
        &daemon,
        &[
            "miner",
            "set",
            "docs",
            "--source",
            "directory",
            "--locator",
            locator,
            "--wing",
            "w",
            "--option",
            "since=2020-01",
        ],
    )
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(created["created"], true);

    // What the CLI wrote is what REST reads, and what `miner list` prints is what REST answers.
    let (_, rest) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(rest["options"], json!({ "since": "2020-01" }));
    let (ok, listed, stderr) = cli(&daemon, &["miner", "list"]).await;
    assert!(ok, "{stderr}");
    let (_, rest_list) = get(&daemon, "/api/miners").await;
    assert_eq!(listed, rest_list, "CLI and REST are one model");
    let (_, got, _) = cli(&daemon, &["miner", "get", "docs"]).await;
    assert_eq!(got, rest);

    let (ok, job, stderr) = cli(&daemon, &["miner", "run", "docs", "since=2999-01"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(job["kind"]["options"], json!({ "since": "2999-01" }));
    let (_, saved) = get(&daemon, "/api/miners/docs").await;
    assert_eq!(saved["options"], json!({ "since": "2020-01" }));

    let (ok, disabled, stderr) = cli(&daemon, &["miner", "disable", "docs"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(disabled["miner"]["enabled"], false);
    let (ok, _, _) = cli(&daemon, &["miner", "enable", "docs"]).await;
    assert!(ok);

    // The same rules: a widening is refused with the same diagnostic, and the CLI says how to allow it.
    put(
        &daemon,
        "chat",
        json!({ "source": "chat", "enabled": false, "options": { "groups": ["a"] } }),
    )
    .await;
    let (ok, _, stderr) = cli(&daemon, &["miner", "set", "chat", "--option", "groups=a,b"]).await;
    assert!(!ok);
    assert!(
        stderr.contains("memcastle::miner::scope_broadened") && stderr.contains("--allow-broaden"),
        "{stderr}"
    );
    let (ok, widened, stderr) = cli(
        &daemon,
        &[
            "miner",
            "set",
            "chat",
            "--option",
            "groups=a,b",
            "--allow-broaden",
        ],
    )
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(widened["miner"]["options"]["groups"], json!("a,b"));

    let (ok, removed, stderr) = cli(&daemon, &["miner", "remove", "chat", "--yes"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(removed["removed"], "chat");
    let (_, left) = get(&daemon, "/api/miners").await;
    assert_eq!(names(&left), ["docs"]);

    let (ok, reload, stderr) = cli(&daemon, &["miner", "reload"]).await;
    assert!(ok, "{stderr}");
    assert_eq!(reload["miners"], 1);
    daemon.shutdown().await;
}
