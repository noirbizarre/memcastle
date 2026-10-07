//! Trigger configuration over REST: list, inspect, create or change, enable, disable, remove, reload and fire
//! (`docs/adr/043-source-triggers.md`).
//!
//! Reading is also offered to MCP (`memcastle_trigger_list`, `memcastle_trigger_get`); everything that changes a
//! trigger is REST and CLI only, so an agent can see what runs unattended but not decide it. The routes sit behind the
//! same authentication layer as everything else: a *webhook delivery* is not one of them. It goes to the separate,
//! opt-in listener the supervisor owns, which is why this module has no route for it.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};

use super::extract::ApiJson;
use super::{ApiError, ApiState, ModeHeader};
use crate::app::TriggerPatch;

/// `GET /api/triggers`.
pub(super) async fn list(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.list_triggers(mode).await?))
}

/// `GET /api/triggers/{name}`.
pub(super) async fn show(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.show_trigger(&name, mode).await?))
}

/// `PUT /api/triggers/{name}`: create the trigger (disabled), or change the fields the body names.
pub(super) async fn set(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    ApiJson(patch): ApiJson<TriggerPatch>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_trigger(&name, patch).await?))
}

/// `POST /api/triggers/{name}/enable`.
pub(super) async fn enable(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_trigger_enabled(&name, true).await?))
}

/// `POST /api/triggers/{name}/disable`.
pub(super) async fn disable(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_trigger_enabled(&name, false).await?))
}

/// `DELETE /api/triggers/{name}`.
pub(super) async fn remove(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state.app.remove_trigger(&name).await?;
    Ok(Json(serde_json::json!({ "removed": name })))
}

/// `POST /api/triggers/reload`.
pub(super) async fn reload(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.reload_triggers().await?))
}

/// `POST /api/triggers/{name}/fire`: ask for a run now, the same way every other firing does.
pub(super) async fn fire(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.fire_trigger(&name, mode).await?))
}
