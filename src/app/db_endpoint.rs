//! The database admin endpoint's lifecycle: starting it, stopping it and
//! reporting on it (`docs/adr/015`).
//!
//! This is where every safety decision is made, in the daemon and not in the
//! CLI that asked: the endpoint binds loopback unless told otherwise, a
//! non-loopback bind needs an explicit opt-in *and* authentication, and
//! nothing starts it except an explicit request. `memcastle serve` never does.
//!
//! Like authentication, it is deliberately unreachable from MCP: an agent
//! integration must not be able to open a database console.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::config::DbConfig;
use crate::dbadmin::{self, Authenticator, OriginPolicy};
use crate::error::{Error, Result};
use crate::store::{EMBEDDED_DATABASE, EMBEDDED_NAMESPACE};

use super::AppServices;

/// How long stopping waits for open connections to close before the listener
/// task is abandoned. Connections watch the same cancellation, so this only
/// bounds a pathological stall.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// What `db start` asks for. Each field left out falls back to the daemon's
/// `[db]` configuration, so `memcastle db start` alone does the safe default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbEndpointRequest {
    /// Interface to listen on.
    #[serde(default)]
    pub bind: Option<IpAddr>,
    /// Port to listen on; `0` asks the OS for a free one.
    #[serde(default)]
    pub port: Option<u16>,
    /// Permit a non-loopback bind.
    #[serde(default)]
    pub allow_remote: Option<bool>,
    /// Further browser origins to allow, added to the configured ones.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

/// Whether the admin endpoint is listening, and where.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DbEndpointStatus {
    /// Whether the endpoint is listening.
    pub running: bool,
    /// The address it is listening on, when it is.
    #[serde(default)]
    pub addr: Option<String>,
    /// The URL to give SurrealDB Studio, when it is listening.
    #[serde(default)]
    pub url: Option<String>,
    /// The namespace to select in Studio.
    #[serde(default)]
    pub namespace: String,
    /// The database to select in Studio.
    #[serde(default)]
    pub database: String,
    /// The username to sign in with in Studio (the password is the token, or
    /// the same value when authentication is disabled).
    #[serde(default)]
    pub user: String,
    /// Whether it listens beyond loopback.
    #[serde(default)]
    pub remote: bool,
    /// Whether a connection must present the daemon's token.
    #[serde(default)]
    pub auth_required: bool,
    /// When it was started.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    /// Set only on the response to a start that found the endpoint already
    /// listening; every other report leaves it `false`.
    #[serde(default)]
    pub already_running: bool,
}

impl DbEndpointStatus {
    /// This status, marked as the answer to a start that changed nothing.
    fn already_running(self) -> Self {
        Self {
            already_running: true,
            ..self
        }
    }
}

/// A listener that is up.
struct Running {
    addr: SocketAddr,
    remote: bool,
    auth_required: bool,
    /// The origin policy it was started with, kept to tell whether a later
    /// start asks for something it already allows.
    origins: OriginPolicy,
    started_at: DateTime<Utc>,
    /// Stops this listener alone (a child of the daemon's shutdown token).
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

impl Running {
    /// Whether `request` asks for nothing this listener does not already do.
    ///
    /// Only what the request states explicitly is compared: a field left out
    /// means "whatever is configured", which cannot contradict a running
    /// listener. `allow_remote` is ignored because it only gates a new bind.
    fn satisfies(&self, request: &DbEndpointRequest) -> bool {
        request.bind.is_none_or(|bind| bind == self.addr.ip())
            // `0` asks the OS for any free port, which the running one is.
            && request
                .port
                .is_none_or(|port| port == 0 || port == self.addr.port())
            // Local pages are always allowed, so only origins beyond them can
            // be a mismatch.
            && request
                .allowed_origins
                .iter()
                .all(|origin| self.origins.allows(origin.trim_end_matches('/')))
    }
}

/// Holds the endpoint's state. At most one listener exists at a time.
pub struct DbEndpoint {
    defaults: DbConfig,
    /// The daemon's shutdown token: stopping the daemon stops the endpoint.
    shutdown: CancellationToken,
    state: Mutex<Option<Running>>,
}

impl DbEndpoint {
    /// An endpoint controller with `defaults` for whatever a request omits.
    #[must_use]
    pub fn new(defaults: DbConfig, shutdown: CancellationToken) -> Self {
        Self {
            defaults,
            shutdown,
            state: Mutex::new(None),
        }
    }

