//! The CLI's HTTP client for a running daemon.
//!
//! Every CLI command except `serve`/`daemon` and `migrate` is a thin wrapper
//! over this (`restart` adds only daemon process management) — it never
//! touches `store` or `jobs` directly (same rule as `api`/`mcp`; see
//! `app`'s doc comment), which is what guarantees the CLI can only ever do
//! what a web dashboard calling the same API could also do.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

pub mod status;

pub use status::StatusView;

use crate::app::{JobControlResult, StatusReport, WakeUpBudget, WakeUpContext};
use crate::domain::channel::CLI as CHANNEL;
use crate::domain::{CheckpointPayload, Drawer, Job, JobId, JobStatus, MemoryMode};
use crate::error::{Error, Result};
use crate::search::SearchHit;
use crate::server::lifecycle;

/// How long a connection attempt may take. A daemon on this machine answers
/// in microseconds, so a longer wait means nothing is listening (or a firewall
/// is swallowing the packets) and should read as "not running", not a hang.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long any one request may take. Every daemon call returns promptly
/// (jobs run in the background and are polled), so this only ever fires on a
/// daemon that is wedged; without it the CLI would hang on it forever, and
/// [`Error::Client`]'s "a timeout" would be a failure that could not happen.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Build the one HTTP client every call goes through, optionally stamping the
/// `X-MemCastle-Mode` header on each request.
fn http_client(mode: Option<MemoryMode>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    if let Some(mode) = mode {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static(MemoryMode::HEADER),
            reqwest::header::HeaderValue::from_static(mode.as_str()),
        );
        builder = builder.default_headers(headers);
    }
    builder
        .build()
        // Only fails if the TLS backend cannot initialise, which
        // `reqwest::Client::new()` would have panicked on just the same.
        .expect("build the HTTP client")
}

/// The address to dial for a daemon listening on `bind_addr`.
///
/// A daemon bound to a wildcard address (`0.0.0.0`, `::`) listens on every
/// interface, but the wildcard itself is not something a client can connect
/// to on every platform; loopback always reaches it. Anything that does not
/// parse as a socket address is passed through untouched.
fn connectable(bind_addr: &str) -> String {
    match bind_addr.parse::<SocketAddr>() {
        Ok(mut addr) if addr.ip().is_unspecified() => {
            addr.set_ip(if addr.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
            addr.to_string()
        }
        _ => bind_addr.to_string(),
    }
}

/// A client for one running daemon, discovered via the registry file for
/// `palace_path` (falling back to the configured bind address if no live
/// registry entry exists — see `server::lifecycle`'s doc comment on why
/// that file is a hint, not a guarantee).
pub struct DaemonClient {
    base_url: String,
    source: EndpointSource,
    http: reqwest::Client,
}

/// Where a [`DaemonClient`] got its address, so `status` can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointSource {
    /// A live daemon's registry file — how a daemon started with `--port` is found.
    Registry,
    /// The configured `server.bind` and `server.port`.
    Config,
}

impl DaemonClient {
    /// Resolve the daemon's address for `palace_path` and build a client
    /// for it. Does not itself check that anything is listening — that's
    /// what [`Self::health`] is for.
    #[must_use]
    pub fn discover(palace_path: &Path, configured_bind: SocketAddr) -> Self {
        let (bind_addr, source) = match lifecycle::read_if_live(palace_path) {
            Some(info) => (info.bind_addr, EndpointSource::Registry),
            None => (configured_bind.to_string(), EndpointSource::Config),
        };
        Self {
            base_url: format!("http://{}", connectable(&bind_addr)),
            source,
            http: http_client(None),
        }
    }

    /// The base URL this client dials, e.g. `http://127.0.0.1:8420`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether the address came from the registry file or the configuration.
    #[must_use]
    pub fn endpoint_source(&self) -> EndpointSource {
        self.source
    }

    /// Send `mode` as the `X-MemCastle-Mode` header on every request, so the
    /// daemon gates this client's calls exactly as it would any other
    /// session in that mode. Without it a client runs as `Full`.
    #[must_use]
    pub fn with_mode(mut self, mode: MemoryMode) -> Self {
        self.http = http_client(Some(mode));
        self
    }

