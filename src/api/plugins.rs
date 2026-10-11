//! Plugin metadata and preflight via the same application services used by the daemon.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::extract::{ApiJson, ApiQuery};
use super::{ApiError, ApiState};

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    #[serde(default)]
    q: String,
    registry: Option<String>,
}

/// `GET /api/plugin-registry/search` reads configured catalogues on demand.
pub(super) async fn search(
    State(state): State<ApiState>,
    ApiQuery(query): ApiQuery<SearchQuery>,
) -> impl IntoResponse {
    Json(
        state
            .app
            .search_plugins(&query.q, query.registry.as_deref())
            .await,
    )
}

#[derive(Deserialize)]
pub(super) struct PreviewQuery {
    version: Option<String>,
    registry: Option<String>,
}

/// `GET /api/plugin-registry/plugins/{id}` verifies a release before obtaining consent.
pub(super) async fn registry_preview(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    ApiQuery(query): ApiQuery<PreviewQuery>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .preview_registry_plugin(&id, query.version.as_deref(), query.registry.as_deref())
            .await?,
    ))
}

#[derive(Deserialize)]
pub(super) struct RegistryInstall {
    id: String,
    version: Option<String>,
    registry: Option<String>,
    #[serde(default)]
    consents: BTreeMap<String, String>,
    #[serde(default)]
    adopt_sources: Vec<String>,
    archive_digest: Option<String>,
}

/// `POST /api/plugin-registry/install` fetches only the reviewed plugin package.
pub(super) async fn registry_install(
    State(state): State<ApiState>,
    ApiJson(request): ApiJson<RegistryInstall>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .install_registry_plugin(
                &request.id,
                request.version.as_deref(),
                request.registry.as_deref(),
                request.consents,
                request.adopt_sources,
                request.archive_digest.as_deref(),
            )
            .await?,
    ))
}

/// `GET /api/plugins` lists installed plugin versions and inventories.
pub(super) async fn list(State(state): State<ApiState>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.list_plugins().await?))
}

/// `GET /api/plugins/{id}`.
pub(super) async fn show(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.show_plugin(&id).await?))
}

/// `DELETE /api/plugins/{id}` refuses active modules and retains mined data.
pub(super) async fn uninstall(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    state.app.uninstall_plugin(&id).await?;
    Ok(Json(serde_json::json!({ "uninstalled": id })))
}

/// `POST /api/plugins/preview` validates an archive without installing its modules.
pub(super) async fn preview(
    State(state): State<ApiState>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.preview_plugin_archive(&body).await?))
}

/// `POST /api/plugins`: install a complete archive, with individually reviewed consent digests.
pub(super) async fn install(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let consents: BTreeMap<String, String> = match headers.get("x-memcastle-consents") {
        Some(raw) => serde_json::from_slice(raw.as_bytes()).map_err(|error| {
            crate::Error::invalid_input(
                "X-MemCastle-Consents",
                format!("expected a JSON map of module IDs to digests: {error}"),
            )
        })?,
        None => BTreeMap::new(),
    };
    let adopt_sources: Vec<String> = match headers.get("x-memcastle-adopt-sources") {
        Some(raw) => serde_json::from_slice(raw.as_bytes()).map_err(|error| {
            crate::Error::invalid_input(
                "X-MemCastle-Adopt-Sources",
                format!("expected a JSON list of source IDs: {error}"),
            )
        })?,
        None => Vec::new(),
    };
    Ok(Json(
        state
            .app
            .install_plugin_archive(body.to_vec(), consents, adopt_sources, None, None)
            .await?,
    ))
}