    fn status_of(running: Option<&Running>) -> DbEndpointStatus {
        match running {
            None => stopped(),
            Some(running) => DbEndpointStatus {
                running: true,
                addr: Some(running.addr.to_string()),
                url: Some(format!("ws://{}", running.addr)),
                remote: running.remote,
                auth_required: running.auth_required,
                started_at: Some(running.started_at),
                ..stopped()
            },
        }
    }
}

impl Default for DbEndpoint {
    fn default() -> Self {
        Self::new(DbConfig::default(), CancellationToken::new())
    }
}

/// The status for "not listening".
fn stopped() -> DbEndpointStatus {
    DbEndpointStatus {
        namespace: EMBEDDED_NAMESPACE.to_string(),
        database: EMBEDDED_DATABASE.to_string(),
        user: dbadmin::SIGNIN_USER.to_string(),
        ..DbEndpointStatus::default()
    }
}

impl AppServices {
    /// Replace the admin endpoint's controller. Set once by `server::run`, which
    /// is the only place that knows the daemon's `[db]` configuration and
    /// shutdown token.
    #[must_use]
    pub fn with_db_endpoint(mut self, endpoint: DbEndpoint) -> Self {
        self.db_endpoint = Arc::new(endpoint);
        self
    }

    /// Start the admin endpoint, returning where it listens.
    ///
    /// Starting an endpoint that is already listening succeeds with its
    /// current status, marked `already_running`, as long as the request does
    /// not contradict it.
    ///
    /// # Errors
    ///
    /// - [`Error::DbEndpointUnavailable`] for a remote palace, which has a
    ///   server of its own.
    /// - [`Error::DbEndpointRunning`] if it is already listening and the
    ///   request asks for a different bind, port or origin.
    /// - [`Error::DbEndpointUnsafe`] for a non-loopback bind without the opt-in
    ///   or without authentication enabled.
    /// - [`Error::DbEndpointBind`] if the address cannot be bound.
    pub async fn start_db_endpoint(&self, request: DbEndpointRequest) -> Result<DbEndpointStatus> {
        let endpoint = &self.db_endpoint;
        // Held across the whole start so two racing requests cannot both
        // decide it is not running and both bind.
        let mut state = endpoint.state.lock().await;

        if self.runtime.backend == "remote" {
            return Err(Error::DbEndpointUnavailable {
                backend: self.runtime.backend.clone(),
            });
        }
        // A listener that died on its own is not "running".
        if state
            .as_ref()
            .is_some_and(|running| running.task.is_finished())
        {
            *state = None;
        }
        if let Some(running) = state.as_ref() {
            // Starting what is already started is a success: the caller wants
            // the endpoint up and it is, so report where instead of failing
            // a script that simply runs `db start` twice.
            if running.satisfies(&request) {
                return Ok(DbEndpoint::status_of(Some(running)).already_running());
            }
            // The caller asked for something else (another port, a new
            // origin). Reporting the old endpoint as if it matched would
            // leave them connecting to the wrong place, so refuse.
            return Err(Error::DbEndpointRunning {
                addr: running.addr.to_string(),
            });
        }

        let defaults = &endpoint.defaults;
        let bind = request.bind.unwrap_or(defaults.bind);
        let port = request.port.unwrap_or(defaults.port);
        let allow_remote = request.allow_remote.unwrap_or(defaults.allow_remote);
        let remote = !bind.is_loopback();
        if remote && !allow_remote {
            return Err(Error::DbEndpointUnsafe {
                reason: format!(
                    "{bind} is not a loopback address and `--allow-remote` was not given"
                ),
            });
        }
        // The token is the only thing between the network and a writable copy
        // of the palace, so there is no "remote but open" combination.
        if remote && !self.auth_enabled() {
            return Err(Error::DbEndpointUnsafe {
                reason: format!(
                    "{bind} is not a loopback address and this daemon has authentication disabled"
                ),
            });
        }

        let addr = SocketAddr::new(bind, port);
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| Error::DbEndpointBind { addr, source })?;
        // The real port: with `0` the OS chose it.
        let addr = listener
            .local_addr()
            .map_err(|source| Error::DbEndpointBind { addr, source })?;

