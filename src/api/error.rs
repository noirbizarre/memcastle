//! Maps `crate::Error` to HTTP status codes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use miette::Diagnostic;

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
            Error::InvalidJobTransition { .. }
            | Error::InvalidInput { .. }
            | Error::InvalidJobId { .. }
            | Error::InvalidBasedOnJob { .. }
            | Error::EmptyLabel { .. } => StatusCode::BAD_REQUEST,
            Error::ModeForbidden { .. } => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            // A 5xx is the daemon's fault, not the caller's, and the client
            // only sees the message: without this the cause of an internal
            // error would exist nowhere an operator can read it.
            tracing::error!(error = %self.0, %status, "request failed");
        }
        // `error` is kept for callers that only read a message; `code` and
        // `help` carry the same diagnostic the CLI would have rendered
        // locally, so a remote failure is exactly as actionable.
        let code = self.0.code().map(|code| code.to_string());
        let help = self.0.help().map(|help| help.to_string());
        (
            status,
            Json(json!({ "error": self.0.to_string(), "code": code, "help": help })),
        )
            .into_response()
    }
}
