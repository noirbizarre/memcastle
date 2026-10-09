//! The daemon composition root: wires storage, the job scheduler, and the
//! HTTP/MCP listeners together, and owns graceful shutdown.
//!
//! This is the *only* place the daemon constructs a [`SurrealStore`] and a
//! [`Scheduler`] (`memcastle migrate` opens a store too, but only to run
//! migrations, never to serve) — everything else (api, mcp, the CLI's
//! client commands) only ever sees them through [`AppServices`] or, for the
//! CLI, through [`crate::client::DaemonClient`] talking to this process over
//! HTTP.

pub mod lifecycle;

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::app::{
    AppServices, AuthPolicy, ConfigReport, DbEndpoint, MinerRegistry, RuntimeContext,
    TriggerRegistry,
};
use crate::config::{Config, Secret};
use crate::credential::Credentials;
use crate::embed::Embeddings;
use crate::error::{Error, Result};
use crate::events::EventBus;
use crate::extract::Extraction;
use crate::jobs::Scheduler;
use crate::store::SurrealStore;

/// Run the daemon in the foreground: bind the listener, connect storage,
/// recover interrupted jobs, start the scheduler, then serve HTTP + MCP until
/// a shutdown signal (SIGINT/SIGTERM, or `POST /api/shutdown`) arrives.
///
/// # Errors
///
/// Returns [`Error::ServerBind`] if the listener can't bind, or an error if
/// storage can't be connected/migrated or the registry file can't be written.
pub async fn run(config: Config) -> Result<()> {
    // Resolved before the listener binds, so a mistyped `--assets-dir` fails
    // before anything is bound or changed. Purely local: it reads directories and
    // never the network, which is what lets a standalone binary start offline.
    let assets = Arc::new(crate::assets::Assets::resolve_for_process(
        config.assets.dir.as_deref(),
    )?);
    info!(source = %assets.source(), "runtime assets resolved");

    // Bound before any side effect (resolving assets above only reads): a
    // taken or invalid port is by far the most common startup failure, and
    // discovering it after creating the palace directory, migrating, and
    // recovering (re-queueing) jobs would leave a failed start having changed
    // things. The listener just holds connections in its backlog until
    // `axum::serve` below; the registry file, which clients discover the
    // daemon by, is still written only once everything is ready.
    let requested_addr = config.server.socket_addr();
    let listener = tokio::net::TcpListener::bind(requested_addr)
        .await
        .map_err(|source| Error::server_bind(requested_addr, source))?;
    // Not `requested_addr`: with port 0 the OS picks the port, and the
    // registry must record the real one for clients to reach the daemon.
    let actual_addr = listener
        .local_addr()
        .map_err(|source| Error::server_bind(requested_addr, source))?;

    std::fs::create_dir_all(&config.palace.path)
        .map_err(|source| Error::io(config.palace.path.display().to_string(), source))?;

    let backend = config.store.clone().into_backend(&config.palace.path);
    let store = SurrealStore::connect(&backend).await?;

    // The exact same runner `memcastle migrate` calls directly (see
    // `main.rs`'s `cmd_migrate`) — a failed migration propagates via `?`
    // here and the daemon never reaches the scheduler or the listener, so
    // it fails closed rather than serving a partially migrated database.
    let report = crate::migrate::run(&store).await?;
    if !report.applied.is_empty() {
        info!(
            from = report.from_version,
            to = report.to_version,
            applied = ?report.applied,
            "applied pending migrations"
        );
    }

    // Hashed here and the plaintext dropped with `config`: the daemon keeps
    // only the digest of a configured secret.
    let auth_policy = AuthPolicy::new(
        config.auth.enabled,
        config.auth.token.as_ref().map(Secret::expose),
    );
    // After migrations (the verifier table exists) and before the scheduler
    // starts: a daemon that could never authenticate anyone must fail before
    // it has changed or served anything.
    auth_policy.ensure_satisfiable(&store).await?;

    // One provider handle for the scheduler's embed jobs and the services'
    // query vectors. Built before anything serves so a bad `[embeddings]`
    // section fails startup, not the first search.
    let embeddings = Embeddings::from_config(&config.embeddings)?;
    // Likewise for `[extraction]`: a half-written section fails startup.
    let extraction = Extraction::from_config(&config.extraction)?;
    // One handle for the scheduler's mining jobs (which ask for access tokens) and the services (which sign sources in
    // and report whether they are). Nothing is read or written until a source that signs in is touched, so a daemon
    // with no such source never starts the platform keyring.
    let credentials = Credentials::from_config(&config.credentials);
    // One bus for the scheduler (job changes, the handlers' writes) and the services (their own writes), so
    // `GET /api/events` hears both.
    let events = EventBus::new();
    let mut scheduler = Scheduler::new(store.clone(), config.jobs.max_concurrency)
        .with_background_concurrency(config.jobs.background_concurrency)
        .with_events(events.clone())
        .with_credentials(Arc::new(credentials.clone()))
        .with_embeddings(embeddings.clone())
        .with_extraction(extraction.clone())
        .with_mining(config.effective_mining())
        .with_dedup(config.dedup.clone())
        .with_preferences(config.effective_preferences())
        .with_drain_timeout(Duration::from_secs(config.jobs.drain_timeout_secs))
        .with_lease_ttl(Duration::from_secs(config.jobs.lease_ttl_secs));
    if backend.is_shared() {
        // Another daemon may be running jobs against this same palace: only
        // reclaim the ones whose lease has actually lapsed.
        scheduler = scheduler.with_shared_store();
    }
    let scheduler = Arc::new(scheduler);
    info!(
        max_concurrency = config.jobs.max_concurrency,
        background_concurrency = config.jobs.background_concurrency,
        lease_ttl_secs = config.jobs.lease_ttl_secs,
        shared_store = backend.is_shared(),
        "job scheduler configured; recovering interrupted jobs"
    );
    scheduler.recover().await?;
    // Drawers written before a provider was configured (or while it was down)
    // are covered by one sweep at startup; a no-op without a provider.
    scheduler.ensure_embedding_sweep().await;
    // Likewise for mined drawers nobody has read yet; a no-op without a provider.
    scheduler.ensure_extraction_sweep().await;

    let shutdown = CancellationToken::new();
    let dispatch_handle = {
        let scheduler = Arc::clone(&scheduler);
        let shutdown = shutdown.clone();
        tokio::spawn(async move { scheduler.run(shutdown).await })
    };

    let trigger_runtime = Arc::new(crate::trigger::Runtime::default());
    let backend_info = backend.describe();
    let backend_info_kind = backend_info.kind.to_string();
    let app = AppServices::new(store, Arc::clone(&scheduler))
        .with_events(events)
        .with_credentials(credentials)
        .with_mining(config.effective_mining())
        .with_embeddings(embeddings)
        .with_extraction(extraction)
        .with_dedup(config.dedup.clone())
        .with_preferences(config.effective_preferences())
        // Starts from what the file held at startup and reads the file again whenever it changes, so the daemon
        // and a hand edit never disagree for long (`docs/adr/037`).
        .with_miners(MinerRegistry::new(
            config.config_file.clone(),
            config.miners.clone(),
        ))
        // Every trigger is off until the user enables it, so a daemon with none enabled runs no task, opens no port and
        // watches no file here (`docs/adr/043`); the supervisor below only starts what is.
        .with_triggers(TriggerRegistry::new(
            config.config_file.clone(),
            config.triggers.clone(),
            config.webhook.clone(),
            Arc::clone(&trigger_runtime),
        ))
        .with_runtime(RuntimeContext {
            // The real bound address, not the requested one: `status` must agree
            // with the registry file when port 0 was asked for.
            bind_addr: actual_addr.to_string(),
            palace_path: config.palace.path.display().to_string(),
            backend: backend_info.kind.to_string(),
            location: backend_info.location,
            // What `GET /api/config` reports, and what tells the authentication layer whether `/ui` is served.
            config: ConfigReport::from_config(&config, &assets),
        })
        .with_auth(auth_policy)
        // Constructed, never started: the admin endpoint listens only after an
        // explicit `memcastle db start` (`docs/adr/015`), so a plain daemon
        // opens exactly the one listener it always did.
        .with_db_endpoint(DbEndpoint::new(config.db.clone(), shutdown.clone()));
    if config.auth.enabled {
        info!(
            "authentication is enabled: every route except GET /api/health requires a bearer token"
        );
    } else if !config.server.bind.is_loopback() {
        // Not an error (the operator may front the daemon with something else)
        // but never silent: this is the configuration where anyone on the
        // network can read and write the palace.
        warn!(
            bind = %config.server.bind,
            "listening beyond loopback with authentication disabled: anyone who can reach this \
             address can read and write the palace; set auth.enabled (see docs/authentication.md)"
        );
    }
    // Deliveries accepted before a crash but never queued are queued now, for the triggers that are still enabled.
    app.recover_triggers().await;
    // Stopped first on the way down (below), so nothing new is asked for while the jobs drain.
    let trigger_handle = tokio::spawn(
        crate::trigger::Supervisor::new(app.trigger_host(), trigger_runtime)
            .run(shutdown.child_token()),
    );
    // Kept for the shutdown below: the layer takes `app` by value.
    let services = app.clone();
    let mcp_service = crate::mcp::service(app.clone(), &shutdown);
    // One trace layer over both surfaces: a request line at `debug` on the
    // way in and a response line (status, latency) on the way out, so "what
    // did the daemon receive, and what did it answer?" has an answer for MCP
    // and REST alike. A failed response (5xx) is logged at `error` by the
    // layer itself; rejections are logged with their diagnostic by
    // `api::ApiError`, which the layer cannot see.
    //
    // Authentication wraps REST and MCP together, *inside* the trace layer (the
    // layer added last is outermost) so a refused request is still traced.
    // Added to the merged router rather than per route, so a route added later
    // cannot be forgotten. The trace layer logs no headers, which is what
    // keeps the `Authorization` header out of the log.
    let mut router =
        crate::api::router(app.clone(), shutdown.clone()).nest_service("/mcp", mcp_service);
    if config.web.enable {
        // Merged before the authentication layer is added, so `/ui` is inside it like every other route: the layer
        // admits its static files itself (`api::auth::is_public`), and only while this is enabled.
        router = router.merge(crate::api::web_router(Arc::clone(&assets)));
        if assets.web_is_built() {
            info!(path = crate::api::UI_PREFIX, assets = %assets.source(), "web UI enabled");
        } else {
            warn!(
                path = crate::api::UI_PREFIX,
                assets = %assets.source(),
                "web UI enabled but web/dist/index.html was not found: /ui answers a page that says how to install it \
                 (install a package, or build it with `mise run web:build` and pass --assets-dir)"
            );
        }
    }
    let router = router
        .layer(axum::middleware::from_fn_with_state(
            app,
            crate::api::require_auth,
        ))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                // Method, path and request id only: no query string (it can
                // carry search terms) and no headers (`Authorization`).
                .make_span_with(|request: &axum::http::Request<_>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    tracing::debug_span!(
                        "request",
                        %request_id,
                        method = %request.method(),
                        path = request.uri().path(),
                    )
                })
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::DEBUG),
                ),
        )
        // Outside the trace layer so the span sees the id; echoed back so a
        // client can quote it when reporting a failure.
        .layer(tower_http::request_id::PropagateRequestIdLayer::x_request_id())
        .layer(tower_http::request_id::SetRequestIdLayer::x_request_id(
            UuidRequestId,
        ));

    lifecycle::write(
        &config.palace.path,
        &lifecycle::RuntimeInfo {
            pid: std::process::id(),
            bind_addr: actual_addr.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    )?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        bind = %actual_addr,
        palace = %config.palace.path.display(),
        backend = %backend_info_kind,
        "memcastle daemon listening"
    );

    let shutdown_signal = shutdown_signal(shutdown.clone());
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await;

    // Closed before the jobs drain: it shares the daemon's database handle, and
    // a Studio connection left open must not outlive the daemon's storage.
    // Idempotent, and a no-op when the endpoint was never started.
    services.stop_db_endpoint().await;
    // Before the jobs drain: a trigger that is still asking for runs would only queue work the daemon is about to leave.
    let _ = trigger_handle.await;

    // The dispatch loop exits on the same `shutdown` token and then drains
    // in-flight jobs (`Scheduler::drain`); wait for it rather than aborting,
    // so a job mid-checkpoint gets to finish writing before we remove the
    // registry file out from under it.
    let _ = dispatch_handle.await;
    lifecycle::remove(&config.palace.path);
    info!("memcastle daemon stopped");

    serve_result.map_err(|source| Error::server(source.to_string()))
}

/// Gives every request a fresh UUID unless the caller sent one.
#[derive(Clone, Copy)]
struct UuidRequestId;

impl tower_http::request_id::MakeRequestId for UuidRequestId {
    fn make_request_id<B>(
        &mut self,
        _request: &axum::http::Request<B>,
    ) -> Option<tower_http::request_id::RequestId> {
        axum::http::HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .ok()
            .map(tower_http::request_id::RequestId::new)
    }
}

/// Resolve when SIGINT, SIGTERM (Unix only), or `shutdown` itself fires —
/// and cancel `shutdown` either way, so the scheduler's dispatch loop and
/// axum's graceful-shutdown future always agree on when to stop.
async fn shutdown_signal(shutdown: CancellationToken) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
        () = shutdown.cancelled() => {}
    }
    shutdown.cancel();
}
