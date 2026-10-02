//! The database admin endpoint: a SurrealDB-compatible WebSocket listener over
//! the daemon's *own* database handle, so SurrealDB Studio (Surrealist) can
//! inspect and query the live palace (`docs/adr/015`).
//!
//! It is an adapter, not a second storage layer. Each connection runs its
//! requests on a clone of the handle the daemon already holds, which is a
//! separate session over the same datastore: nothing here opens a database, a
//! second process never touches the SurrealKV directory, and the schema and
//! migration ownership stay where they were. It is also not a second
//! application API: it forwards SurrealQL and does not interpret it.
//!
//! The listener is started and stopped by [`crate::app::AppServices`] on an
//! explicit request; `memcastle serve` never starts it. This module only knows
//! how to serve a listener it is handed, which keeps the decisions about *where*
//! (loopback, remote, with or without authentication) in one reviewed place.

mod connection;
mod origin;
mod wire;

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Request, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_REQUEST_HEADERS, AUTHORIZATION, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub use connection::SIGNIN_USER;
pub use origin::OriginPolicy;

use wire::{Format, PROTOCOLS};

/// The largest message accepted from a client. Far above any query a person
/// types into Studio, far below what would let one frame exhaust memory.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// How many Studio connections may be open at once. A browser opens one per
/// tab; this is an inspection tool, not a database service.
pub const MAX_CONNECTIONS: usize = 8;

/// Decides whether a presented token may use the endpoint.
///
/// Boxed because it closes over the application services, which this module
/// must not depend on. `None` is "no credential was presented".
pub type Authenticator =
    Arc<dyn Fn(Option<String>) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// How the endpoint is guarded.
#[derive(Clone)]
pub struct Options {
    /// Which browser origins may connect.
    pub origins: OriginPolicy,
    /// Checks a token, or `None` when the daemon has authentication disabled
    /// (then a client is accepted without a token and may sign in as
    /// [`SIGNIN_USER`] with that same value as the password).
    pub auth: Option<Authenticator>,
}

/// Everything a connection needs, shared across them.
struct Shared {
    /// The daemon's own handle; connections clone it.
    db: Surreal<Any>,
    auth: Option<Authenticator>,
    origins: OriginPolicy,
    /// `surrealdb-<version>`, as SurrealDB's own `version` method reports it.
    version: String,
    /// Bounds concurrent connections.
    permits: Arc<Semaphore>,
    /// Fired when the endpoint is stopped or the daemon shuts down.
    cancel: CancellationToken,
}

/// Serve the admin endpoint on `listener` until `cancel` fires.
///
/// `db` is the daemon's own handle. The listener is bound by the caller so a
/// taken port is reported before anything is spawned.
pub async fn serve(
    listener: TcpListener,
    db: Surreal<Any>,
    options: Options,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    let version = match db.version().await {
        Ok(version) => format!("surrealdb-{version}"),
        // Studio only displays it; an unreadable version must not stop the endpoint.
        Err(_) => "surrealdb-unknown".to_string(),
    };
    let shared = Arc::new(Shared {
        db,
        auth: options.auth,
        origins: options.origins,
        version,
        permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        cancel: cancel.clone(),
    });
    let router = Router::new()
        .route("/health", get(health))
        .route("/version", get(version_text))
        .route("/rpc", get(rpc))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&shared),
            origin_guard,
        ))
        .with_state(shared);
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(cancel.cancelled_owned())
    .await
}

/// Refuse a browser page that is not allowed to use the endpoint, and answer
/// the cross-origin preflight for the ones that are.
///
/// Applied to every route (the probes included): a foreign page has no business
/// learning that this endpoint exists, and Studio's own `fetch` of `/health`
/// needs the CORS headers below to be readable at all.
async fn origin_guard(State(shared): State<Arc<Shared>>, request: Request, next: Next) -> Response {
    let origin = match shared.origins.check(request.headers()) {
        Ok(origin) => origin.map(str::to_string),
        Err(refused) => {
            // The origin is attacker-chosen text, but it is also the one fact
            // that tells an operator which page tried; it is not a secret.
            warn!(origin = %refused, "database admin request from a disallowed origin refused");
            // Echoed back so a client we did not anticipate can be allowed with
            // the exact flag value; bounded because the text is client-chosen.
            let shown: String = refused.chars().take(100).collect();
            return (
                StatusCode::FORBIDDEN,
                format!(
                    "the origin `{shown}` is not allowed to use the MemCastle database admin \
                     endpoint; add it with `--allow-origin {shown}` (see docs/database-access.md)"
                ),
            )
                .into_response();
        }
    };

    let preflight = request.method() == Method::OPTIONS;
    let requested_headers = request
        .headers()
        .get(ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let mut response = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };

    if let Some(origin) = origin.and_then(|origin| HeaderValue::from_str(&origin).ok()) {
        let headers = response.headers_mut();
        // Echo the one allowed origin, never `*`: the allow-list is the point.
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(VARY, HeaderValue::from_static("Origin"));
        if preflight {
            headers.insert(
                ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("GET, OPTIONS"),
            );
            headers.insert(
                ACCESS_CONTROL_ALLOW_HEADERS,
                requested_headers.unwrap_or_else(|| HeaderValue::from_static("*")),
            );
        }
    }
    response
}

/// `GET /health`: SurrealDB's liveness probe, which Studio polls.
async fn health() -> StatusCode {
    StatusCode::OK
}

/// `GET /version`: the version string Studio shows next to the connection.
async fn version_text(State(shared): State<Arc<Shared>>) -> String {
    shared.version.clone()
}

/// `GET /rpc`: upgrade to SurrealDB's WebSocket RPC.
async fn rpc(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Taken before the upgrade so a full endpoint answers with a status, not
    // with a connection that opens and immediately closes.
    let Ok(permit) = Arc::clone(&shared.permits).try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many open database admin connections; close another Studio tab",
        )
            .into_response();
    };

    // A client that is not a browser can present the token as a header on the
    // upgrade. A browser cannot set one on a WebSocket, so it signs in with
    // the token as the password instead (see `connection`).
    let pre_authenticated = match (&shared.auth, bearer(&headers)) {
        (Some(authenticate), Some(token)) => authenticate(Some(token)).await,
        _ => false,
    };

    upgrade
        .protocols(PROTOCOLS)
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let format = Format::negotiated(socket.protocol().and_then(|p| p.to_str().ok()));
            connection::run(socket, format, shared, pre_authenticated, peer).await;
            // Held for the connection's whole life.
            drop(permit);
        })
}

/// The token of an `Authorization: Bearer` header, if there is one.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let (scheme, token) = headers.get(AUTHORIZATION)?.to_str().ok()?.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then(|| token.to_string())
}

/// Log that the endpoint is up, naming the one fact an operator needs.
pub fn log_listening(addr: SocketAddr, remote: bool, authenticated: bool) {
    if remote {
        warn!(
            %addr,
            authenticated,
            "database admin endpoint is listening beyond loopback: anyone who can reach this \
             address and holds the token can read and write the whole palace database, and the \
             token crosses the network in cleartext"
        );
    } else {
        info!(%addr, "database admin endpoint listening on loopback");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_header_yields_its_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&headers).as_deref(), Some("abc"));
    }

    #[test]
    fn anything_but_a_non_empty_bearer_credential_yields_nothing() {
        for value in ["Basic abc", "Bearer ", "abc"] {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, value.parse().unwrap());
            assert_eq!(bearer(&headers), None, "{value:?}");
        }
        assert_eq!(bearer(&HeaderMap::new()), None);
    }
}
