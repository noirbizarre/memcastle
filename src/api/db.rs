//! The administrative routes that start, stop and report on the database admin
//! endpoint (`docs/adr/015`).
//!
//! REST-only, like the token routes: there is no MCP tool for them, so an agent
//! integration cannot open a database console. They sit behind
//! [`super::require_auth`] with everything else, which matters most for the
//! one that *starts* an endpoint onto the palace's database.

use axum::extract::State;
use axum::response::{IntoResponse, Json};

use super::ApiState;
use super::error::ApiError;
use super::extract::ApiJson;
use crate::app::DbEndpointRequest;

/// `GET /api/db`: whether the endpoint is listening, and where.
pub(super) async fn status(State(state): State<ApiState>) -> impl IntoResponse {
    Json(state.app.db_endpoint_status().await)
}

/// `POST /api/db`: start the endpoint. Refused when it would be unsafe; see
/// `AppServices::start_db_endpoint`.
pub(super) async fn start(
    State(state): State<ApiState>,
    ApiJson(request): ApiJson<DbEndpointRequest>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.start_db_endpoint(request).await?))
}

/// `DELETE /api/db`: stop the endpoint. Succeeds when it was not running.
pub(super) async fn stop(State(state): State<ApiState>) -> impl IntoResponse {
    Json(state.app.stop_db_endpoint().await)
}
