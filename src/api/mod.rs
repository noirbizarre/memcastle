//! The HTTP API: health, status, search/recall/wake-up/diary, job inspection/control and shutdown.
//!
//! Every handler is a deserialize -> call one `AppServices` method ->
//! serialize sandwich — no business logic lives here. This is also where
//! `/api/shutdown` lives, which is how `memcastle stop` asks a foreground
//! `memcastle serve` to shut down gracefully (see `server::lifecycle`).

mod error;
mod extract;
mod mode;

use std::str::FromStr;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::app::{AppServices, WakeUpBudget};
use crate::domain::{JobId, JobKind, JobStatus, MiningSource};

use extract::{ApiJson, ApiQuery};

pub use error::ApiError;
pub use mode::ModeHeader;

#[derive(Clone)]
struct ApiState {
    app: AppServices,
    shutdown: CancellationToken,
}

/// Build the API router. `shutdown` is fired by `POST /api/shutdown`; the
/// caller (`server::run`) is what actually stops the listener in response.
pub fn router(app: AppServices, shutdown: CancellationToken) -> Router {
    let state = ApiState { app, shutdown };
    Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/search", get(search))
        .route("/api/recall", get(recall))
        .route("/api/wake-up", get(wake_up))
        .route("/api/diary", get(diary_read).post(diary_write))
        .route("/api/jobs", get(list_jobs).post(submit_job))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/pause", post(pause_job))
        .route("/api/jobs/{id}/resume", post(resume_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/jobs/{id}/retry", post(retry_job))
        .route("/api/shutdown", post(shutdown_now))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn status(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.status(mode).await?))
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to one wing by name (see `AppServices::search`).
    wing: Option<String>,
    /// Restrict results to one room by name.
    room: Option<String>,
}

fn default_search_limit() -> u32 {
    crate::app::DEFAULT_SEARCH_LIMIT
}

async fn search(
    State(state): State<ApiState>,
    ApiQuery(params): ApiQuery<SearchParams>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .search(
                &params.q,
                params.wing.as_deref(),
                params.room.as_deref(),
                params.limit,
                mode,
            )
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct RecallParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to one wing by name (see `AppServices::recall`).
    wing: Option<String>,
}

async fn recall(
    State(state): State<ApiState>,
    ApiQuery(params): ApiQuery<RecallParams>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .recall(&params.q, params.wing.as_deref(), params.limit, mode)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct WakeUpParams {
    agent_identity: String,
    wing: Option<String>,
    /// Falls back to `WakeUpBudget::default()`'s fields when absent — see
    /// that impl for the exact numbers.
    max_items: Option<usize>,
    max_bytes: Option<usize>,
}

async fn wake_up(
    State(state): State<ApiState>,
    ApiQuery(params): ApiQuery<WakeUpParams>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    let budget = WakeUpBudget::from_options(params.max_items, params.max_bytes);
    Ok(Json(
        state
            .app
            .wake_up(&params.agent_identity, params.wing.as_deref(), budget, mode)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct DiaryWriteBody {
    /// The identity to scope this entry to — see `AppServices::diary_write`.
    agent_identity: String,
    /// The wing to file this entry under (fixed `"diary"` room within it).
    wing: String,
    /// The entry's content.
    content: String,
    /// The channel this write came through, recorded as
    /// `provenance.requested_by` — `"http"` unless a caller says otherwise
    /// (the CLI sends `"cli"`).
    #[serde(default = "default_requested_by")]
    requested_by: String,
}

async fn diary_write(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(body): ApiJson<DiaryWriteBody>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .diary_write(
                &body.agent_identity,
                &body.wing,
                body.content,
                &body.requested_by,
                mode,
            )
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct DiaryReadParams {
    agent_identity: String,
    wing: String,
    #[serde(default = "default_diary_limit")]
    limit: u32,
}

fn default_diary_limit() -> u32 {
    crate::app::DEFAULT_DIARY_LIMIT
}

async fn diary_read(
    State(state): State<ApiState>,
    ApiQuery(params): ApiQuery<DiaryReadParams>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .diary_read(&params.agent_identity, &params.wing, params.limit, mode)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
struct ListJobsParams {
    status: Option<String>,
}

async fn list_jobs(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiQuery(params): ApiQuery<ListJobsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let status = params.status.map(|s| parse_status(&s)).transpose()?;
    Ok(Json(state.app.list_jobs(status, mode).await?))
}

#[derive(Debug, Deserialize)]
struct SubmitJobBody {
    #[serde(flatten)]
    kind: JobKind,
    #[serde(default = "default_requested_by")]
    requested_by: String,
    /// Only meaningful for `kind: Checkpoint` — escalates to
    /// `Priority::Critical` instead of the default `Priority::High`. A
    /// boolean rather than an exposed `Priority` field, so a caller can
    /// only pick between the two levels this job kind actually sanctions.
    #[serde(default)]
    emergency: bool,
}

fn default_requested_by() -> String {
    "http".to_string()
}

async fn submit_job(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(body): ApiJson<SubmitJobBody>,
) -> Result<impl IntoResponse, ApiError> {
    let job = match body.kind {
        JobKind::Mine { source, wing } => {
            let MiningSource::Directory { path } = source;
            // Gated as a write inside `submit_mine`: mining files drawers.
            state
                .app
                .submit_mine(path, wing, &body.requested_by, mode)
                .await?
        }
        JobKind::Demo { steps } => state.app.submit_demo(steps, &body.requested_by).await?,
        JobKind::Checkpoint { payload } => {
            if body.emergency {
                state
                    .app
                    .emergency_checkpoint(payload, &body.requested_by, mode)
                    .await?
            } else {
                state
                    .app
                    .checkpoint(payload, &body.requested_by, mode)
                    .await?
            }
        }
        JobKind::Audit { scope } => {
            // Not gated by `mode` — same reasoning as `Mine` above (see
            // `AppServices::submit_audit`'s doc comment).
            state.app.submit_audit(scope, &body.requested_by).await?
        }
        JobKind::Repair {
            dry_run,
            based_on_job,
        } => {
            // A dry run is ungated like `Audit`; an applied repair is gated
            // as a write inside `submit_repair` (see its doc comment).
            state
                .app
                .submit_repair(dry_run, based_on_job, &body.requested_by, mode)
                .await?
        }
    };
    Ok(Json(job))
}

async fn get_job(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_job_id(&id)?;
    Ok(Json(state.app.get_job(id, mode).await?))
}

async fn pause_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let result = state.app.pause_job(parse_job_id(&id)?).await?;
    // "requested", not "paused": pausing is cooperative, so the job stops
    // at its next check (every handler has one, audit and repair included)
    // rather than at the instant of the request.
    Ok(Json(result))
}

async fn resume_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let result = state.app.resume_job(parse_job_id(&id)?).await?;
    Ok(Json(result))
}

async fn cancel_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let result = state.app.cancel_job(parse_job_id(&id)?).await?;
    Ok(Json(result))
}

async fn retry_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let result = state.app.retry_job(parse_job_id(&id)?).await?;
    Ok(Json(result))
}

async fn shutdown_now(State(state): State<ApiState>) -> impl IntoResponse {
    state.shutdown.cancel();
    Json(serde_json::json!({ "status": "shutting_down" }))
}

fn parse_job_id(raw: &str) -> Result<JobId, ApiError> {
    JobId::from_str(raw).map_err(|_| ApiError::from(crate::Error::invalid_job_id(raw)))
}

fn parse_status(raw: &str) -> Result<JobStatus, ApiError> {
    raw.parse()
        .map_err(|message: String| ApiError::from(crate::Error::invalid_input("status", message)))
}
