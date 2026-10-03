//! What the integration conformance suite needs from an MCP client, written once: opening a session, calling a tool
//! without panicking on a tool-level error, waiting for a job, and loading the language-neutral fixtures in
//! `tests/fixtures/integration/`.
//!
//! Older test files keep their own private copies of `connect`/`call`; this module is for new code, and folding
//! them in is a separate change.

use std::path::PathBuf;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService, serve_client};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransport;
use serde_json::Value;

/// One MCP session: its own `initialize` handshake, and therefore its own `mcp-session-id`.
pub type Session = RunningService<RoleClient, ClientConfig>;

/// How long any single step may take before the test calls it hung rather than slow.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// What a tool call answered. A tool-level failure is a successful JSON-RPC round trip whose result is flagged as
/// an error, which is exactly how an integration sees it, so it is data here and not a panic.
pub struct ToolOutcome {
    /// Whether the daemon flagged the result as an error.
    pub is_error: bool,
    /// The concatenated text content, the JSON body in both the success and the error case.
    pub text: String,
}

impl ToolOutcome {
    /// The body as JSON; a body that is not JSON is a bug in the daemon's contract, so it fails the test.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text)
            .unwrap_or_else(|error| panic!("a tool answered non-JSON ({error}): {}", self.text))
    }

    /// The success body, asserting that the call was not rejected.
    pub fn ok(&self) -> Value {
        assert!(!self.is_error, "the call was rejected: {}", self.text);
        self.json()
    }

    /// The error body's diagnostic `code`, asserting that the call *was* rejected and carries the `help` the
    /// contract promises every integration it can show to a user.
    pub fn error_code(&self) -> String {
        assert!(self.is_error, "the call was accepted: {}", self.text);
        let body = self.json();
        assert!(
            body["help"].as_str().is_some_and(|help| !help.is_empty()),
            "an error must tell the user what to do (`help`): {body}"
        );
        body["code"]
            .as_str()
            .unwrap_or_else(|| panic!("an error must carry a diagnostic `code`: {body}"))
            .to_string()
    }
}

/// Open a fresh MCP session against `base_url`.
pub async fn connect(base_url: &str) -> Session {
    let transport = StreamableHttpClientTransport::from_uri(format!("{base_url}/mcp"));
    tokio::time::timeout(
        STEP_TIMEOUT,
        serve_client(ClientConfig::default(), transport),
    )
    .await
    .expect("an mcp session initializes in time")
    .expect("an mcp session initializes")
}

/// Call `tool` with `arguments` (a JSON object) and report what it answered.
pub async fn call(session: &Session, tool: &str, arguments: Value) -> ToolOutcome {
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("tool arguments are a JSON object");
    let result = tokio::time::timeout(
        STEP_TIMEOUT,
        session
            .peer()
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments)),
    )
    .await
    .unwrap_or_else(|_| panic!("{tool} did not answer within {STEP_TIMEOUT:?}"))
    .unwrap_or_else(|error| panic!("{tool} failed at the protocol level: {error}"));

    let text = serde_json::to_value(&result.content)
        .expect("content serializes")
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ToolOutcome {
        is_error: result.is_error.unwrap_or(false),
        text,
    }
}

/// Select `mode` (a wire value) for `session`, asserting that the daemon accepted it.
pub async fn set_mode(session: &Session, mode: &str) {
    call(
        session,
        "memcastle_set_mode",
        serde_json::json!({ "mode": mode }),
    )
    .await
    .ok();
}

/// The mode the daemon reports for `session`, which is the only way an integration can check what it was given.
pub async fn reported_mode(session: &Session) -> String {
    call(session, "memcastle_status", serde_json::json!({}))
        .await
        .ok()["mode"]
        .as_str()
        .expect("status reports a mode")
        .to_string()
}

/// Poll `memcastle_job_get` until job `id` reaches `status`, and return the job. Waiting for a status other than
/// the one a job ends in is a failure at once, not a thirty-second timeout.
pub async fn wait_for_job(session: &Session, id: &str, status: &str) -> Value {
    for _ in 0..600 {
        let job = call(
            session,
            "memcastle_job_get",
            serde_json::json!({ "id": id }),
        )
        .await
        .ok();
        let current = job["status"].as_str().unwrap_or_default();
        if current == status {
            return job;
        }
        assert!(
            !matches!(current, "completed" | "failed" | "cancelled"),
            "job {id} ended as `{current}` while the test waited for `{status}`: {job}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {id} did not reach `{status}` within 30s");
}

/// The `content` of every hit or drawer in a tool's JSON array result.
pub fn contents(result: &Value) -> Vec<String> {
    result
        .as_array()
        .expect("the result is a list")
        .iter()
        .filter_map(|item| item["content"].as_str().map(str::to_owned))
        .collect()
}

/// Load `tests/fixtures/integration/<name>`, which every Pi, OpenCode or later suite reads as the same file.
pub fn fixture(name: &str) -> Value {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "integration",
        name,
    ]
    .iter()
    .collect();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("fixture {} is not valid JSON: {error}", path.display()))
}

/// Replace every string equal to `{{name}}` in `value` with `replacement`, so a fixture can name a path that only
/// exists while the test runs.
pub fn substitute(value: &mut Value, name: &str, replacement: &str) {
    match value {
        Value::String(text) if text == &format!("{{{{{name}}}}}") => {
            *text = replacement.to_owned();
        }
        Value::Array(items) => {
            for item in items {
                substitute(item, name, replacement);
            }
        }
        Value::Object(fields) => {
            for field in fields.values_mut() {
                substitute(field, name, replacement);
            }
        }
        _ => {}
    }
}
