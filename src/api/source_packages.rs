//! Installed sources over REST: install, inspect, enable, disable and remove (docs/adr/026).
//!
//! Administrative and deliberately REST-only, like the token and the database endpoint: there is no MCP tool, so an
//! agent integration cannot install code or widen what a source may do. The routes sit behind the same
//! authentication layer as everything else.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Json, Response};
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

/// `POST /api/source-packages/{name}/auth`: start signing the source in, and say what the user must do.
///
/// Administrative and REST-only: a sign-in lets a source act on an account, so an agent cannot start one. The answer
/// holds the code the user types and the page to type it at, never a token, and is still kept out of every cache.
pub(super) async fn auth_begin(
    State(state): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let challenge = state.app.begin_source_auth(&name).await?;
    Ok(no_store(Json(challenge).into_response()))
}

#[derive(Deserialize)]
pub(super) struct WaitQuery {
    /// How long to hold the request open, in seconds; the daemon caps it.
    #[serde(default)]
    timeout: Option<u64>,
}

/// `POST /api/source-packages/{name}/auth/{flow}/wait`: a long poll for the end of a sign-in.
///
/// `{"status":"pending"}` means the user has not finished and the caller asks again; `signed_in` ends it, and a
/// failed sign-in is an error answer.
pub(super) async fn auth_wait(
    State(state): State<ApiState>,
    Path((name, flow)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<WaitQuery>,
) -> Result<Response, ApiError> {
    let window = std::time::Duration::from_secs(query.timeout.unwrap_or(25));
    let status = state.app.wait_source_auth(&name, &flow, window).await?;
    Ok(no_store(Json(status).into_response()))
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