        let origins = OriginPolicy::new(
            defaults
                .allowed_origins
                .iter()
                .cloned()
                .chain(request.allowed_origins),
        );
        let auth_required = self.auth_enabled();
        let options = dbadmin::Options {
            origins: origins.clone(),
            auth: auth_required.then(|| self.authenticator()),
        };
        let cancel = endpoint.shutdown.child_token();
        let task = tokio::spawn({
            let session = self.store.session();
            let cancel = cancel.clone();
            async move {
                if let Err(error) = dbadmin::serve(listener, session, options, cancel).await {
                    error!(%error, "database admin endpoint stopped unexpectedly");
                }
            }
        });
        dbadmin::log_listening(addr, remote, auth_required);

        let running = Running {
            addr,
            remote,
            auth_required,
            origins,
            started_at: Utc::now(),
            cancel,
            task,
        };
        let status = DbEndpoint::status_of(Some(&running));
        *state = Some(running);
        Ok(status)
    }

    /// Stop the admin endpoint, closing its open connections. Succeeds when it
    /// was not running: the caller wants it off, and it is.
    pub async fn stop_db_endpoint(&self) -> DbEndpointStatus {
        let running = self.db_endpoint.state.lock().await.take();
        if let Some(running) = running {
            running.cancel.cancel();
            // Awaited so the port is free and the connections closed by the time
            // this returns; bounded so a stuck connection cannot hang a
            // shutdown.
            let mut task = running.task;
            if tokio::time::timeout(STOP_TIMEOUT, &mut task).await.is_err() {
                task.abort();
            }
            tracing::info!(addr = %running.addr, "database admin endpoint stopped");
        }
        stopped()
    }

    /// Whether and where the admin endpoint is listening.
    pub async fn db_endpoint_status(&self) -> DbEndpointStatus {
        let mut state = self.db_endpoint.state.lock().await;
        // A listener task that ended on its own (an I/O error, or the daemon's
        // shutdown) must not be reported as up.
        if state
            .as_ref()
            .is_some_and(|running| running.task.is_finished())
        {
            *state = None;
        }
        DbEndpoint::status_of(state.as_ref())
    }

