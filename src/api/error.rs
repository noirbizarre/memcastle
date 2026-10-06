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
            Error::JobNotFound { .. }
            | Error::RelationshipNotFound { .. }
            | Error::EntityNotFound { .. }
            | Error::WingNotFound { .. }
            | Error::RoomNotFound { .. }
            | Error::DrawerNotFound { .. }
            | Error::SourceNotFound { .. }
            | Error::MinerNotFound { .. }
            // No registry offers it, or no version of it can be installed here.
            | Error::SourceNotInRegistry { .. } => StatusCode::NOT_FOUND,
            Error::InvalidJobTransition { .. }
            | Error::InvalidPalacePath { .. }
            | Error::InvalidInput { .. }
            | Error::InvalidJobId { .. }
            | Error::InvalidBasedOnJob { .. }
            | Error::EmptyLabel { .. }
            // The caller asked for a ranking this daemon cannot serve right now.
            | Error::SemanticUnavailable { .. }
            // A package, manifest or consent the caller supplied is theirs to fix.
            | Error::SourceManifestInvalid { .. }
            | Error::SourcePackageInvalid { .. }
            | Error::SourceIncompatible { .. }
            | Error::SourceConsentRequired { .. }
            | Error::SourceBuiltin { .. }
            // A miner definition the caller wrote is theirs to fix.
            | Error::MinerInvalid { .. }
            // The trust policy is the daemon's configuration, but the package was the caller's choice.
            | Error::SourceUntrusted { .. }
            | Error::EmbeddingsNotConfigured
            | Error::ExtractionNotConfigured => StatusCode::BAD_REQUEST,
            // The embedding provider is an upstream, not the caller and not us.
            Error::EmbeddingFailed { .. }
            | Error::EmbeddingDimension { .. }
            | Error::ExtractionFailed { .. }
            // A registry is an upstream too: unreachable, or serving something other than what it published.
            | Error::SourceRegistryUnavailable { .. }
            | Error::SourceIntegrity { .. } => StatusCode::BAD_GATEWAY,
            Error::ModeForbidden { .. } => StatusCode::FORBIDDEN,
            // 401, not 403: the caller is unidentified, which is different from
            // an identified caller being refused by its memory mode.
            Error::Unauthorized { .. } => StatusCode::UNAUTHORIZED,
            // The request is fine; the job's recorded state (Running, but with
            // no worker) is what conflicts with it, and a restart resolves it.
            // A 500 would blame the daemon for something the caller can fix.
            // A name already taken, or a delete that a running job would undo:
            // the request is well-formed but conflicts with the palace's state.
            // The same for a job that kept changing under the request: the
            // caller only has to retry.
            Error::JobOrphaned { .. }
            | Error::JobContended { .. }
            | Error::DbEndpointRunning { .. }
            // The address the caller asked for is taken, or not theirs to
            // bind: a different port fixes it, so it is not the daemon's fault.
            | Error::DbEndpointBind { .. }
            | Error::DrawerNameTaken { .. }
            | Error::DrawerSuperseded { .. }
            // The source exists, but its state (disabled, unavailable) conflicts with the request.
            | Error::SourceNotEnabled { .. }
            // A name already taken, a change that needs confirming, or a miner whose state forbids the request.
            // A hand-edited file that no longer parses is also a conflict: the request is fine, the file is not.
            | Error::MinerExists { .. }
            | Error::MinerScopeBroadened { .. }
            | Error::MinerDisabled { .. }
            | Error::MinerNotRunnable { .. }
            | Error::MinerConfigFile { .. }
            | Error::PalaceBusy { .. } => StatusCode::CONFLICT,
            // The caller asked for something the daemon will not do (an unsafe
            // bind, or an endpoint for a database it does not embed): theirs to fix.
            Error::DbEndpointUnsafe { .. } | Error::DbEndpointUnavailable { .. } => {
                StatusCode::BAD_REQUEST
            }
            // Startup/internal conditions (auth not configured, migration lock,
            // lost lease, missing assets) and client-side errors never reach a
            // request handler as the caller's fault, so 500 is the right default.
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
        let mut response = (status, Json(body)).into_response();
        if status == StatusCode::UNAUTHORIZED {
            // RFC 9110 requires a challenge on every 401; it also lets generic
            // HTTP clients tell this apart from any other refusal.
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
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
    fn an_orphaned_job_is_a_conflict_the_caller_can_resolve_not_a_server_fault() {
        let response = ApiError::from(Error::JobOrphaned { id: "x".into() }).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn a_contended_job_is_a_retryable_conflict_not_a_server_fault() {
        let response = ApiError::from(Error::job_contended("x")).into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn a_database_endpoint_bind_failure_is_a_conflict_the_caller_can_resolve() {
        let error = Error::DbEndpointBind {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 8000)),
            source: std::io::Error::from(std::io::ErrorKind::AddrInUse),
        };

        assert_eq!(
            ApiError::from(error).into_response().status(),
            StatusCode::CONFLICT
        );
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
