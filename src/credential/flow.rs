//! Signing in: the device flow and the browser flow, run by the daemon and observed by whoever asked.
//!
//! [`Credentials::begin`] starts one and returns what the user must do; the flow then runs on a task of its own, so that
//! it finishes (and the credential is kept) even if the command that started it goes away. [`Credentials::wait`] is a
//! long poll on that task, which a client repeats until it is told the outcome.
//!
//! The browser flow's redirect lands on a one-shot listener bound to the loopback address and an ephemeral port,
//! outside the HTTP API: that is what keeps the API's authentication layer, which has no public route but the health
//! check, the only door into the daemon (docs/adr/014). The listener answers one request that carries the right
//! `state` and then closes.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::domain::OAuthRequirement;
use crate::error::{Error, Result};

use super::oauth::{self, DeviceAuthorization, OAuthError, Pkce, Poll, TokenSet};
use super::{Credentials, SignedIn, locked};

/// How long the browser flow waits for the user to come back.
const BROWSER_WINDOW: Duration = Duration::from_secs(600);

/// How long one request to the callback listener may take to arrive, so that a connection that says nothing cannot hold
/// the flow.
const CALLBACK_READ: Duration = Duration::from_secs(5);

/// How many polls in a row may fail to reach the provider before the sign-in gives up.
const POLL_FAILURES_ALLOWED: u32 = 3;

type Outcome = Option<std::result::Result<SignedIn, String>>;

/// What the user must do to sign in. Carries no token: `user_code` is what the user types, and the daemon holds the
/// device code that proves it was this daemon that asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    /// Names this sign-in to `wait`.
    pub flow: String,
    /// Which flow it is.
    pub kind: FlowKind,
    /// The device flow's code, to be typed at `verification_uri`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    /// The device flow's page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri: Option<String>,
    /// A page to open: the browser flow's authorization URL, or the device flow's page with the code filled in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// How long the user has, in seconds.
    pub expires_in: u64,
}

/// Which OAuth flow a sign-in uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowKind {
    /// The user types a code at a page on any device (RFC 8628).
    Device,
    /// The user is sent to the provider in a browser on this machine and redirected back (RFC 7636).
    Browser,
}

/// Where a sign-in stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FlowStatus {
    /// The user has not finished; ask again.
    Pending,
    /// Signed in.
    SignedIn(SignedIn),
}

/// The sign-in in progress for each source: at most one, so that starting again replaces the earlier attempt and its
/// listener.
#[derive(Default)]
pub(super) struct Flows {
    by_source: HashMap<String, Pending>,
}

struct Pending {
    id: String,
    abort: AbortHandle,
    outcome: watch::Receiver<Outcome>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        // A replaced or forgotten flow stops with it: nothing is left polling a provider or holding a port.
        self.abort.abort();
    }
}

fn failed(source: &str, message: impl Into<String>) -> Error {
    Error::CredentialFlowFailed {
        source_name: source.to_string(),
        message: message.into(),
    }
}

impl Credentials {
    /// Stop the sign-in in progress for `source`, if any.
    pub(super) fn cancel_flow(&self, source: &str) {
        locked(&self.inner.flows).by_source.remove(source);
    }

