//! The daemon half of the integration conformance matrix (`docs/integration-contract.md`).
//!
//! An integration (Pi, OpenCode, ...) owns *when* to call MemCastle; this file proves that what it calls behaves as
//! the contract says, against a real daemon over a real MCP session. The expectations come from the
//! language-neutral fixtures in `tests/fixtures/integration/`, so a client-side suite written in another language
//! replays the very same files, and a new integration implements the same matrix instead of rediscovering it.
//!
//! What this cannot prove, and the contract assigns to each integration's own tests: that the client calls these
//! operations at the right lifecycle point, and that a disabled session injects no MemCastle-derived context.

mod common;

use std::collections::BTreeSet;

use common::TestDaemon;
use common::mcp::{
    Session, call, connect, contents, fixture, reported_mode, set_mode, substitute, wait_for_job,
};
use memcastle::config::Secret;
use serde_json::{Value, json};

/// A one-item `general` checkpoint payload whose content is exactly `content`.
fn checkpoint_of(content: &str) -> Value {
    json!({ "items": [{
        "destination": "general",
        "content": content,
        "tags": [],
        "source": { "kind": "manual", "uri": null, "agent": "conformance-agent" },
        "fact": null,
    }] })
}

/// Submit `payload` as a checkpoint and wait until it is stored, so the next read can see it.
async fn checkpoint_and_wait(session: &Session, payload: Value) {
    let job = call(
        session,
        "memcastle_checkpoint",
        json!({ "payload": payload }),
    )
    .await
    .ok();
    wait_for_job(session, job["id"].as_str().expect("a job id"), "completed").await;
}

/// The wire mode for each client-facing label, in the order the fixture lists them.
fn mode_labels() -> Vec<(String, String)> {
    fixture("modes.json")["labels"]
        .as_object()
        .expect("labels is an object")
        .iter()
        .map(|(label, wire)| {
            (
                label.clone(),
                wire.as_str().expect("a wire mode").to_owned(),
            )
        })
        .collect()
}

/// The ids in the first column of the documented conformance matrix, which is the table directly under the
/// `## Conformance matrix` heading of the contract page. The per-capability `###` sections after it hold other
/// tables (the failure classes) that must not be mistaken for it.
fn documented_capability_ids() -> BTreeSet<String> {
    let page = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/integration-contract.md"
    ))
    .expect("docs/integration-contract.md exists");
    let section = page
        .split("\n## ")
        .find(|section| section.starts_with("Conformance matrix"))
        .expect("the contract has a `## Conformance matrix` section");
    let matrix = section.split("\n### ").next().expect("a matrix");
    matrix
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once('`'))
        .map(|(id, _)| id.to_owned())
        .collect()
}

