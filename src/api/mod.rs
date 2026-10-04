//! The HTTP API: health, status, search/recall/wake-up/diary, job inspection/control, wing/room/drawer
//! management, authentication tokens, the database endpoint and shutdown.
//!
//! Every handler is a deserialize -> call one `AppServices` method ->
//! serialize sandwich — no business logic lives here. This is also where
//! `/api/shutdown` lives, which is how `memcastle daemon stop` asks a
//! running `memcastle serve` to shut down gracefully (see `server::lifecycle`).

mod auth;
mod db;
mod error;
mod extract;
mod mode;
mod palace;

use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::app::{AppServices, WakeUpBudget};
use crate::domain::{JobId, JobKind, JobStatus};
use crate::search::{SearchOptions, SearchQuery};

use extract::{ApiJson, ApiQuery};

pub use auth::require_auth;
pub use error::ApiError;
pub use mode::ModeHeader;

#[derive(Clone)]
struct ApiState {
    app: AppServices,
    shutdown: CancellationToken,
}

/// Build the API router. `shutdown` is fired by `POST /api/shutdown`; the
/// caller (`server::run`) is what actually stops the listener in response.
///
/// Authentication is *not* applied here: `server::run` wraps this router and
/// `/mcp` together in [`require_auth`], so one layer guards both surfaces.
pub fn router(app: AppServices, shutdown: CancellationToken) -> Router {
    let state = ApiState { app, shutdown };
    Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/search", get(search).post(search_json))
        .route("/api/recall", get(recall).post(recall_json))
        .route("/api/wake-up", get(wake_up))
        .route("/api/diary", get(diary_read).post(diary_write))
        .route("/api/jobs", get(list_jobs).post(submit_job))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/pause", post(pause_job))
        .route("/api/jobs/{id}/resume", post(resume_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/jobs/{id}/retry", post(retry_job))
        .route("/api/sources", get(list_sources))
        .route("/api/shutdown", post(shutdown_now))
        // The hierarchy. Reads and creates are open to the same callers as any
        // other palace content; deletes have no MCP tool (docs/adr/018).
        .route(
            "/api/wings",
            get(palace::list_wings).post(palace::create_wing),
        )
        .route(
            "/api/wings/{wing}",
            get(palace::show_wing).delete(palace::delete_wing),
        )
        .route(
            "/api/wings/{wing}/rooms",
            get(palace::list_rooms).post(palace::create_room),
        )
        .route(
            "/api/wings/{wing}/rooms/{room}",
            get(palace::show_room).delete(palace::delete_room),
        )
        .route(
            "/api/wings/{wing}/rooms/{room}/drawers",
            get(palace::list_drawers).post(palace::create_drawer),
        )
        // A wildcard: a drawer's name may itself contain `/`.
        .route(
            "/api/wings/{wing}/rooms/{room}/drawers/{*drawer}",
            get(palace::show_drawer).delete(palace::delete_drawer),
        )
        // Derived and corrective writes on one drawer, by id (a search hit
        // already carries it). Not nested under the room path: a drawer name
        // may contain `/`, so that route's wildcard would swallow a suffix.
        .route(
            "/api/drawers/{id}/supersede",
            post(palace::supersede_drawer),
        )
        .route(
            "/api/drawers/{id}/embedding",
            axum::routing::put(palace::set_drawer_embedding),
        )
        .route(
            "/api/drawers/{id}/mentions",
            post(palace::link_drawer_entity),
        )
        // Administrative, and deliberately REST-only: there is no MCP tool for
        // these, so an agent integration cannot mint or revoke credentials.
        .route(
            "/api/auth/token",
            post(auth::generate_token).delete(auth::revoke_token),
        )
        // Also administrative and REST-only: an explicit, opt-in database console
        // for developers, never started by `serve` (docs/adr/015).
        .route("/api/db", get(db::status).post(db::start).delete(db::stop))
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

/// The query-string form of a search, shared by `search` and `recall`.
///
/// Every option beyond `q` is optional, so a plain `?q=word` still means what
/// it always did. Query strings carry no lists, so `tags` is comma-separated.
#[derive(Debug, Deserialize)]
struct SearchParams {
    /// `query` is accepted as an alias: MCP and the CLI call this `query`, and
    /// a REST caller guessing the same name should not get a missing-parameter
    /// error for it.
    #[serde(alias = "query")]
    q: String,
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to one wing by name (see `AppServices::search`).
    wing: Option<String>,
    /// Restrict results to one room by name. Ignored by `recall`.
    room: Option<String>,
    /// `auto` (default), `lexical`, `semantic` or `hybrid`.
    ranking: Option<String>,
    /// Comma-separated tags a drawer must all carry.
    #[serde(alias = "tag")]
    tags: Option<String>,
    /// Restrict to drawers from one source kind: `file`, `manual`, `transcript` or `other`.
    source_kind: Option<String>,
    /// An RFC 3339 instant: search the memory valid then.
    as_of: Option<String>,
    /// Include superseded memory as well as current.
    #[serde(default)]
    include_historical: bool,
    /// Enrich the hits through the knowledge graph.
    #[serde(default)]
    expand: bool,
}

fn default_search_limit() -> u32 {
    crate::app::DEFAULT_SEARCH_LIMIT
}

impl SearchParams {
    fn into_query(self) -> Result<SearchQuery, crate::Error> {
        SearchOptions {
            limit: Some(self.limit),
            wing: self.wing,
            room: self.room,
            ranking: self.ranking,
            tags: self
                .tags
                .iter()
                .flat_map(|tags| tags.split(','))
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect(),
            source_kind: self.source_kind,
            as_of: self.as_of,
            include_historical: self.include_historical,
            expand: self.expand,
        }
        .into_query(self.q)
    }
}

async fn search(
    State(state): State<ApiState>,
    ApiQuery(params): ApiQuery<SearchParams>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.search(params.into_query()?, mode).await?))
}

/// `POST /api/search`: the same search as a JSON [`SearchQuery`], which is the
/// only way to send a `query_embedding` (a vector does not fit a query string).
async fn search_json(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(query): ApiJson<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.search(query, mode).await?))
}