    async fn send<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T> {
        let response = request.send().await.map_err(|source| {
            if source.is_connect() {
                Error::DaemonNotRunning
            } else {
                Error::from(source)
            }
        })?;
        if !response.status().is_success() {
            let status = response.status();
            // A body that isn't our JSON (an old daemon, a proxy in the way)
            // degrades to the bare status line rather than failing to parse.
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let text = |key: &str| {
                body.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            return Err(Error::remote(
                status.as_u16(),
                text("code").as_deref(),
                text("error").unwrap_or_else(|| status.to_string()),
                text("help"),
            ));
        }
        response.json().await.map_err(Error::from)
    }

    /// Whether the daemon responds to a health check.
    #[must_use]
    pub async fn health(&self) -> bool {
        self.http
            .get(format!("{}/api/health", self.base_url))
            .send()
            .await
            .is_ok_and(|resp| resp.status().is_success())
    }

    /// Fetch daemon status.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn status(&self) -> Result<StatusReport> {
        self.send(self.http.get(format!("{}/api/status", self.base_url)))
            .await
    }

    /// Lexical search over drawer content, optionally scoped to one wing
    /// and/or room by name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn search(
        &self,
        query: &str,
        wing: Option<&str>,
        room: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        let mut request = self
            .http
            .get(format!("{}/api/search", self.base_url))
            .query(&[("q", query), ("limit", &limit.to_string())]);
        // Only append when set, same reasoning as `list_jobs` below: an
        // absent query param, not an empty-string one, is what the server
        // side treats as "no filter".
        if let Some(wing) = wing {
            request = request.query(&[("wing", wing)]);
        }
        if let Some(room) = room {
            request = request.query(&[("room", room)]);
        }
        self.send(request).await
    }

    /// Retrieve palace content matching `query`, returned verbatim — the
    /// recall-oriented counterpart to `search` — see `AppServices::recall`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn recall(
        &self,
        query: &str,
        wing: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SearchHit>> {
        let mut request = self
            .http
            .get(format!("{}/api/recall", self.base_url))
            .query(&[("q", query), ("limit", &limit.to_string())]);
        // Only append when set — same reasoning as `search`'s identical
        // pattern: an absent query param, not an empty-string one, is what
        // the server side treats as "no filter".
        if let Some(wing) = wing {
            request = request.query(&[("wing", wing)]);
        }
        self.send(request).await
    }

    /// Build `agent_identity`'s session-start context — see
    /// `AppServices::wake_up`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn wake_up(
        &self,
        agent_identity: &str,
        wing: Option<&str>,
        budget: WakeUpBudget,
    ) -> Result<WakeUpContext> {
        let mut request = self
            .http
            .get(format!("{}/api/wake-up", self.base_url))
            .query(&[("agent_identity", agent_identity)])
            .query(&[
                ("max_items", budget.max_items.to_string()),
                ("max_bytes", budget.max_bytes.to_string()),
            ]);
        if let Some(wing) = wing {
            request = request.query(&[("wing", wing)]);
        }
        self.send(request).await
    }

    /// Write a diary entry for `agent_identity`, filed under `wing`'s fixed
    /// `"diary"` room — see `AppServices::diary_write`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn diary_write(
        &self,
        agent_identity: &str,
        wing: &str,
        content: String,
    ) -> Result<Drawer> {
        self.send(
            self.http
                .post(format!("{}/api/diary", self.base_url))
                .json(&json!({
                    "agent_identity": agent_identity,
                    "wing": wing,
                    "content": content,
                    "requested_by": CHANNEL,
                })),
        )
        .await
    }

    /// Read back `agent_identity`'s most recent diary entries in `wing`,
    /// newest first — see `AppServices::diary_read`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn diary_read(
        &self,
        agent_identity: &str,
        wing: &str,
        limit: u32,
    ) -> Result<Vec<Drawer>> {
        self.send(
            self.http
                .get(format!("{}/api/diary", self.base_url))
                .query(&[
                    ("agent_identity", agent_identity),
                    ("wing", wing),
                    ("limit", &limit.to_string()),
                ]),
        )
        .await
    }

    /// Submit a mining job.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_mine(&self, path: std::path::PathBuf, wing: Option<String>) -> Result<Job> {
        self.send(
            self.http.post(format!("{}/api/jobs", self.base_url)).json(
                &json!({ "type": "mine", "path": path, "wing": wing, "requested_by": CHANNEL }),
            ),
        )
        .await
    }

    /// Submit a read-only palace consistency audit — see
    /// `AppServices::submit_audit`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_audit(&self, scope: Option<String>) -> Result<Job> {
        self.send(
            self.http
                .post(format!("{}/api/jobs", self.base_url))
                .json(&json!({ "type": "audit", "scope": scope, "requested_by": CHANNEL })),
        )
        .await
    }

    /// Submit a repair job — see `AppServices::submit_repair` and
    /// `memcastle::repair`'s module doc.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_repair(&self, dry_run: bool, based_on_job: Option<JobId>) -> Result<Job> {
        self.send(self.http.post(format!("{}/api/jobs", self.base_url)).json(
            &json!({ "type": "repair", "dry_run": dry_run, "based_on_job": based_on_job, "requested_by": CHANNEL }),
        ))
        .await
    }

    /// Submit a synthetic demo job (see `domain::job::JobKind::Demo`) —
    /// useful for exercising the scheduler end-to-end without a real
    /// directory to mine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn demo(&self, steps: u32) -> Result<Job> {
        self.send(
            self.http
                .post(format!("{}/api/jobs", self.base_url))
                .json(&json!({ "type": "demo", "steps": steps, "requested_by": CHANNEL })),
        )
        .await
    }

    /// Submit a checkpoint job. `emergency` selects `Priority::Critical`
    /// instead of the default `Priority::High` — see
    /// `AppServices::emergency_checkpoint`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn checkpoint(&self, payload: CheckpointPayload, emergency: bool) -> Result<Job> {
        self.send(
            self.http.post(format!("{}/api/jobs", self.base_url)).json(
                &json!({ "type": "checkpoint", "payload": payload, "requested_by": CHANNEL, "emergency": emergency }),
            ),
        )
        .await
    }

    /// List jobs, optionally filtered to one status.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn list_jobs(&self, status: Option<JobStatus>) -> Result<Vec<Job>> {
        let mut request = self.http.get(format!("{}/api/jobs", self.base_url));
        if let Some(status) = status {
            request = request.query(&[("status", status.as_str())]);
        }
        self.send(request).await
    }

    /// Fetch one job by id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Remote`] (status 404, with the daemon's not-found
    /// diagnostic code) if the daemon has no such job.
    pub async fn get_job(&self, id: JobId) -> Result<Job> {
        self.send(self.http.get(format!("{}/api/jobs/{id}", self.base_url)))
            .await
    }

    /// Request that a running job pause.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails; on success, the daemon's own
    /// answer ([`crate::app::JobControlStatus::PauseRequested`]), so every caller reports it
    /// in the daemon's words instead of inventing its own.
    pub async fn pause_job(&self, id: JobId) -> Result<JobControlResult> {
        self.send(
            self.http
                .post(format!("{}/api/jobs/{id}/pause", self.base_url)),
        )
        .await
    }

    /// Resume a paused job.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn resume_job(&self, id: JobId) -> Result<JobControlResult> {
        self.send(
            self.http
                .post(format!("{}/api/jobs/{id}/resume", self.base_url)),
        )
        .await
    }

    /// Cancel a job.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn cancel_job(&self, id: JobId) -> Result<JobControlResult> {
        self.send(
            self.http
                .post(format!("{}/api/jobs/{id}/cancel", self.base_url)),
        )
        .await
    }

    /// Retry a failed job.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn retry_job(&self, id: JobId) -> Result<JobControlResult> {
        self.send(
            self.http
                .post(format!("{}/api/jobs/{id}/retry", self.base_url)),
        )
        .await
    }

    /// Ask the daemon to shut down gracefully.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn shutdown(&self) -> Result<serde_json::Value> {
        self.send(self.http.post(format!("{}/api/shutdown", self.base_url)))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::connectable;

    #[test]
    fn a_wildcard_listener_is_dialled_through_loopback() {
        assert_eq!(connectable("0.0.0.0:8420"), "127.0.0.1:8420");
        assert_eq!(connectable("[::]:8420"), "[::1]:8420");
    }

    #[test]
    fn a_specific_listener_address_is_dialled_as_is() {
        assert_eq!(connectable("192.168.1.5:9000"), "192.168.1.5:9000");
        assert_eq!(connectable("127.0.0.1:8420"), "127.0.0.1:8420");
    }
}