    /// Start signing `source` in, with the device flow when the source declares one and the browser flow otherwise.
    ///
    /// # Errors
    ///
    /// [`Error::CredentialFlowFailed`] when the provider cannot be reached or refuses to start one, or when the
    /// loopback listener cannot be opened.
    pub async fn begin(&self, source: &str, requirement: &OAuthRequirement) -> Result<Challenge> {
        let flow = oauth::random_token(16).map_err(|m| failed(source, m))?;
        let (sender, receiver) = watch::channel(None);
        let client = oauth::client().map_err(|e| failed(source, e.to_string()))?;
        let (challenge, task) = if requirement.supports_device() {
            let device = oauth::start_device(&client, requirement)
                .await
                .map_err(|e| failed(source, e.to_string()))?;
            let challenge = Challenge {
                flow: flow.clone(),
                kind: FlowKind::Device,
                user_code: Some(device.user_code.clone()),
                verification_uri: Some(device.verification_uri.clone()),
                url: device.verification_uri_complete.clone(),
                expires_in: device.expires_in,
            };
            let run = self.clone().run_device(
                source.to_string(),
                requirement.clone(),
                client,
                device,
                sender,
            );
            (challenge, tokio::spawn(run))
        } else {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|e| {
                failed(
                    source,
                    format!("cannot open a local port for the sign-in redirect: {e}"),
                )
            })?;
            let port = listener
                .local_addr()
                .map_err(|e| failed(source, e.to_string()))?
                .port();
            let callback_path = requirement.callback_path.as_deref().unwrap_or("/callback");
            let redirect = format!("http://127.0.0.1:{port}{callback_path}");
            let state = oauth::random_token(16).map_err(|m| failed(source, m))?;
            let pkce = Pkce::generate().map_err(|m| failed(source, m))?;
            let url = oauth::authorize_url(requirement, &redirect, &state, &pkce.challenge)
                .map_err(|m| failed(source, m))?;
            let challenge = Challenge {
                flow: flow.clone(),
                kind: FlowKind::Browser,
                user_code: None,
                verification_uri: None,
                url: Some(url),
                expires_in: BROWSER_WINDOW.as_secs(),
            };
            let run = self.clone().run_browser(
                source.to_string(),
                requirement.clone(),
                client,
                listener,
                Redirect {
                    uri: redirect,
                    path: callback_path.to_string(),
                    state,
                    verifier: pkce.verifier,
                },
                sender,
            );
            (challenge, tokio::spawn(run))
        };
        // Replaces, and so stops, an earlier attempt for the same source.
        locked(&self.inner.flows).by_source.insert(
            source.to_string(),
            Pending {
                id: flow,
                abort: task.abort_handle(),
                outcome: receiver,
            },
        );
        Ok(challenge)
    }

    /// Wait up to `window` for the sign-in `flow` of `source` to finish.
    ///
    /// # Errors
    ///
    /// [`Error::CredentialFlowFailed`] when it ended badly (declined, expired, refused by the provider), or when no such
    /// sign-in is in progress, which means it was replaced by a newer one, forgotten, or already reported.
    pub async fn wait(&self, source: &str, flow: &str, window: Duration) -> Result<FlowStatus> {
        let mut outcome = {
            let flows = locked(&self.inner.flows);
            match flows.by_source.get(source) {
                Some(pending) if pending.id == flow => pending.outcome.clone(),
                _ => {
                    return Err(failed(
                        source,
                        "no such sign-in is in progress; it may have been replaced by a newer one or already finished",
                    ));
                }
            }
        };
        let result = match tokio::time::timeout(window, outcome.wait_for(Option::is_some)).await {
            // Not finished within the window: the caller asks again.
            Err(_elapsed) => return Ok(FlowStatus::Pending),
            Ok(Ok(done)) => (*done).clone(),
            // The task went away without an answer, which only a stopped daemon or a replaced flow does.
            Ok(Err(_)) => {
                self.cancel_flow(source);
                return Err(failed(source, "the sign-in stopped without an answer"));
            }
        };
        // The answer is given once; asking again for it is asking about a sign-in that no longer exists.
        self.cancel_flow(source);
        match result {
            Some(Ok(signed_in)) => Ok(FlowStatus::SignedIn(signed_in)),
            Some(Err(message)) => Err(failed(source, message)),
            None => Ok(FlowStatus::Pending),
        }
    }

    /// Keep what a finished sign-in produced, off the async runtime: the store may be a system service.
    async fn finish(
        self,
        source: String,
        requirement: OAuthRequirement,
        tokens: TokenSet,
    ) -> std::result::Result<SignedIn, String> {
        tokio::task::spawn_blocking(move || {
            let (expires_at, stored_in, scopes) = self
                .install(&source, &requirement, &tokens, None, true)
                .map_err(|e| e.to_string())?;
            tracing::info!(source = %source, stored_in, "signed a source in");
            Ok(SignedIn {
                source,
                signed_in: true,
                expires_at: Some(expires_at),
                scopes,
                stored_in: stored_in.to_string(),
            })
        })
        .await
        .map_err(|e| format!("keeping the credential failed: {e}"))?
    }

    async fn run_device(
        self,
        source: String,
        requirement: OAuthRequirement,
        client: reqwest::Client,
        device: DeviceAuthorization,
        outcome: watch::Sender<Outcome>,
    ) {
        let result = async {
            let timing = self.inner.timing;
            let deadline = Instant::now() + Duration::from_secs(device.expires_in);
            let mut interval = Duration::from_secs(device.interval).max(timing.min_poll);
            let mut failures = 0_u32;
            let tokens = loop {
                tokio::time::sleep(interval).await;
                if Instant::now() >= deadline {
                    return Err("the code expired before the sign-in was finished".to_string());
                }
                match oauth::poll_device(&client, &requirement, &device).await {
                    Ok(Poll::Pending) => failures = 0,
                    Ok(Poll::SlowDown) => {
                        failures = 0;
                        interval += timing.slow_down_step;
                    }
                    Ok(Poll::Done(tokens)) => break tokens,
                    // A blip on the network should not cost the user the code they just typed.
                    Err(OAuthError::Unreachable(message)) => {
                        failures += 1;
                        if failures >= POLL_FAILURES_ALLOWED {
                            return Err(message);
                        }
                    }
                    Err(error) => return Err(error.to_string()),
                }
            };
            self.clone()
                .finish(source.clone(), requirement, tokens)
                .await
        }
        .await;
        // `send_replace`, since nobody may be listening yet and the answer must still be there for `wait`.
        outcome.send_replace(Some(result));
    }

    async fn run_browser(
        self,
        source: String,
        requirement: OAuthRequirement,
        client: reqwest::Client,
        listener: TcpListener,
        redirect: Redirect,
        outcome: watch::Sender<Outcome>,
    ) {
        let result = async {
            let code = tokio::time::timeout(
                BROWSER_WINDOW,
                await_callback(&listener, &redirect.state, &redirect.path),
            )
            .await
            .map_err(|_| "the sign-in was not finished in time".to_string())??;
            // The listener has done its one job; the provider is talked to without a port held open.
            drop(listener);
            let tokens = oauth::exchange_code(
                &client,
                &requirement,
                &code,
                &redirect.uri,
                &redirect.verifier,
            )
            .await
            .map_err(|e| e.to_string())?;
            self.clone()
                .finish(source.clone(), requirement, tokens)
                .await
        }
        .await;
        outcome.send_replace(Some(result));
    }
}

