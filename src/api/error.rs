//! Maps `crate::Error` to HTTP status codes.

use crate::Error;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

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
            Error::JobNotFound { .. } | Error::RelationshipNotFound { .. } => StatusCode::NOT_FOUND,
            Error::InvalidJobTransition { .. }
            | Error::InvalidInput { .. }
            | Error::InvalidJobId { .. }
            | Error::InvalidBasedOnJob { .. }
            | Error::EmptyLabel { .. } => StatusCode::BAD_REQUEST,
            Error::ModeForbidden { .. } => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = self.0.body();
        let code = body.code.as_deref().unwrap_or("-");
        if status.is_server_error() {
            // A 5xx is the daemon's fault, not the caller's, and the client
            // only sees the message: without this the cause of an internal
            // error would exist nowhere an operator can read it.
            tracing::error!(error = %self.0, %status, code, "request failed");
        } else {
            // A rejected request is the caller's mistake, but an operator
            // asking "why does this integration keep getting refused?" has
            // nowhere else to look; `warn`, not `error`, because the daemon
            // did exactly what it should.
            tracing::warn!(error = %self.0, %status, code, "request rejected");
        }
        // The same body MCP reports (`Error::body`): `error` for callers that
        // only read a message, `code` and `help` so a remote failure is
        // exactly as actionable as one rendered locally.
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `MakeWriter` that keeps everything logged, so a test can read it.
    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn logged_while_responding(error: Error) -> String {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let _ = ApiError::from(error).into_response();
        });
        String::from_utf8(captured.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn a_rejected_request_leaves_a_warning_naming_its_code() {
        let log = logged_while_responding(Error::invalid_job_id("nope"));

        assert!(
            log.contains("WARN"),
            "a caller's mistake is a warning: {log}"
        );
        let code = Error::invalid_job_id("nope").body().code.unwrap();
        assert!(log.contains(&code), "{log}");
    }

    #[test]
    fn a_server_failure_leaves_an_error_line_naming_its_code() {
        let log = logged_while_responding(Error::server("boom"));

        assert!(
            log.contains("ERROR"),
            "the daemon's own fault is an error: {log}"
        );
        assert!(log.contains("memcastle::server::failed"), "{log}");
    }
}
