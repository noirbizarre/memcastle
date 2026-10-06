//! `GET /api/events`, the server-sent change stream, against a real daemon and real HTTP
//! (`docs/adr/041-server-sent-events-for-dashboard-updates.md`).
//!
//! What the events hold is unit tested in `src/events/mod.rs`, and what the dashboard does with them in `web/test`;
//! this holds the wire: delivery, the memory-mode gate, authentication, and that an open stream never keeps the
//! daemon from stopping.

use crate::common;

use std::time::Duration;

use common::TestDaemon;
use memcastle::config::Secret;
use reqwest::StatusCode;
use serde_json::{Value, json};

const SECRET: &str = "mc_a_shared_secret_for_the_tests_0123456789";

/// How long a test waits for a frame it expects, generous for a busy machine.
const PATIENCE: Duration = Duration::from_secs(20);

/// One SSE frame: its event name and the JSON it carried.
#[derive(Debug)]
struct Frame {
    name: String,
    data: Value,
}

/// An open `GET /api/events`, read one frame at a time.
struct Stream {
    response: reqwest::Response,
    buffer: String,
}

impl Stream {
    async fn open(
        daemon: &TestDaemon,
        mode: Option<&str>,
        token: Option<&str>,
    ) -> reqwest::Response {
        let mut request = reqwest::Client::new().get(format!("{}/api/events", daemon.base_url));
        if let Some(mode) = mode {
            request = request.header("x-memcastle-mode", mode);
        }
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.expect("request")
    }

    /// Open the stream and read its first frame, which says it is live: nothing published before this returns can
    /// be missed, so a test may write straight after it.
    async fn live(daemon: &TestDaemon, mode: Option<&str>, token: Option<&str>) -> Self {
        let response = Self::open(daemon, mode, token).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "text/event-stream",
            "the stream must be served as server-sent events"
        );
        let mut stream = Self {
            response,
            buffer: String::new(),
        };
        assert_eq!(stream.next().await.name, "open");
        stream
    }

    /// The next frame, skipping keep-alive comments.
    async fn next(&mut self) -> Frame {
        tokio::time::timeout(PATIENCE, async {
            loop {
                while let Some(end) = self.buffer.find("\n\n") {
                    let raw: String = self.buffer.drain(..end + 2).collect();
                    let mut name = String::new();
                    let mut data = String::new();
                    for line in raw.lines() {
                        if let Some(value) = line.strip_prefix("event:") {
                            name = value.trim().to_string();
                        } else if let Some(value) = line.strip_prefix("data:") {
                            data.push_str(value.trim());
                        }
                    }
                    if !name.is_empty() {
                        return Frame {
                            name,
                            data: serde_json::from_str(&data).expect("frame data is JSON"),
                        };
                    }
                }
                let chunk = self
                    .response
                    .chunk()
                    .await
                    .expect("read the stream")
                    .expect("the stream ended while a frame was expected");
                self.buffer.push_str(&String::from_utf8_lossy(&chunk));
            }
        })
        .await
        .expect("no frame arrived in time")
    }

    /// The next frame named `name` satisfying `accept`, skipping others.
    async fn until(&mut self, name: &str, accept: impl Fn(&Value) -> bool) -> Value {
        loop {
            let frame = self.next().await;
            if frame.name == name && accept(&frame.data) {
                return frame.data;
            }
        }
    }
}

async fn write_note(daemon: &TestDaemon, content: &str) -> Value {
    reqwest::Client::new()
        .post(format!("{}/api/notes", daemon.base_url))
        .json(
            &json!({ "wing": "keep", "room": "notes", "content": content, "requested_by": "cli" }),
        )
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json")
}

#[tokio::test]
async fn a_written_note_reaches_a_subscriber_as_an_id_and_never_as_text() {
    let daemon = TestDaemon::start().await;
    let mut stream = Stream::live(&daemon, None, None).await;

    let note = write_note(&daemon, "the secret recipe is nutmeg").await;

    let event = stream
        .until("drawer", |event| event["action"] == "created")
        .await;
    let id = note["id"].as_str().expect("the note has an id");
    assert_eq!(event["id"], id);
    assert!(
        !event.to_string().contains("nutmeg"),
        "an event names what changed and never what it holds: {event}"
    );
    drop(stream);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_new_wing_is_announced_before_its_first_drawer() {
    let daemon = TestDaemon::start().await;
    let mut stream = Stream::live(&daemon, None, None).await;

    write_note(&daemon, "anything").await;

    let first = stream.next().await;
    assert_eq!(first.name, "wing");
    assert_eq!(first.data["action"], "created");
    drop(stream);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_jobs_progress_and_final_status_are_pushed_as_it_runs() {
    let daemon = TestDaemon::start().await;
    let mut stream = Stream::live(&daemon, None, None).await;

    let job: Value = reqwest::Client::new()
        .post(format!("{}/api/jobs", daemon.base_url))
        .json(&json!({ "type": "demo", "steps": 2, "requested_by": "test" }))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    let id = job["id"].as_str().expect("the job has an id").to_string();

    let queued = stream.until("job", |event| event["id"] == id).await;
    assert_eq!(queued["action"], "created");
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["job_kind"], "demo");
    // Every later job event follows one write the job made, so the stream and the job agree on what is next.
    stream
        .until("job", |event| {
            event["id"] == id && event["status"] == "running"
        })
        .await;
    stream
        .until("job", |event| {
            event["id"] == id && event["status"] == "completed"
        })
        .await;
    drop(stream);
    daemon.shutdown().await;
}

#[tokio::test]
async fn a_disabled_session_is_refused_and_a_read_only_one_is_served() {
    let daemon = TestDaemon::start().await;

    let refused = Stream::open(&daemon, Some("disabled"), None).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let body: Value = refused.json().await.expect("json");
    assert_eq!(body["code"], "memcastle::mode::forbidden");

    let mut read_only = Stream::live(&daemon, Some("read_only"), None).await;
    write_note(&daemon, "hello").await;
    read_only.until("drawer", |_| true).await;
    drop(read_only);
    daemon.shutdown().await;
}

#[tokio::test]
async fn an_unknown_mode_is_rejected_and_not_taken_for_full() {
    let daemon = TestDaemon::start().await;

    let response = Stream::open(&daemon, Some("sideways"), None).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    daemon.shutdown().await;
}

#[tokio::test]
async fn the_stream_needs_the_token_when_authentication_is_on() {
    let daemon = TestDaemon::start_configured(|config| {
        config.auth.enabled = true;
        config.auth.token = Some(Secret::new(SECRET));
    })
    .await;

    let anonymous = Stream::open(&daemon, None, None).await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    let wrong = Stream::open(&daemon, None, Some("mc_not_the_secret")).await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let stream = Stream::live(&daemon, None, Some(SECRET)).await;
    drop(stream);
    daemon.shutdown_as(Some(SECRET)).await;
}

#[tokio::test]
async fn an_open_stream_does_not_keep_the_daemon_from_shutting_down() {
    let daemon = TestDaemon::start().await;
    let stream = Stream::live(&daemon, None, None).await;

    // `shutdown` fails the test if the daemon is still running after 15 seconds; the stream is held open throughout.
    daemon.shutdown().await;
    drop(stream);
}