async fn recall(
    State(state): State<ApiState>,
    ApiQuery(params): ApiQuery<SearchParams>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.recall(params.into_query()?, mode).await?))
}

/// `POST /api/recall`: [`search_json`]'s counterpart for recall.
async fn recall_json(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(query): ApiJson<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.recall(query, mode).await?))
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
    crate::domain::channel::HTTP.to_string()
}

/// Deserialize a job submission, naming a bad job id or checkpoint payload the
/// way MCP and the CLI do.
///
/// Letting serde reject the whole body would report both as
/// `memcastle::input::invalid` on `body`, while the other two channels say
/// the shared job-id diagnostic and `payload`: the same mistake with a
/// different diagnostic per channel. The two checks run on the raw JSON first,
/// so those cases get the shared diagnostic and everything else still falls
/// through to the single serde pass below.
fn parse_submit_body(raw: &serde_json::Value) -> Result<SubmitJobBody, crate::Error> {
    match raw.get("type").and_then(serde_json::Value::as_str) {
        Some("repair") => {
            if let Some(id) = raw.get("based_on_job").and_then(serde_json::Value::as_str) {
                crate::Error::parse_job_id(id)?;
            }
        }
        Some("checkpoint") => {
            if let Some(payload) = raw.get("payload") {
                serde_json::from_value::<crate::domain::CheckpointPayload>(payload.clone())
                    .map_err(|source| crate::Error::invalid_input("payload", source.to_string()))?;
            }
        }
        _ => {}
    }
    serde_json::from_value(raw.clone())
        .map_err(|source| crate::Error::invalid_input("body", source.to_string()))
}

async fn submit_job(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(raw): ApiJson<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    let body = parse_submit_body(&raw)?;
    let job = match body.kind {
        JobKind::Mine { source, wing, full } => {
            // Gated as a write inside `submit_mine`: mining files drawers.
            state
                .app
                .submit_mine(source, wing, full, &body.requested_by, mode)
                .await?
        }
        JobKind::Demo { steps } => state.app.submit_demo(steps, &body.requested_by).await?,
        JobKind::Checkpoint { payload } => {
            state
                .app
                .submit_checkpoint_with_urgency(payload, body.emergency, &body.requested_by, mode)
                .await?
        }
        JobKind::Audit { scope } => {
            // Not gated by `mode` — same reasoning as `Mine` above (see
            // `AppServices::submit_audit`'s doc comment).
            state.app.submit_audit(scope, &body.requested_by).await?
        }
        JobKind::Embed { wing } => {
            // Gated as a write inside `submit_embed`: it fills every drawer's embedding.
            state
                .app
                .submit_embed(wing, &body.requested_by, mode)
                .await?
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

/// The sources this daemon can mine and the ones it has mined (`memcastle sources`).
async fn list_sources(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.list_sources(mode).await?))
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
    // The answer says "requested", not "paused": pausing is cooperative, so the
    // job stops at its next check (every handler has one, audit and repair
    // included) rather than at the instant of the request.
    Ok(Json(state.app.pause_job(parse_job_id(&id)?).await?))
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
    Ok(crate::Error::parse_job_id(raw)?)
}

fn parse_status(raw: &str) -> Result<JobStatus, ApiError> {
    Ok(crate::Error::parse_job_status(raw)?)
}
