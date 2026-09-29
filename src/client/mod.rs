//! The CLI's HTTP client for a running daemon.
//!
//! Every CLI command except `serve`/`daemon` and `migrate` is a thin wrapper
//! over this (`restart` adds only daemon process management) — it never
//! touches `store` or `jobs` directly (same rule as `api`/`mcp`; see
//! `app`'s doc comment), which is what guarantees the CLI can only ever do
//! what a web dashboard calling the same API could also do.

use std::net::SocketAddr;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::json;

use crate::app::{StatusReport, WakeUpBudget, WakeUpContext};
use crate::domain::channel::CLI as CHANNEL;
use crate::domain::{CheckpointPayload, Drawer, Job, JobId, JobStatus, MemoryMode};
use crate::error::{Error, Result};
use crate::search::SearchHit;
use crate::server::lifecycle;

/// A client for one running daemon, discovered via the registry file for
/// `palace_path` (falling back to the configured bind address if no live
/// registry entry exists — see `server::lifecycle`'s doc comment on why
/// that file is a hint, not a guarantee).
pub struct DaemonClient {
    base_url: String,
    http: reqwest::Client,
}

impl DaemonClient {
    /// Resolve the daemon's address for `palace_path` and build a client
    /// for it. Does not itself check that anything is listening — that's
    /// what [`Self::health`] is for.
    #[must_use]
    pub fn discover(palace_path: &Path, configured_bind: SocketAddr) -> Self {
        let bind_addr = lifecycle::read_if_live(palace_path)
            .map(|info| info.bind_addr)
            .unwrap_or_else(|| configured_bind.to_string());
        Self {
            base_url: format!("http://{bind_addr}"),
            http: reqwest::Client::new(),
        }
    }

    /// Send `mode` as the `X-MemCastle-Mode` header on every request, so the
    /// daemon gates this client's calls exactly as it would any other
    /// session in that mode. Without it a client runs as `Full`.
    #[must_use]
    pub fn with_mode(mut self, mode: MemoryMode) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_static(MemoryMode::HEADER),
            reqwest::header::HeaderValue::from_static(mode.as_str()),
        );
        self.http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            // Only fails if the TLS backend cannot initialise, which
            // `reqwest::Client::new()` in `discover` would have panicked on
            // first.
            .expect("build the HTTP client");
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
    /// answer (`{"status": "pause_requested"}`), so every caller reports it
    /// in the daemon's words instead of inventing its own.
    pub async fn pause_job(&self, id: JobId) -> Result<serde_json::Value> {
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
    pub async fn resume_job(&self, id: JobId) -> Result<serde_json::Value> {
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
    pub async fn cancel_job(&self, id: JobId) -> Result<serde_json::Value> {
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
    pub async fn retry_job(&self, id: JobId) -> Result<serde_json::Value> {
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
