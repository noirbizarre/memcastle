//! The hierarchy routes (`/api/wings/...`) against a real daemon: the
//! lifecycle, the error contract, and the memory-mode gates.

use crate::common;

use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::JobStatus;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// Send `method` to `path` (optionally with a JSON body and a memory mode),
/// returning the status and the JSON body.
async fn call(
    daemon: &TestDaemon,
    method: Method,
    path: &str,
    body: Option<Value>,
    mode: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = reqwest::Client::new().request(method, format!("{}{path}", daemon.base_url));
    if let Some(body) = body {
        request = request.json(&body);
    }
    if let Some(mode) = mode {
        request = request.header("x-memcastle-mode", mode);
    }
    let response = request.send().await.expect("request");
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn get(daemon: &TestDaemon, path: &str) -> (StatusCode, Value) {
    call(daemon, Method::GET, path, None, None).await
}

async fn post(daemon: &TestDaemon, path: &str, body: Value) -> (StatusCode, Value) {
    call(daemon, Method::POST, path, Some(body), None).await
}

async fn delete(daemon: &TestDaemon, path: &str) -> (StatusCode, Value) {
    call(daemon, Method::DELETE, path, None, None).await
}

#[tokio::test]
async fn a_wing_room_and_drawer_can_be_created_listed_shown_and_deleted() {
    let daemon = TestDaemon::start().await;

    let (status, wing) = post(&daemon, "/api/wings", json!({ "name": "work" })).await;
    assert_eq!(status, StatusCode::CREATED, "{wing}");
    assert_eq!(wing["name"], "work");
    assert_eq!(wing["created"], true);

    let (status, room) = post(
        &daemon,
        "/api/wings/work/rooms",
        json!({ "name": "project-x" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{room}");
    assert_eq!(room["wing_name"], "work");

    let (status, drawer) = post(
        &daemon,
        "/api/wings/work/rooms/project-x/drawers",
        json!({ "name": "context", "content": "the plan" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{drawer}");
    assert_eq!(drawer["name"], "context");

    let (_, wings) = get(&daemon, "/api/wings").await;
    assert_eq!(wings[0]["name"], "work");
    assert_eq!(
        (wings[0]["rooms"].as_u64(), wings[0]["drawers"].as_u64()),
        (Some(1), Some(1))
    );

    let (_, detail) = get(&daemon, "/api/wings/work").await;
    assert_eq!(detail["rooms"][0]["name"], "project-x");

    let (_, rooms) = get(&daemon, "/api/wings/work/rooms").await;
    assert_eq!(rooms[0]["drawers"], 1);

    let (_, listed) = get(&daemon, "/api/wings/work/rooms/project-x/drawers").await;
    assert_eq!(listed[0]["name"], "context");
    assert_eq!(listed[0]["preview"], "the plan");

    let (_, shown) = get(&daemon, "/api/wings/work/rooms/project-x/drawers/context").await;
    assert_eq!(shown["content"], "the plan");

    let (status, deleted) = delete(&daemon, "/api/wings/work").await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(deleted, json!({ "wings": 1, "rooms": 1, "drawers": 1 }));

    let (status, body) = get(&daemon, "/api/wings/work").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "memcastle::palace::wing_not_found");

    daemon.shutdown().await;
}

#[tokio::test]
async fn creating_the_same_wing_twice_is_a_200_that_says_it_was_not_created() {
    let daemon = TestDaemon::start().await;
    post(&daemon, "/api/wings", json!({ "name": "work" })).await;

    let (status, again) = post(&daemon, "/api/wings", json!({ "name": "work" })).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["created"], false);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_drawer_is_addressed_by_its_uuid_and_by_a_name_containing_slashes() {
    let daemon = TestDaemon::start().await;
    let base = "/api/wings/w/rooms/files/drawers";
    let (_, named) = post(
        &daemon,
        base,
        json!({ "name": "src/main.rs", "content": "fn main() {}" }),
    )
    .await;
    let (_, unnamed) = post(&daemon, base, json!({ "content": "no name" })).await;

    let (status, by_name) = get(&daemon, &format!("{base}/src/main.rs")).await;
    assert_eq!(status, StatusCode::OK, "{by_name}");
    assert_eq!(by_name["id"], named["id"]);

    let id = unnamed["id"].as_str().unwrap();
    let (status, by_id) = get(&daemon, &format!("{base}/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{by_id}");
    assert_eq!(by_id["content"], "no name");

    let (status, _) = delete(&daemon, &format!("{base}/src/main.rs")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = get(&daemon, &format!("{base}/src/main.rs")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "memcastle::palace::drawer_not_found");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_drawer_name_holding_other_content_is_a_409_and_the_same_content_is_a_no_op() {
    let daemon = TestDaemon::start().await;
    let path = "/api/wings/w/rooms/r/drawers";
    post(&daemon, path, json!({ "name": "n", "content": "one" })).await;

    let (status, same) = post(&daemon, path, json!({ "name": "n", "content": "one" })).await;
    assert_eq!((status, &same["created"]), (StatusCode::OK, &json!(false)));

    let (status, clash) = post(&daemon, path, json!({ "name": "n", "content": "two" })).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(clash["code"], "memcastle::palace::drawer_name_taken");
    assert!(clash["help"].is_string());
    daemon.shutdown().await;
}

#[tokio::test]
async fn unknown_records_and_unusable_names_get_their_own_status_and_code() {
    let daemon = TestDaemon::start().await;
    post(&daemon, "/api/wings", json!({ "name": "work" })).await;

    let (status, body) = get(&daemon, "/api/wings/work/rooms/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "memcastle::palace::room_not_found");

    let (status, body) = post(&daemon, "/api/wings", json!({ "name": "a/b" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "memcastle::palace::invalid_path");

    let (status, body) = post(
        &daemon,
        "/api/wings/work/rooms/r/drawers",
        json!({ "content": "  " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "memcastle::input::invalid");
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_wing_can_be_addressed_by_uuid_and_a_room_id_from_another_wing_is_not_found() {
    let daemon = TestDaemon::start().await;
    let (_, a) = post(&daemon, "/api/wings", json!({ "name": "a" })).await;
    let (_, room) = post(&daemon, "/api/wings/a/rooms", json!({ "name": "x" })).await;
    post(&daemon, "/api/wings", json!({ "name": "b" })).await;

    let (status, _) = get(
        &daemon,
        &format!("/api/wings/{}", a["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = get(
        &daemon,
        &format!("/api/wings/b/rooms/{}", room["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    daemon.shutdown().await;
}

#[tokio::test]
async fn deleting_a_room_leaves_its_wing_and_sibling_rooms() {
    let daemon = TestDaemon::start().await;
    post(
        &daemon,
        "/api/wings/w/rooms/a/drawers",
        json!({ "content": "1" }),
    )
    .await;
    post(
        &daemon,
        "/api/wings/w/rooms/b/drawers",
        json!({ "content": "2" }),
    )
    .await;

    let (status, deleted) = delete(&daemon, "/api/wings/w/rooms/a").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted, json!({ "wings": 0, "rooms": 1, "drawers": 1 }));
    let (_, detail) = get(&daemon, "/api/wings/w").await;
    assert_eq!(detail["wing"]["rooms"], 1);
    assert_eq!(detail["wing"]["drawers"], 1);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_delete_is_refused_with_a_409_while_a_mining_job_is_pending() {
    let daemon = TestDaemon::start().await;
    post(
        &daemon,
        "/api/wings/work/rooms/r/drawers",
        json!({ "content": "keep me" }),
    )
    .await;
    // A demo job that never finishes in time to matter keeps the scheduler
    // busy, so the mine queued behind it is still pending when we delete.
    let (_, blocker) = post(
        &daemon,
        "/api/jobs",
        json!({ "type": "demo", "steps": 500 }),
    )
    .await;
    let scratch = tempfile::tempdir().unwrap();
    std::fs::write(scratch.path().join("f.txt"), "x").unwrap();
    let (status, mine) = post(
        &daemon,
        "/api/jobs",
        json!({ "type": "mine", "path": scratch.path(), "wing": "other" }),
    )
    .await;
    assert!(status.is_success(), "{mine}");

    let (status, body) = delete(&daemon, "/api/wings/work").await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "memcastle::palace::busy");
    let (status, _) = get(&daemon, "/api/wings/work").await;
    assert_eq!(status, StatusCode::OK, "nothing was deleted");

    // Cancel both; once nothing is pending the same delete goes through.
    // The queued mine goes first: with a concurrency of one, cancelling the
    // blocker frees the worker, and a slow runner (macOS CI) can then start and
    // complete the one-file mine before its own cancel arrives, so it would end
    // `Completed` and never reach `Cancelled`.
    for job in [&mine, &blocker] {
        let id = job["id"].as_str().unwrap();
        post(&daemon, &format!("/api/jobs/{id}/cancel"), json!({})).await;
    }
    for job in [&blocker, &mine] {
        let id: memcastle::domain::JobId = job["id"].as_str().unwrap().parse().unwrap();
        wait_for_job_status(
            &reqwest::Client::new(),
            &daemon.base_url,
            id,
            JobStatus::Cancelled,
        )
        .await;
    }
    let (status, _) = delete(&daemon, "/api/wings/work").await;
    assert_eq!(status, StatusCode::OK);
    daemon.shutdown().await;
}

#[tokio::test]
async fn read_only_may_look_but_not_change_and_disabled_may_do_neither() {
    let daemon = TestDaemon::start().await;
    post(
        &daemon,
        "/api/wings/w/rooms/r/drawers",
        json!({ "name": "n", "content": "x" }),
    )
    .await;

    for path in [
        "/api/wings",
        "/api/wings/w",
        "/api/wings/w/rooms/r/drawers/n",
    ] {
        let (status, _) = call(&daemon, Method::GET, path, None, Some("read_only")).await;
        assert_eq!(status, StatusCode::OK, "read_only GET {path}");
        let (status, body) = call(&daemon, Method::GET, path, None, Some("disabled")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "disabled GET {path}");
        assert_eq!(body["code"], "memcastle::app::mode_forbidden");
    }

    for (method, path, body) in [
        (Method::POST, "/api/wings", Some(json!({ "name": "z" }))),
        (Method::DELETE, "/api/wings/w", None),
        (Method::DELETE, "/api/wings/w/rooms/r", None),
        (Method::DELETE, "/api/wings/w/rooms/r/drawers/n", None),
    ] {
        let (status, _) = call(&daemon, method.clone(), path, body, Some("read_only")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "read_only {method} {path}");
    }
    let (status, _) = get(&daemon, "/api/wings/w/rooms/r/drawers/n").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "nothing was deleted by the refused calls"
    );
    daemon.shutdown().await;
}
