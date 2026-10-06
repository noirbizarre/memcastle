//! A fake OAuth provider for tests: the three endpoints of a public client, with a knob for every way a real one
//! misbehaves, and a record of what it was asked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Form, Json, Router};
use serde_json::json;

use crate::domain::OAuthRequirement;

/// How the provider behaves right now. Changed between steps of a test.
pub(crate) struct Behaviour {
    /// What `expires_in` says on the next token it issues.
    pub expires_in: u64,
    /// Whether a refresh issues a new refresh token.
    pub rotate: bool,
    /// Whether any refresh token is issued at all.
    pub issue_refresh: bool,
    /// Whether a refresh is answered `invalid_grant`.
    pub revoke: bool,
    /// Whether the token endpoint answers 503.
    pub outage: bool,
    /// How many device polls answer `authorization_pending` first.
    pub pending_polls: usize,
    /// Whether the next device poll answers `slow_down` (once).
    pub slow_down_once: bool,
    /// Whether the user declines the device sign-in.
    pub deny: bool,
    /// `expires_in` of the device code.
    pub device_expires_in: u64,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            expires_in: 3600,
            rotate: false,
            issue_refresh: true,
            revoke: false,
            outage: false,
            pending_polls: 0,
            slow_down_once: false,
            deny: false,
            device_expires_in: 30,
        }
    }
}

pub(crate) struct Provider {
    pub base: String,
    pub behaviour: Mutex<Behaviour>,
    /// Every form the token endpoint received.
    pub seen: Mutex<Vec<HashMap<String, String>>>,
    pub refresh_calls: AtomicUsize,
    issued: AtomicUsize,
}

impl Provider {
    pub fn set(&self, change: impl FnOnce(&mut Behaviour)) {
        change(&mut self.behaviour.lock().unwrap());
    }

    pub fn refreshes(&self) -> usize {
        self.refresh_calls.load(Ordering::SeqCst)
    }

    /// The refresh tokens the provider was asked to renew, in order.
    pub fn refresh_tokens_seen(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|form| form.get("grant_type").map(String::as_str) == Some("refresh_token"))
            .filter_map(|form| form.get("refresh_token").cloned())
            .collect()
    }

    /// A requirement naming this provider's endpoints, with both flows available.
    pub fn requirement(&self) -> OAuthRequirement {
        OAuthRequirement {
            client_id: "test-client".into(),
            scopes: vec!["read".into()],
            token_url: format!("{}/token", self.base),
            authorize_url: Some(format!("{}/authorize", self.base)),
            device_authorization_url: Some(format!("{}/device", self.base)),
        }
    }

    /// As [`Provider::requirement`], with only the browser flow.
    pub fn browser_requirement(&self) -> OAuthRequirement {
        OAuthRequirement {
            device_authorization_url: None,
            ..self.requirement()
        }
    }
}

fn oauth_error(code: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": code, "error_description": format!("fake {code}")})),
    )
}

async fn device(State(provider): State<Arc<Provider>>) -> impl IntoResponse {
    let expires_in = provider.behaviour.lock().unwrap().device_expires_in;
    Json(json!({
        "device_code": "device-secret",
        "user_code": "ABCD-1234",
        "verification_uri": format!("{}/activate", provider.base),
        "expires_in": expires_in,
        "interval": 0,
    }))
}

async fn token(
    State(provider): State<Arc<Provider>>,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    provider.seen.lock().unwrap().push(form.clone());
    let mut behaviour = provider.behaviour.lock().unwrap();
    if behaviour.outage {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let grant = form.get("grant_type").map(String::as_str).unwrap_or("");
    match grant {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            if behaviour.deny {
                return oauth_error("access_denied").into_response();
            }
            if behaviour.slow_down_once {
                behaviour.slow_down_once = false;
                return oauth_error("slow_down").into_response();
            }
            if behaviour.pending_polls > 0 {
                behaviour.pending_polls -= 1;
                return oauth_error("authorization_pending").into_response();
            }
        }
        "authorization_code" => {
            if form.get("code").map(String::as_str) != Some("good-code") {
                return oauth_error("invalid_grant").into_response();
            }
        }
        "refresh_token" => {
            provider.refresh_calls.fetch_add(1, Ordering::SeqCst);
            if behaviour.revoke {
                return oauth_error("invalid_grant").into_response();
            }
        }
        _ => return oauth_error("unsupported_grant_type").into_response(),
    }
    let n = provider.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let mut answer = json!({
        "access_token": format!("access-{n}"),
        "token_type": "Bearer",
        "expires_in": behaviour.expires_in,
        "scope": "read",
    });
    // A first sign-in always gets a refresh token when the provider issues them; a refresh only when it rotates.
    if behaviour.issue_refresh && (grant != "refresh_token" || behaviour.rotate) {
        answer["refresh_token"] = json!(format!("refresh-{n}"));
    }
    Json(answer).into_response()
}

/// Start a provider on a loopback port.
pub(crate) async fn start() -> Arc<Provider> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let provider = Arc::new(Provider {
        base,
        behaviour: Mutex::new(Behaviour::default()),
        seen: Mutex::default(),
        refresh_calls: AtomicUsize::new(0),
        issued: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/device", post(device))
        .route("/token", post(token))
        .with_state(Arc::clone(&provider));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    provider
}
