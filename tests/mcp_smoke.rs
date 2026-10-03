//! End-to-end smoke test of the Phase 1 MCP surface, with nothing mocked or in-process.
//!
//! A real `memcastle serve` subprocess is driven by a real MCP client (rmcp's streamable HTTP transport)
//! through the `/mcp` endpoint, the way an agent harness would.
//! It proves, in one scenario over one palace:
//!
//! - the palace is migrated *before* the first client can connect;
//! - representative read and write tools work through the protocol;
//! - a second, concurrent MCP session sees the same palace;
//! - everything survives a daemon restart, and a brand-new MCP session can reconnect to the same palace.
//!
//! It is a subprocess test because SurrealKV's file lock is not released within one process,
//! so a restart cannot be simulated in-process (see `tests/persistence.rs`).
//! It deliberately touches no web GUI and no Pi/OpenCode integration.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use memcastle::server::lifecycle::RuntimeInfo;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService, serve_client};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

type Session = RunningService<RoleClient, ClientConfig>;

/// Long enough for a cold, instrumented CI runner, short enough to fail before nextest's own kill.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// A `memcastle` command scoped to `palace`, with every ambient setting cleared and the XDG directories under `root`.
/// A developer's own config, palace or daemon can then neither leak into the test nor be disturbed by it.
fn memcastle(root: &Path, palace: &Path) -> Command {
    let mut cmd = Command::new(cargo_bin("memcastle"));
    for name in [
        "MEMCASTLE_CONFIG",
        "MEMCASTLE_BIND",
        "MEMCASTLE_PORT",
        "MEMCASTLE_MODE",
        "MEMCASTLE_AUTH_ENABLED",
        "MEMCASTLE_AUTH_TOKEN",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("MEMCASTLE_PALACE_PATH", palace)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .stdin(Stdio::null());
    cmd
}

/// The registry entry of the daemon started under `root`, once it has written one.
/// `XDG_STATE_HOME` is private to `root`, so scanning finds this daemon's file and nothing else;
/// `read_if_live` would look in the test process's own state directory instead.
fn registered(root: &Path) -> Option<RuntimeInfo> {
    let run = root.join("state/memcastle/run");
    for entry in std::fs::read_dir(run).ok()?.flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path().join("daemon.json"))
            && let Ok(info) = serde_json::from_str(&text)
        {
            return Some(info);
        }
    }
    None
}

/// Read what a child wrote to stderr without waiting for an EOF a live process never sends.
async fn drain_stderr(child: &mut Child) -> String {
    let mut stderr = String::new();
    if let Some(mut out) = child.stderr.take() {
        let _ =
            tokio::time::timeout(Duration::from_millis(500), out.read_to_string(&mut stderr)).await;
    }
    stderr
}

