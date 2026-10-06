//! Miner configuration over REST: list, inspect, create or change, enable, disable, remove, reload and run
//! (`docs/adr/037-persistent-miner-configuration.md`).
//!
//! Reading is also offered to MCP (`memcastle_miner_list`, `memcastle_miner_get`); everything that changes a miner is
//! REST and CLI only, like installing a source, so an agent can see what is configured but not widen what the daemon
//! mines. The routes sit behind the same authentication layer as everything else.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

use super::extract::ApiJson;
use super::{ApiError, ApiState, ModeHeader, default_requested_by};
use crate::app::MinerPatch;

/// `GET /api/miners`.
pub(super) async fn list(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.list_miners(mode).await?))
}

/// `GET /api/miners/{name}`.
pub(super) async fn show(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.show_miner(&name, mode).await?))
}

/// `PUT /api/miners/{name}`: create the miner, or change the fields the body names.
pub(super) async fn set(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    ApiJson(patch): ApiJson<MinerPatch>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_miner(&name, patch).await?))
}

/// `POST /api/miners/{name}/enable`.
pub(super) async fn enable(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_miner_enabled(&name, true).await?))
}

/// `POST /api/miners/{name}/disable`.
pub(super) async fn disable(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_miner_enabled(&name, false).await?))
}

/// `DELETE /api/miners/{name}`.
pub(super) async fn remove(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state.app.remove_miner(&name).await?;
    Ok(Json(serde_json::json!({ "removed": name })))
}

/// `POST /api/miners/reload`.
pub(super) async fn reload(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.reload_miners().await?))
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RunBody {
    /// Ignore the cursor and read the source from the beginning.
    full: bool,
    /// Who asked, recorded on the job; `"http"` unless a caller says otherwise.
    requested_by: String,
}

impl Default for RunBody {
    fn default() -> Self {
        Self {
            full: false,
            requested_by: default_requested_by(),
        }
    }
}

/// `POST /api/miners/{name}/run`: the body is optional.
pub(super) async fn run(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(name): Path<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    // Read by hand: an empty body means "the defaults", which the JSON extractor would reject as a missing body.
    let body: RunBody = if body.iter().all(u8::is_ascii_whitespace) {
        RunBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|source| crate::Error::invalid_input("body", source.to_string()))?
    };
    Ok(Json(
        state
            .app
            .run_miner(&name, body.full, &body.requested_by, mode)
            .await?,
    ))
}