#[tokio::test]
async fn the_capability_manifest_matches_the_documented_conformance_matrix() {
    let manifest = fixture("capabilities.json");
    let capabilities = manifest["capabilities"]
        .as_array()
        .expect("capabilities is a list");
    let manifest_ids: BTreeSet<String> = capabilities
        .iter()
        .map(|capability| capability["id"].as_str().expect("an id").to_owned())
        .collect();

    assert_eq!(
        documented_capability_ids(),
        manifest_ids,
        "docs/integration-contract.md and tests/fixtures/integration/capabilities.json must list the same capabilities"
    );

    // Every operation the manifest names is a tool the daemon really registers, and every named test really
    // exists, so renaming either side cannot leave the matrix pointing at nothing.
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    let registered: BTreeSet<String> = session
        .peer()
        .list_all_tools()
        .await
        .expect("tools/list")
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let this_file = include_str!("integration_contract.rs");
    for capability in capabilities {
        let id = capability["id"].as_str().unwrap();
        for operation in capability["operations"].as_array().unwrap() {
            let operation = operation.as_str().unwrap();
            assert!(
                registered.contains(operation),
                "`{id}` names `{operation}`, which is not a registered MCP tool"
            );
        }
        if let Some(test) = capability["daemon_test"].as_str() {
            assert!(
                this_file.contains(&format!("async fn {test}(")),
                "`{id}` names the daemon test `{test}`, which does not exist"
            );
        }
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn three_sessions_in_three_modes_share_one_daemon_and_read_back_their_own_mode() {
    let daemon = TestDaemon::start().await;

    // Every label an integration may offer its user becomes one session, all open at once on the same daemon.
    let mut sessions = Vec::new();
    for (label, wire) in mode_labels() {
        let session = connect(&daemon.base_url).await;
        set_mode(&session, &wire).await;
        sessions.push((label, wire, session));
    }
    assert_eq!(sessions.len(), 3, "full, read-only and off");

    // Only after all three are configured, so one session choosing a mode visibly cannot have changed another.
    for (label, wire, session) in &sessions {
        assert_eq!(
            &reported_mode(session).await,
            wire,
            "the `{label}` session must read back its own mode"
        );
    }
    // `off` is the client's label; the wire value is `disabled`.
    let off = &sessions
        .iter()
        .find(|(label, ..)| label == "off")
        .expect("an `off` label")
        .1;
    assert_eq!(off, "disabled");

    for (.., session) in sessions {
        session.cancel().await.expect("close session");
    }
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_client_label_is_not_a_wire_mode_and_a_refused_mode_leaves_the_session_unchanged() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;

    // The label `off` (and the other near misses) must be translated by the client; sending one must fail loudly
    // instead of silently leaving the session in `full`.
    for rejected in fixture("modes.json")["rejected_wire_values"]
        .as_array()
        .expect("a list")
    {
        let outcome = call(&session, "memcastle_set_mode", json!({ "mode": rejected })).await;
        assert_eq!(
            outcome.error_code(),
            "memcastle::input::invalid",
            "`{rejected}` must be refused"
        );
    }
    assert_eq!(reported_mode(&session).await, "full");

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn every_fixture_mode_outcome_holds_for_every_gated_tool() {
    let daemon = TestDaemon::start().await;
    let mine_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(mine_dir.path().join("note.txt"), "something to mine").expect("write a file");
    let mine_path = mine_dir.path().to_str().expect("a UTF-8 path").to_owned();

    let modes = fixture("modes.json");
    let forbidden_code = modes["forbidden_code"].as_str().expect("a code");
    for (wire, rules) in modes["rules"].as_object().expect("rules is an object") {
        let session = connect(&daemon.base_url).await;
        set_mode(&session, wire).await;

        for operation in modes["operations"]
            .as_array()
            .expect("operations is a list")
        {
            let id = operation["id"].as_str().expect("an id");
            let class = operation["class"].as_str().expect("a class");
            let mut arguments = operation["arguments"].clone();
            substitute(&mut arguments, "mine_dir", &mine_path);

            let outcome = call(
                &session,
                operation["tool"].as_str().expect("a tool"),
                arguments,
            )
            .await;

            match rules[class].as_str().expect("a rule") {
                "ok" => assert!(
                    !outcome.is_error,
                    "`{id}` ({class}) must succeed in `{wire}`: {}",
                    outcome.text
                ),
                "mode_forbidden" => assert_eq!(
                    outcome.error_code(),
                    forbidden_code,
                    "`{id}` ({class}) must be refused in `{wire}`"
                ),
                other => panic!("the fixture has an unknown rule `{other}`"),
            }
        }
        session.cancel().await.expect("close session");
    }

    daemon.shutdown().await;
}

#[tokio::test]
async fn wake_up_returns_the_diary_and_checkpointed_highlights_within_its_limits() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    let wing = "wake-wing";

    // A newcomer's wake-up is empty, not an error: an integration injects nothing and moves on.
    let empty = call(
        &session,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent", "wing": wing }),
    )
    .await
    .ok();
    assert_eq!(empty["diary"], Value::Null);
    assert_eq!(empty["recent_highlights"], json!([]));

    call(
        &session,
        "memcastle_diary_write",
        json!({
            "agent_identity": "conformance-agent",
            "wing": wing,
            "content": "wakediarytoken yesterday I fixed the parser",
        }),
    )
    .await
    .ok();
    for content in [
        "wakehighlightone a first durable decision",
        "wakehighlighttwo a second durable decision",
    ] {
        // Filed under the wing being woken up, because a wake-up scoped to a wing only reads that wing.
        checkpoint_and_wait(
            &session,
            json!({ "items": [{
                "destination": "project",
                "wing": wing,
                "content": content,
                "tags": [],
                "source": { "kind": "manual", "uri": null, "agent": "conformance-agent" },
                "fact": null,
            }] }),
        )
        .await;
    }

    let context = call(
        &session,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent", "wing": wing }),
    )
    .await
    .ok();
    assert_eq!(
        context["diary"]["content"],
        "wakediarytoken yesterday I fixed the parser"
    );
    let highlights = contents(&context["recent_highlights"]);
    assert_eq!(highlights.len(), 2, "{context}");
    assert!(context["generated_at"].is_string(), "{context}");

    // The budget is the integration's protection for its own prompt, so it must hold.
    let one = call(
        &session,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent", "wing": wing, "max_items": 1 }),
    )
    .await
    .ok();
    assert_eq!(contents(&one["recent_highlights"]).len(), 1, "{one}");

    let no_room = call(
        &session,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent", "wing": wing, "max_bytes": 1 }),
    )
    .await
    .ok();
    assert_eq!(no_room["recent_highlights"], json!([]), "{no_room}");
    assert_eq!(
        no_room["diary"]["content"], "wakediarytoken yesterday I fixed the parser",
        "the byte budget bounds the highlights only, never the diary entry"
    );

    // Without a wing there is no diary lookup at all.
    let unscoped = call(
        &session,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent" }),
    )
    .await
    .ok();
    assert_eq!(unscoped["diary"], Value::Null, "{unscoped}");

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn recall_returns_checkpointed_content_verbatim() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;

    // Whitespace, punctuation and non-ASCII text are what a summarising layer would quietly "tidy", and what an
    // integration must be able to quote back word for word.
    let content = "recalltoken  keeps\texact   spacing, “quotes” and 日本語 — verbatim";
    checkpoint_and_wait(&session, checkpoint_of(content)).await;

    for tool in ["memcastle_recall", "memcastle_search"] {
        let hits = call(&session, tool, json!({ "query": "recalltoken" }))
            .await
            .ok();
        assert_eq!(
            contents(&hits),
            [content],
            "{tool} must return the stored text"
        );
    }

    // Nothing matching is an empty list, which an integration must read as "nothing remembered", not as an error.
    let none = call(
        &session,
        "memcastle_recall",
        json!({ "query": "tokenthatnothingstores" }),
    )
    .await
    .ok();
    assert_eq!(none, json!([]));

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn every_valid_checkpoint_fixture_is_accepted_and_every_invalid_one_is_refused_with_its_code()
{
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    let fixtures = fixture("checkpoint-payloads.json");

    // Invalid first, so the empty job list afterwards proves a refusal is made before any job exists.
    for case in fixtures["invalid"].as_array().expect("a list") {
        let name = case["name"].as_str().expect("a name");
        let outcome = call(
            &session,
            "memcastle_checkpoint",
            json!({ "payload": case["payload"] }),
        )
        .await;
        assert_eq!(
            outcome.error_code(),
            case["expected_code"].as_str().expect("a code"),
            "`{name}` must be refused with its documented code"
        );
    }
    assert_eq!(
        call(&session, "memcastle_job_list", json!({})).await.ok(),
        json!([]),
        "a refused checkpoint must not leave a job behind"
    );

    for case in fixtures["valid"].as_array().expect("a list") {
        let name = case["name"].as_str().expect("a name");
        let token = case["recall_token"].as_str().expect("a token");

        checkpoint_and_wait(&session, case["payload"].clone()).await;

        let hits = call(&session, "memcastle_recall", json!({ "query": token }))
            .await
            .ok();
        assert!(
            contents(&hits)
                .iter()
                .any(|content| content.contains(token)),
            "`{name}` must be recallable once its job completes: {hits}"
        );
    }

    // A client that JSON-encodes the payload into a string is tolerated, so it need not fall back to the CLI.
    let valid = &fixtures["valid"][0];
    let stringified = call(
        &session,
        "memcastle_checkpoint",
        json!({ "payload": valid["payload"].to_string() }),
    )
    .await;
    stringified.ok();

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_emergency_checkpoint_is_critical_and_a_normal_one_is_high_priority() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;

    let normal = call(
        &session,
        "memcastle_checkpoint",
        json!({ "payload": checkpoint_of("a routine checkpoint") }),
    )
    .await
    .ok();
    let emergency = call(
        &session,
        "memcastle_checkpoint",
        json!({ "payload": checkpoint_of("an emergency checkpoint"), "emergency": true }),
    )
    .await
    .ok();

    // The scheduler claims higher numbers first, so what the integration must be able to rely on is the number.
    assert_eq!(normal["priority"], 75, "{normal}");
    assert_eq!(emergency["priority"], 100, "{emergency}");
    assert_eq!(emergency["requested_by"], "mcp", "{emergency}");

    for job in [&normal, &emergency] {
        wait_for_job(&session, job["id"].as_str().unwrap(), "completed").await;
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn one_persistent_session_serves_wake_up_recall_and_checkpoint_without_reconnecting() {
    let daemon = TestDaemon::start().await;

    // One handshake carries the whole lifecycle: select the mode once, then wake up, write, and read it back.
    let full = connect(&daemon.base_url).await;
    set_mode(&full, "full").await;
    call(
        &full,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent" }),
    )
    .await
    .ok();
    checkpoint_and_wait(
        &full,
        checkpoint_of("persistenttoken one session, many calls"),
    )
    .await;
    let hits = call(
        &full,
        "memcastle_recall",
        json!({ "query": "persistenttoken" }),
    )
    .await
    .ok();
    assert_eq!(contents(&hits).len(), 1, "{hits}");

    // The mode chosen at the start is still in force many calls later, with no re-negotiation.
    let read_only = connect(&daemon.base_url).await;
    set_mode(&read_only, "read_only").await;
    call(
        &read_only,
        "memcastle_wake_up",
        json!({ "agent_identity": "conformance-agent" }),
    )
    .await
    .ok();
    call(
        &read_only,
        "memcastle_recall",
        json!({ "query": "persistenttoken" }),
    )
    .await
    .ok();
    assert_eq!(
        call(
            &read_only,
            "memcastle_checkpoint",
            json!({ "payload": checkpoint_of("must not be stored") }),
        )
        .await
        .error_code(),
        "memcastle::app::mode_forbidden"
    );
    assert_eq!(reported_mode(&read_only).await, "read_only");

    full.cancel().await.expect("close session");
    read_only.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn mining_is_background_priority_requires_an_absolute_path_and_is_not_deduplicated_by_the_daemon()
 {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("note.txt"), "mineablecontent").expect("write a file");
    let path = dir.path().to_str().expect("a UTF-8 path");

    let first = call(&session, "memcastle_mine", json!({ "path": path }))
        .await
        .ok();
    // Background is the lowest priority, so mining never delays a checkpoint.
    assert_eq!(first["priority"], 0, "{first}");
    assert_eq!(first["kind"]["type"], "mine", "{first}");

    // The daemon has no scheduler and no memory of earlier requests: "once a day" and "never twice for one event"
    // are the integration's to keep, so asking twice is two jobs.
    let second = call(&session, "memcastle_mine", json!({ "path": path }))
        .await
        .ok();
    assert_ne!(first["id"], second["id"], "the daemon does not deduplicate");

    // A relative path would be resolved against the daemon's working directory, so it is refused up front.
    assert_eq!(
        call(
            &session,
            "memcastle_mine",
            json!({ "path": "relative/dir" })
        )
        .await
        .error_code(),
        "memcastle::input::invalid"
    );

    for job in [&first, &second] {
        wait_for_job(&session, job["id"].as_str().unwrap(), "completed").await;
    }

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}

#[tokio::test]
async fn each_failure_class_is_distinguishable_with_its_code_and_help() {
    let classes = fixture("failure-classes.json");
    let expected = |id: &str| -> Value {
        classes["classes"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|class| class["id"] == id)
            .unwrap_or_else(|| panic!("the fixture has no `{id}` class"))
            .clone()
    };

    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;

    // mode_rejected
    set_mode(&session, "read_only").await;
    assert_eq!(
        call(
            &session,
            "memcastle_checkpoint",
            json!({ "payload": checkpoint_of("refused") }),
        )
        .await
        .error_code(),
        expected("mode_rejected")["code"].as_str().unwrap()
    );

    // invalid_input
    let invalid = connect(&daemon.base_url).await;
    assert_eq!(
        call(
            &invalid,
            "memcastle_checkpoint",
            json!({ "payload": { "items": [] } }),
        )
        .await
        .error_code(),
        expected("invalid_input")["code"].as_str().unwrap()
    );

    // job_failed: the job exists and was accepted, and the failure arrives later, on the job, with its reason.
    let failing = call(
        &invalid,
        "memcastle_mine",
        json!({ "path": "/nonexistent/does-not-exist" }),
    )
    .await
    .ok();
    let failed = wait_for_job(&invalid, failing["id"].as_str().unwrap(), "failed").await;
    assert!(
        failed["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "a failed job must carry its reason in `error`: {failed}"
    );
    // ...and is retried through the same session, not by leaving MCP.
    call(
        &invalid,
        "memcastle_job_retry",
        json!({ "id": failing["id"] }),
    )
    .await
    .ok();

    session.cancel().await.expect("close session");
    invalid.cancel().await.expect("close session");
    daemon.shutdown().await;

    // daemon_unavailable: nothing answers, so there is no body to read, and the failure is the connection itself.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let address = closed.local_addr().expect("a local address");
    drop(closed);
    let error = reqwest::get(format!("http://{address}/api/health"))
        .await
        .expect_err("nothing is listening");
    assert!(error.is_connect(), "{error}");
    assert_eq!(expected("daemon_unavailable")["detected_by"], "transport");

    // unauthorized
    const SECRET: &str = "mc_a_shared_secret_for_the_conformance_suite_0123";
    let guarded = TestDaemon::start_configured(|config| {
        config.auth.enabled = true;
        config.auth.token = Some(Secret::new(SECRET));
    })
    .await;
    let response = reqwest::get(format!("{}/api/status", guarded.base_url))
        .await
        .expect("request");
    let unauthorized = expected("unauthorized");
    assert_eq!(
        response.status().as_u16(),
        u16::try_from(unauthorized["http_status"].as_u64().unwrap()).unwrap()
    );
    let body: Value = response.json().await.expect("an error body");
    assert_eq!(body["code"], unauthorized["code"]);
    assert!(body["help"].is_string(), "{body}");
    guarded.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn audit_is_read_only_and_a_dry_run_repair_is_allowed_in_read_only_but_applying_is_not() {
    let daemon = TestDaemon::start().await;
    let session = connect(&daemon.base_url).await;
    checkpoint_and_wait(&session, checkpoint_of("auditedtoken something to audit")).await;
    let drawers_before =
        call(&session, "memcastle_status", json!({})).await.ok()["drawer_count"].clone();

    // The whole audit/repair conversation happens in a read-only session: looking is allowed, changing is not.
    set_mode(&session, "read_only").await;

    let audit = call(&session, "memcastle_audit", json!({})).await.ok();
    let audit = wait_for_job(&session, audit["id"].as_str().unwrap(), "completed").await;
    let report = &audit["result"];
    assert_eq!(report["orphan_drawers"], json!([]), "{audit}");
    assert_eq!(report["total_drawers_in_scope"], drawers_before, "{audit}");

    let dry_run = call(
        &session,
        "memcastle_repair",
        json!({ "dry_run": true, "based_on_job": audit["id"] }),
    )
    .await
    .ok();
    let dry_run = wait_for_job(&session, dry_run["id"].as_str().unwrap(), "completed").await;
    assert_eq!(dry_run["result"]["dry_run"], true, "{dry_run}");
    assert_eq!(dry_run["result"]["actions"], json!([]), "{dry_run}");

    // Applying is a write, so the same session is refused until the user chooses otherwise.
    assert_eq!(
        call(
            &session,
            "memcastle_repair",
            json!({ "dry_run": false, "based_on_job": audit["id"] }),
        )
        .await
        .error_code(),
        "memcastle::app::mode_forbidden"
    );
    // A repair is a dry run unless it says otherwise, so forgetting the flag can never be destructive.
    let default = call(&session, "memcastle_repair", json!({})).await.ok();
    assert_eq!(default["kind"]["dry_run"], true, "{default}");

    assert_eq!(
        call(&session, "memcastle_status", json!({})).await.ok()["drawer_count"],
        drawers_before,
        "looking must not change the palace"
    );

    session.cancel().await.expect("close session");
    daemon.shutdown().await;
}
