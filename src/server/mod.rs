//! The daemon composition root: wires storage, the job scheduler, and the
//! HTTP/MCP listeners together, and owns graceful shutdown.
//!
//! This is the *only* place that constructs a [`SurrealStore`] and a
//! [`Scheduler`] — everything else (api, mcp, the CLI's non-`serve`
//! commands) only ever sees them through [`AppServices`] or, for the CLI,
//! through [`crate::client::DaemonClient`] talking to this process over
//! HTTP.

pub mod lifecycle;

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::app::AppServices;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::jobs::Scheduler;
use crate::store::SurrealStore;

/// Run the daemon in the foreground: connect storage, recover interrupted
/// jobs, start the scheduler, then serve HTTP + MCP until a shutdown signal
/// (SIGINT/SIGTERM, or `POST /api/shutdown`) arrives.
///
/// # Errors
///
/// Returns an error if storage can't be connected/migrated, the HTTP
/// listener can't bind, or the registry file can't be written.
pub async fn run(config: Config) -> Result<()> {
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

    let mut scheduler = Scheduler::new(store.clone(), config.jobs.max_concurrency)
        .with_drain_timeout(Duration::from_secs(config.jobs.drain_timeout_secs))
        .with_lease_ttl(Duration::from_secs(config.jobs.lease_ttl_secs));
    if backend.is_shared() {
        // Another daemon may be running jobs against this same palace: only
        // reclaim the ones whose lease has actually lapsed.
        scheduler = scheduler.with_shared_store();
    }
    let scheduler = Arc::new(scheduler);
    scheduler.recover().await?;

    let shutdown = CancellationToken::new();
    let dispatch_handle = {
        let scheduler = Arc::clone(&scheduler);
        let shutdown = shutdown.clone();
        tokio::spawn(async move { scheduler.run(shutdown).await })
    };

    let app = AppServices::new(store, Arc::clone(&scheduler));
    let mcp_service = crate::mcp::service(app.clone(), &shutdown);
    // One trace layer over both surfaces: a request line at `debug` on the
    // way in and a response line (status, latency) on the way out, so "what
    // did the daemon receive, and what did it answer?" has an answer for MCP
    // and REST alike. A failed response (5xx) is logged at `error` by the
    // layer itself; rejections are logged with their diagnostic by
    // `api::ApiError`, which the layer cannot see.
    let router = crate::api::router(app, shutdown.clone())
        .nest_service("/mcp", mcp_service)
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(
                    tower_http::trace::DefaultMakeSpan::new().level(tracing::Level::DEBUG),
                )
                .on_response(
                    tower_http::trace::DefaultOnResponse::new().level(tracing::Level::DEBUG),
                ),
        );

    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .map_err(|source| {
            Error::server(format!("failed to bind {}: {source}", config.server.bind))
        })?;
    let actual_addr = listener
        .local_addr()
        .map_err(|source| Error::server(source.to_string()))?;

    lifecycle::write(
        &config.palace.path,
        &lifecycle::RuntimeInfo {
            pid: std::process::id(),
            bind_addr: actual_addr.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    )?;
    info!(bind = %actual_addr, palace = %config.palace.path.display(), "memcastle daemon listening");

    let shutdown_signal = shutdown_signal(shutdown.clone());
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal)
        .await;

    // The dispatch loop exits on the same `shutdown` token and then drains
    // in-flight jobs (`Scheduler::drain`); wait for it rather than aborting,
    // so a job mid-checkpoint gets to finish writing before we remove the
    // registry file out from under it.
    let _ = dispatch_handle.await;
    lifecycle::remove(&config.palace.path);
    info!("memcastle daemon stopped");

    serve_result.map_err(|source| Error::server(source.to_string()))
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