    /// The check each connection's token goes through: the daemon's own
    /// [`AppServices::authenticate`], so the admin endpoint accepts exactly the
    /// credentials the REST API does and a revoked token stops working here too.
    fn authenticator(&self) -> Authenticator {
        let app = self.clone();
        Arc::new(move |presented: Option<String>| {
            let app = app.clone();
            Box::pin(async move {
                match presented {
                    Some(token) => app.authenticate(Some(&token)).await.is_ok(),
                    None => false,
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AuthPolicy;

    const SECRET: &str = "mc_a_secret_for_the_tests_0123456789";

    async fn app(auth: bool) -> AppServices {
        AppServices::for_tests()
            .await
            .with_auth(AuthPolicy::new(auth, auth.then_some(SECRET)))
    }

    fn loopback_ephemeral() -> DbEndpointRequest {
        DbEndpointRequest {
            port: Some(0),
            ..DbEndpointRequest::default()
        }
    }

    fn remote_ephemeral(allow_remote: bool) -> DbEndpointRequest {
        DbEndpointRequest {
            bind: Some("0.0.0.0".parse().unwrap()),
            port: Some(0),
            allow_remote: Some(allow_remote),
            ..DbEndpointRequest::default()
        }
    }

    #[tokio::test]
    async fn a_fresh_daemon_reports_the_endpoint_as_stopped() {
        let status = app(false).await.db_endpoint_status().await;
        assert!(!status.running);
        assert_eq!(status.addr, None);
    }

    #[tokio::test]
    async fn the_endpoint_listens_on_loopback_unless_told_otherwise() {
        let app = app(false).await;
        let status = app.start_db_endpoint(loopback_ephemeral()).await.unwrap();

        assert!(status.running);
        assert!(!status.remote);
        assert!(status.addr.unwrap().starts_with("127.0.0.1:"));
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn starting_twice_reports_the_running_endpoint_instead_of_failing() {
        let app = app(false).await;
        let first = app.start_db_endpoint(loopback_ephemeral()).await.unwrap();
        assert!(!first.already_running);

        let second = app
            .start_db_endpoint(loopback_ephemeral())
            .await
            .expect("a repeated start is not an error");
        assert!(second.already_running);
        assert_eq!(second.addr, first.addr);
        assert_eq!(second.started_at, first.started_at);
        // Only a start reports it: a plain status read does not.
        assert!(!app.db_endpoint_status().await.already_running);
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn starting_again_without_any_setting_reports_the_running_endpoint() {
        let app = app(false).await;
        app.start_db_endpoint(loopback_ephemeral()).await.unwrap();

        let again = app
            .start_db_endpoint(DbEndpointRequest::default())
            .await
            .unwrap();
        assert!(again.already_running);
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn starting_again_on_another_port_is_a_conflict_that_names_where_it_listens() {
        let app = app(false).await;
        let first = app.start_db_endpoint(loopback_ephemeral()).await.unwrap();
        let running_port = first.addr.as_ref().unwrap().rsplit(':').next().unwrap();
        let other = if running_port == "65000" {
            65001
        } else {
            65000
        };

        let error = app
            .start_db_endpoint(DbEndpointRequest {
                port: Some(other),
                ..DbEndpointRequest::default()
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&error, Error::DbEndpointRunning { addr } if Some(addr) == first.addr.as_ref()),
            "{error}"
        );
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn starting_again_on_the_running_port_is_not_a_conflict() {
        let app = app(false).await;
        let first = app.start_db_endpoint(loopback_ephemeral()).await.unwrap();
        let port = first.addr.as_ref().unwrap().rsplit(':').next().unwrap();

        let again = app
            .start_db_endpoint(DbEndpointRequest {
                port: Some(port.parse().unwrap()),
                ..DbEndpointRequest::default()
            })
            .await
            .unwrap();
        assert!(again.already_running);
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn starting_again_on_another_interface_is_a_conflict() {
        let app = app(false).await;
        app.start_db_endpoint(loopback_ephemeral()).await.unwrap();

        let error = app
            .start_db_endpoint(DbEndpointRequest {
                bind: Some("::1".parse().unwrap()),
                ..DbEndpointRequest::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(error, Error::DbEndpointRunning { .. }), "{error}");
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn starting_again_with_an_origin_it_does_not_allow_is_a_conflict() {
        let app = app(false).await;
        app.start_db_endpoint(DbEndpointRequest {
            allowed_origins: vec!["https://studio.example".to_string()],
            ..loopback_ephemeral()
        })
        .await
        .unwrap();

        let known = app
            .start_db_endpoint(DbEndpointRequest {
                allowed_origins: vec!["https://studio.example/".to_string()],
                ..DbEndpointRequest::default()
            })
            .await
            .unwrap();
        assert!(known.already_running);

        let error = app
            .start_db_endpoint(DbEndpointRequest {
                allowed_origins: vec!["https://other.example".to_string()],
                ..DbEndpointRequest::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(error, Error::DbEndpointRunning { .. }), "{error}");
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn a_non_loopback_bind_is_refused_without_the_explicit_opt_in() {
        let app = app(true).await;
        let error = app
            .start_db_endpoint(remote_ephemeral(false))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::DbEndpointUnsafe { .. }), "{error}");
        assert!(!app.db_endpoint_status().await.running);
    }

    #[tokio::test]
    async fn a_non_loopback_bind_is_refused_while_authentication_is_disabled() {
        let app = app(false).await;
        let error = app
            .start_db_endpoint(remote_ephemeral(true))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, Error::DbEndpointUnsafe { reason } if reason.contains("authentication")),
            "{error}"
        );
        assert!(!app.db_endpoint_status().await.running);
    }

    #[tokio::test]
    async fn a_non_loopback_bind_with_the_opt_in_and_authentication_is_allowed() {
        let app = app(true).await;
        let status = app.start_db_endpoint(remote_ephemeral(true)).await.unwrap();
        assert!(status.remote && status.auth_required);
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn stopping_closes_the_endpoint_and_is_idempotent() {
        let app = app(false).await;
        app.start_db_endpoint(loopback_ephemeral()).await.unwrap();

        assert!(!app.stop_db_endpoint().await.running);
        assert!(!app.stop_db_endpoint().await.running);
        assert!(!app.db_endpoint_status().await.running);
        // Restartable after a stop.
        app.start_db_endpoint(loopback_ephemeral()).await.unwrap();
        app.stop_db_endpoint().await;
    }

    #[tokio::test]
    async fn the_daemons_shutdown_stops_the_endpoint() {
        let shutdown = CancellationToken::new();
        let app = app(false)
            .await
            .with_db_endpoint(DbEndpoint::new(DbConfig::default(), shutdown.clone()));
        app.start_db_endpoint(loopback_ephemeral()).await.unwrap();

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), async {
            while app.db_endpoint_status().await.running {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the endpoint stopped with the daemon");
    }
}
