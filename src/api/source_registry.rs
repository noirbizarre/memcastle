//! Source registries over REST: search, preview, install and update (docs/adr/033).
//!
//! Administrative and deliberately REST-only, like installing from a file: there is no MCP tool, so an agent
//! integration can neither make the daemon fetch from a registry nor install what it finds. The routes sit behind the
//! same authentication layer as everything else.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

use crate::app::RegistryInstall;

use super::extract::{ApiJson, ApiQuery};
use super::{ApiError, ApiState};

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    /// Part of a name or description; absent lists everything.
    q: Option<String>,
    /// Consult only this index.
    registry: Option<String>,
}

/// `GET /api/source-registry/search`.
pub(super) async fn search(
    State(state): State<ApiState>,
    ApiQuery(query): ApiQuery<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .search_registry(query.q.as_deref(), query.registry.as_deref())
            .await?,
    ))
}

#[derive(Deserialize)]
pub(super) struct PreviewQuery {
    version: Option<String>,
    registry: Option<String>,
}

/// `GET /api/source-registry/sources/{name}`: what installing it would do, with nothing installed.
pub(super) async fn preview(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    ApiQuery(query): ApiQuery<PreviewQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .preview_registry_source(&name, query.version.as_deref(), query.registry.as_deref())
            .await?,
    ))
}

/// `POST /api/source-registry/install`.
pub(super) async fn install(
    State(state): State<ApiState>,
    ApiJson(request): ApiJson<RegistryInstall>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.install_registry_source(request).await?))
}

/// `GET /api/source-registry/updates`.
pub(super) async fn updates(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.check_source_updates().await?))
}

#[derive(Deserialize)]
pub(super) struct UpdateBody {
    /// One source; absent updates every source that has an update.
    #[serde(default)]
    name: Option<String>,
    /// The digest of the permissions the user agreed to, for a source whose permissions changed.
    #[serde(default)]
    consent: Option<String>,
}

/// `POST /api/source-registry/update`.
pub(super) async fn update(
    State(state): State<ApiState>,
    ApiJson(body): ApiJson<UpdateBody>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .update_sources(body.name.as_deref(), body.consent.as_deref())
            .await?,
    ))
}
