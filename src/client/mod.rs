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

mod palace;
pub mod palace_view;
pub mod status;
pub mod table;

pub use status::StatusView;

use crate::app::{
    DbEndpointRequest, DbEndpointStatus, GeneratedToken, JobControlResult, RevokeResult,
    StatusReport, WakeUpBudget, WakeUpContext,
};
use crate::config::Secret;
use crate::domain::channel::CLI as CHANNEL;
use crate::domain::{CheckpointPayload, Drawer, Job, JobId, JobStatus, MemoryMode, MiningSource};
use crate::error::{Error, Result};
use crate::search::{SearchHit, SearchQuery};
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
/// `X-MemCastle-Mode` and `Authorization` headers on each request.
///
/// Both headers are built here, together, from the client's stored settings:
/// `with_mode` and `with_token` each rebuild the client, and if either built
/// only its own header the other would be silently dropped.
fn http_client(mode: Option<MemoryMode>, token: Option<&Secret>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(mode) = mode {
        headers.insert(
            reqwest::header::HeaderName::from_static(MemoryMode::HEADER),
            reqwest::header::HeaderValue::from_static(mode.as_str()),
        );
    }
    // A token that cannot be a header value (a newline in it, say) is skipped
    // rather than panicking; the daemon then answers 401, which names the
    // problem, instead of the CLI crashing with the secret in a backtrace.
    if let Some(mut value) = token.and_then(|token| {
        reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.expose())).ok()
    }) {
        // Keeps the credential out of `Debug` output and HTTP/2 header compression.
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    if !headers.is_empty() {
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
    /// Kept so either `with_*` can rebuild `http` with the other's setting intact.
    mode: Option<MemoryMode>,
    token: Option<Secret>,
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
            http: http_client(None, None),
            mode: None,
            token: None,
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
        self.mode = Some(mode);
        self.http = http_client(self.mode, self.token.as_ref());
        self
    }

    /// Present `token` as `Authorization: Bearer` on every request, for a
    /// daemon that has authentication enabled. `None` leaves the client
    /// anonymous, which is what a daemon without authentication expects.
    #[must_use]
    pub fn with_token(mut self, token: Option<Secret>) -> Self {
        self.token = token;
        self.http = http_client(self.mode, self.token.as_ref());
        self
    }

    /// Generate a token on the daemon (`POST /api/auth/token`), replacing any
    /// previous one. The plaintext is in the answer and nowhere else.
    ///
    /// # Errors
    ///
    /// [`Error::Remote`] with status 401 when the daemon requires a token this
    /// client does not hold, or [`Error::DaemonNotRunning`].
    pub async fn auth_generate(&self) -> Result<GeneratedToken> {
        self.send(self.http.post(format!("{}/api/auth/token", self.base_url)))
            .await
    }

    /// Revoke the generated token (`DELETE /api/auth/token`).
    ///
    /// # Errors
    ///
    /// As for [`Self::auth_generate`].
    pub async fn auth_revoke(&self) -> Result<RevokeResult> {
        self.send(
            self.http
                .delete(format!("{}/api/auth/token", self.base_url)),
        )
        .await
    }

    /// Ask the daemon to start its database admin endpoint (`POST /api/db`).
    ///
    /// # Errors
    ///
    /// [`Error::Remote`] when the daemon refuses (an unsafe bind, already
    /// running, a remote palace, or 401), or [`Error::DaemonNotRunning`].
    pub async fn db_start(&self, request: &DbEndpointRequest) -> Result<DbEndpointStatus> {
        self.send(
            self.http
                .post(format!("{}/api/db", self.base_url))
                .json(request),
        )
        .await
    }

    /// Ask the daemon to stop its database admin endpoint (`DELETE /api/db`).
    ///
    /// # Errors
    ///
    /// As for [`Self::db_start`].
    pub async fn db_stop(&self) -> Result<DbEndpointStatus> {
        self.send(self.http.delete(format!("{}/api/db", self.base_url)))
            .await
    }

    /// Whether the database admin endpoint is listening (`GET /api/db`).
    ///
    /// # Errors
    ///
    /// As for [`Self::db_start`].
    pub async fn db_status(&self) -> Result<DbEndpointStatus> {
        self.send(self.http.get(format!("{}/api/db", self.base_url)))
            .await
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

    /// Search drawer content with `query`: ranking mode, scope, point in time
    /// and graph expansion are all part of the [`SearchQuery`]. Sent as a JSON
    /// body (`POST /api/search`) so the request is not limited to what fits in
    /// a query string.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        self.send(
            self.http
                .post(format!("{}/api/search", self.base_url))
                .json(query),
        )
        .await
    }

    /// Retrieve palace content matching `query`, returned verbatim — the
    /// recall-oriented counterpart to `search` — see `AppServices::recall`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn recall(&self, query: &SearchQuery) -> Result<Vec<SearchHit>> {
        self.send(
            self.http
                .post(format!("{}/api/recall", self.base_url))
                .json(query),
        )
        .await
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

    /// Submit a mining job for a directory or a source adapter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_mine(
        &self,
        source: MiningSource,
        wing: Option<String>,
        full: bool,
    ) -> Result<Job> {
        // `MiningSource` serialises flat (`path`, or `provider` and `locator`), which is the wire shape the daemon
        // reads; `full` is left off when false so a plain mine sends exactly what it always did.
        let mut body = serde_json::to_value(&source)
            .map_err(|source| Error::serialization("a mining request", source))?;
        body["type"] = json!("mine");
        body["wing"] = json!(wing);
        body["requested_by"] = json!(CHANNEL);
        if full {
            body["full"] = json!(true);
        }
        self.send(
            self.http
                .post(format!("{}/api/jobs", self.base_url))
                .json(&body),
        )
        .await
    }

    /// The sources the daemon can mine and the ones it has mined.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn list_sources(&self) -> Result<crate::app::SourcesReport> {
        self.send(self.http.get(format!("{}/api/sources", self.base_url)))
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

    /// Submit an embedding sweep — see `AppServices::submit_embed`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_embed(&self, wing: Option<String>) -> Result<Job> {
        self.send(
            self.http
                .post(format!("{}/api/jobs", self.base_url))
                .json(&json!({ "type": "embed", "wing": wing, "requested_by": CHANNEL })),
        )
        .await
    }

    /// Submit an entity extraction sweep — see `AppServices::submit_extract`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_extract(&self, wing: Option<String>) -> Result<Job> {
        self.send(
            self.http
                .post(format!("{}/api/jobs", self.base_url))
                .json(&json!({ "type": "extract", "wing": wing, "requested_by": CHANNEL })),
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
    pub async fn submit_demo(&self, steps: u32) -> Result<Job> {
        self.send(
            self.http
                .post(format!("{}/api/jobs", self.base_url))
                .json(&json!({ "type": "demo", "steps": steps, "requested_by": CHANNEL })),
        )
        .await
    }

    /// Submit a checkpoint job. `emergency` selects `Priority::Critical`
    /// instead of the default `Priority::High` — see
    /// `AppServices::submit_emergency_checkpoint`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DaemonNotRunning`] if no daemon is reachable.
    pub async fn submit_checkpoint(
        &self,
        payload: CheckpointPayload,
        emergency: bool,
    ) -> Result<Job> {
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
