//! Maps `crate::Error` to HTTP status codes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::Error;

/// A thin wrapper so `crate::Error` (which has no opinion about HTTP) can
/// implement `IntoResponse` without that impl living in `error.rs` — the
/// library's error type shouldn't depend on axum.
pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::JobNotFound { .. } => StatusCode::NOT_FOUND,
            Error::InvalidJobTransition { .. } | Error::Config { .. } => StatusCode::BAD_REQUEST,
            Error::ModeForbidden { .. } => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(json!({ "error": self.0.to_string() }))).into_response()
    }
}