/// Start `memcastle serve` on an OS-assigned port and wait until it has registered, i.e. is really serving.
/// The registry file is written only after migrations ran, so returning here also means the palace is migrated.
async fn start_daemon(root: &Path, palace: &Path) -> (Child, String) {
    let mut child = memcastle(root, palace)
        .arg("serve")
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // A failing assertion must not leak a process holding the palace lock and a port.
        .kill_on_drop(true)
        .spawn()
        .expect("spawn `memcastle serve`");

    // Generous: startup is slow on instrumented CI (see `common::wait_for_registry`).
    for _ in 0..1200 {
        if let Some(info) = registered(root) {
            return (child, format!("http://{}", info.bind_addr));
        }
        if let Ok(Some(status)) = child.try_wait() {
            let stderr = drain_stderr(&mut child).await;
            panic!("daemon exited with {status} before registering; stderr:\n{stderr}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stderr = drain_stderr(&mut child).await;
    panic!("daemon did not register within 60s; stderr so far:\n{stderr}");
}

/// Ask the daemon to stop through the CLI and wait for the process to exit,
/// so the palace lock is released before the next daemon (or `migrate --status`) opens it.
async fn stop_daemon(root: &Path, palace: &Path, child: &mut Child) {
    let status = memcastle(root, palace)
        .arg("stop")
        .status()
        .await
        .expect("run `memcastle stop`");
    assert!(status.success(), "`memcastle stop` should succeed");
    tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("daemon exits within 15s of being asked to stop")
        .expect("wait for daemon");
}

/// `memcastle migrate --status` against a palace no daemon holds open.
async fn migrate_status(root: &Path, palace: &Path) -> Value {
    let output = memcastle(root, palace)
        .args(["migrate", "--status"])
        .output()
        .await
        .expect("run `memcastle migrate --status`");
    assert!(
        output.status.success(),
        "`migrate --status` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("`migrate --status` prints JSON")
}

/// Open a fresh MCP session: its own `initialize` handshake and therefore its own session id.
async fn connect(base_url: &str) -> Session {
    let transport = StreamableHttpClientTransport::from_uri(format!("{base_url}/mcp"));
    tokio::time::timeout(
        STEP_TIMEOUT,
        serve_client(ClientConfig::default(), transport),
    )
    .await
    .expect("mcp session initializes in time")
    .expect("mcp session initializes")
}

/// Call a tool and parse its text content as JSON.
/// A tool failure is a successful JSON-RPC round trip whose result is flagged `is_error`,
/// so it is asserted on explicitly rather than surfacing as a transport error.
async fn call(session: &Session, name: &'static str, arguments: Value) -> Value {
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("tool arguments are a JSON object");
    let result = tokio::time::timeout(
        STEP_TIMEOUT,
        session
            .peer()
            .call_tool(CallToolRequestParams::new(name).with_arguments(arguments)),
    )
    .await
    .unwrap_or_else(|_| panic!("{name} did not answer within {STEP_TIMEOUT:?}"))
    .unwrap_or_else(|e| panic!("{name} failed at the protocol level: {e}"));

    let text = serde_json::to_value(&result.content)
        .expect("content serializes")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !result.is_error.unwrap_or(false),
        "{name} was rejected: {text}"
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} returned non-JSON ({e}): {text}"))
}

/// Poll `memcastle_job_get` until the job completes; any other terminal state is a failure.
async fn wait_for_completion(session: &Session, job_id: &str) {
    for _ in 0..600 {
        let job = call(session, "memcastle_job_get", json!({ "id": job_id })).await;
        match job["status"].as_str() {
            Some("completed") => return,
            Some("failed" | "cancelled") => panic!("job {job_id} ended badly: {job}"),
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("job {job_id} did not complete within 30s");
}

/// The `content` of every hit or drawer in a tool's JSON array result.
fn contents(result: &Value) -> Vec<&str> {
    result
        .as_array()
        .expect("result is a list")
        .iter()
        .filter_map(|item| item["content"].as_str())
        .collect()
}

/// Assert the daemon reports a fully migrated, healthy datastore, and return its drawer count.
async fn assert_migrated_and_count_drawers(session: &Session) -> u64 {
    let status = call(session, "memcastle_status", json!({})).await;
    let datastore = &status["datastore"];
    assert_eq!(datastore["ok"], true, "datastore unhealthy: {status}");
    assert_eq!(
        datastore["pending"],
        json!([]),
        "a serving daemon has no pending migrations: {status}"
    );
    assert!(
        datastore["migration_version"].as_u64().unwrap_or(0) > 0,
        "the migration watermark was never recorded: {status}"
    );
    assert_eq!(
        datastore["migration_version"], datastore["latest_version"],
        "the palace is behind the shipped migrations: {status}"
    );
    status["drawer_count"]
        .as_u64()
        .expect("status reports a drawer count")
}

#[tokio::test]
async fn the_mcp_surface_serves_a_migrated_palace_and_survives_a_daemon_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let palace = root.join("palace");

    // A never-served palace is behind every shipped migration,
    // so the "migrated" assertions below can only pass if serving really migrated it.
    let before = migrate_status(root, &palace).await;
    assert_eq!(before["current_version"], 0, "fresh palace: {before}");
    assert!(
        !before["pending"]
            .as_array()
            .expect("pending list")
            .is_empty(),
        "a fresh palace has migrations pending: {before}"
    );

    // --- Daemon #1: migrations, tool surface, writes and reads ---
    let (mut daemon, base_url) = start_daemon(root, &palace).await;
    let session_a = connect(&base_url).await;

    let tools = tokio::time::timeout(STEP_TIMEOUT, session_a.peer().list_all_tools())
        .await
        .expect("tools/list answers in time")
        .expect("tools/list succeeds");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    for expected in [
        "memcastle_status",
        "memcastle_search",
        "memcastle_recall",
        "memcastle_wake_up",
        "memcastle_mine",
        "memcastle_checkpoint",
        "memcastle_audit",
        "memcastle_diary_write",
        "memcastle_diary_read",
        "memcastle_repair",
        "memcastle_job_list",
        "memcastle_job_get",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    // The first client to get an answer already sees a migrated palace: nothing served it before migrations ran.
    assert_eq!(assert_migrated_and_count_drawers(&session_a).await, 0);

    // Write path 1: a synchronous diary entry.
    let diary_marker = "smokediarymarker the daemon must remember this entry";
    let written = call(
        &session_a,
        "memcastle_diary_write",
        json!({ "agent_identity": "smoke", "wing": "smoke-wing", "content": diary_marker }),
    )
    .await;
    assert_eq!(written["content"], diary_marker);

    // Write path 2: an asynchronous checkpoint job, which must be polled to completion before it is searchable.
    let checkpoint_marker = "smokecheckpointmarker durable fact from a checkpoint";
    let job = call(
        &session_a,
        "memcastle_checkpoint",
        json!({
            "payload": { "items": [{
                "destination": "general",
                "content": checkpoint_marker,
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "smoke" },
                "fact": null,
            }] },
        }),
    )
    .await;
    wait_for_completion(&session_a, job["id"].as_str().expect("job has an id")).await;

    // Write path 3: some MCP clients JSON-encode the payload into a string;
    // that must be stored too, not bounce the agent over to the CLI.
    let stringified_marker = "smokestringifiedmarker payload sent as a json string";
    let job = call(
        &session_a,
        "memcastle_checkpoint",
        json!({
            "payload": json!({ "items": [{
                "destination": "general",
                "content": stringified_marker,
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "smoke" },
                "fact": null,
            }] }).to_string(),
        }),
    )
    .await;
    wait_for_completion(&session_a, job["id"].as_str().expect("job has an id")).await;
    let hits = call(
        &session_a,
        "memcastle_search",
        json!({ "query": "smokestringifiedmarker" }),
    )
    .await;
    assert_eq!(contents(&hits), [stringified_marker]);

    // Read paths: diary read and lexical search see both writes.
    let diary = call(
        &session_a,
        "memcastle_diary_read",
        json!({ "agent_identity": "smoke", "wing": "smoke-wing" }),
    )
    .await;
    assert_eq!(contents(&diary), [diary_marker]);
    let hits = call(
        &session_a,
        "memcastle_search",
        json!({ "query": "smokecheckpointmarker" }),
    )
    .await;
    assert_eq!(contents(&hits), [checkpoint_marker]);

    let drawers_before_restart = assert_migrated_and_count_drawers(&session_a).await;
    assert!(
        drawers_before_restart >= 2,
        "both writes should be counted, got {drawers_before_restart}"
    );

    // A second, concurrent session is a separate handshake against the same palace.
    let session_b = connect(&base_url).await;
    let hits = call(
        &session_b,
        "memcastle_search",
        json!({ "query": "smokediarymarker" }),
    )
    .await;
    assert_eq!(
        contents(&hits),
        [diary_marker],
        "a second session must see what the first wrote"
    );

    // --- Restart: close both sessions, stop the daemon, and check the palace on disk ---
    session_a.cancel().await.expect("close session A");
    session_b.cancel().await.expect("close session B");
    stop_daemon(root, &palace, &mut daemon).await;

    let after = migrate_status(root, &palace).await;
    assert_eq!(
        after["current_version"], after["latest_version"],
        "serving must leave the palace fully migrated: {after}"
    );
    assert_eq!(after["pending"], json!([]));

    // --- Daemon #2: same palace, new process, new port, new session ---
    let (mut daemon, base_url) = start_daemon(root, &palace).await;
    let session_c = connect(&base_url).await;

    assert_eq!(
        assert_migrated_and_count_drawers(&session_c).await,
        drawers_before_restart,
        "no drawer may be lost or duplicated by a restart"
    );
    let diary = call(
        &session_c,
        "memcastle_diary_read",
        json!({ "agent_identity": "smoke", "wing": "smoke-wing" }),
    )
    .await;
    assert_eq!(contents(&diary), [diary_marker]);
    let hits = call(
        &session_c,
        "memcastle_search",
        json!({ "query": "smokecheckpointmarker" }),
    )
    .await;
    assert_eq!(contents(&hits), [checkpoint_marker]);

    // The reopened palace still accepts writes, not just reads.
    let second_entry = "smokesecondentry written after the restart";
    call(
        &session_c,
        "memcastle_diary_write",
        json!({ "agent_identity": "smoke", "wing": "smoke-wing", "content": second_entry }),
    )
    .await;
    let diary = call(
        &session_c,
        "memcastle_diary_read",
        json!({ "agent_identity": "smoke", "wing": "smoke-wing" }),
    )
    .await;
    // Newest first, so the post-restart entry leads and the pre-restart one is kept behind it.
    assert_eq!(contents(&diary), [second_entry, diary_marker]);

    session_c.cancel().await.expect("close session C");
    stop_daemon(root, &palace, &mut daemon).await;
}