/// What the browser flow's callback must match.
struct Redirect {
    uri: String,
    path: String,
    state: String,
    verifier: String,
}

/// Accept connections until one carries the authorization code (or the provider's refusal) under the right `state`.
async fn await_callback(
    listener: &TcpListener,
    state: &str,
    path: &str,
) -> std::result::Result<String, String> {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        // Anything else (not ours, not valid, too slow) has been answered or dropped, and the real one may still come.
        if let Ok(Some(result)) =
            tokio::time::timeout(CALLBACK_READ, answer_callback(stream, state, path)).await
        {
            return result;
        }
    }
}

/// Answer one connection. `Some` when it was the callback, with the code or the reason there is none.
async fn answer_callback(
    mut stream: TcpStream,
    state: &str,
    path: &str,
) -> Option<std::result::Result<String, String>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while !buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() < 8192 {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&buffer);
    let mut line = head.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (line.next(), line.next());
    let parsed = (method == Some("GET"))
        .then_some(target)
        .flatten()
        .and_then(|target| reqwest::Url::parse(&format!("http://localhost{target}")).ok())
        .filter(|url| url.path() == path);
    let Some(url) = parsed else {
        respond(&mut stream, "404 Not Found", "Not found.").await;
        return None;
    };
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    // Constant time, though a local attacker could do little with the difference: it costs nothing.
    let state_matches = query
        .get("state")
        .is_some_and(|given| bool::from(given.as_bytes().ct_eq(state.as_bytes())));
    if !state_matches {
        // A stray request, or a forged one: refused, and the real redirect can still arrive.
        respond(
            &mut stream,
            "400 Bad Request",
            "This sign-in link is not the one in progress.",
        )
        .await;
        return None;
    }
    if let Some(error) = query.get("error") {
        respond(
            &mut stream,
            "200 OK",
            "The sign-in was declined. You can close this tab.",
        )
        .await;
        let detail = query
            .get("error_description")
            .map_or_else(|| error.clone(), |d| format!("{error}: {d}"));
        return Some(Err(if error == "access_denied" {
            "the sign-in was declined".to_string()
        } else {
            detail
        }));
    }
    match query.get("code").filter(|code| !code.is_empty()) {
        Some(code) => {
            respond(
                &mut stream,
                "200 OK",
                "Signed in. You can close this tab and return to the terminal.",
            )
            .await;
            Some(Ok(code.clone()))
        }
        None => {
            respond(
                &mut stream,
                "400 Bad Request",
                "The provider sent no authorization code.",
            )
            .await;
            Some(Err(
                "the provider redirected back without an authorization code".to_string(),
            ))
        }
    }
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body =
        format!("<!doctype html><meta charset=\"utf-8\"><title>MemCastle</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Best effort: the browser having gone away must not fail a sign-in that already has its code.
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}
