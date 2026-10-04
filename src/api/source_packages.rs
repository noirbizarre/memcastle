//! Installed sources over REST: install, inspect, enable, disable and remove (docs/adr/026).
//!
//! Administrative and deliberately REST-only, like the token and the database endpoint: there is no MCP tool, so an
//! agent integration cannot install code or widen what a source may do. The routes sit behind the same
//! authentication layer as everything else.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

use super::extract::ApiQuery;
use super::{ApiError, ApiState, ModeHeader};

/// The most a package upload may be: a component is megabytes, and this bounds a mistake or an attack.
pub(super) const MAX_PACKAGE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Deserialize)]
pub(super) struct InstallQuery {
    /// The digest of the permissions the user agreed to.
    consent: Option<String>,
    /// Turn the source on once installed.
    #[serde(default)]
    enable: bool,
}

/// `POST /api/source-packages`: the body is the package archive itself.
pub(super) async fn install(
    State(state): State<ApiState>,
    ApiQuery(query): ApiQuery<InstallQuery>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .install_source_package(body.to_vec(), query.consent.as_deref(), query.enable)
            .await?,
    ))
}

/// `GET /api/source-packages/{name}`.
pub(super) async fn show(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.show_source(&name, mode).await?))
}

/// `POST /api/source-packages/{name}/enable`.
pub(super) async fn enable(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_source_enabled(&name, true).await?))
}

/// `POST /api/source-packages/{name}/disable`.
pub(super) async fn disable(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.set_source_enabled(&name, false).await?))
}

/// `DELETE /api/source-packages/{name}`.
pub(super) async fn remove(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state.app.remove_source_package(&name).await?;
    Ok(Json(serde_json::json!({ "removed": name })))
}
