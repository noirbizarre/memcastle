//! Authentication at the HTTP edge: the layer that guards every route, and the
//! two administrative routes that manage the generated token.
//!
//! The layer wraps the *whole* router, `/mcp` included, so there is no route
//! that can be added later and forgotten: everything is guarded except the one
//! path named in [`is_public`]. It runs before rmcp sees a request, so an MCP
//! `initialize` needs the token like any tool call.

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL};
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};

use super::{ApiError, ApiState};
use crate::app::AppServices;
use crate::error::Error;

/// The one request that never needs a token: the liveness probe.
///
/// `memcastle daemon restart`, supervisors and the client's own "is a daemon there?"
/// check poll it, and it answers only `{"status":"ok"}`. Keeping it open also
/// keeps "the daemon is down" (no answer) distinct from "you are not
/// authenticated" (a 401).
fn is_public(request: &Request) -> bool {
    request.method() == Method::GET && request.uri().path() == "/api/health"
}

/// The bearer token in `headers`, `Ok(None)` when there is no `Authorization`
/// header at all.
///
/// # Errors
///
/// [`Error::Unauthorized`] (`malformed`) for a header that is not valid text,
/// is not the `Bearer` scheme, or carries an empty token. The error names the
/// problem and never echoes the header.
fn bearer_token(headers: &HeaderMap) -> Result<Option<&str>, Error> {
    let Some(value) = headers.get(AUTHORIZATION) else {
        return Ok(None);
    };
    let malformed = || Error::Unauthorized {
        reason: "malformed",
    };
    let text = value.to_str().map_err(|_| malformed())?;
    let (scheme, token) = text.split_once(' ').ok_or_else(malformed)?;
    // The scheme name is case-insensitive (RFC 9110 section 11.1).
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return Err(malformed());
    }
    Ok(Some(token))
}

/// Refuse any request that does not carry a valid token, when authentication
/// is enabled. A no-op when it is not, which is what keeps the standalone
/// workflow (and the bootstrap `auth generate`) working without a token.
pub async fn require_auth(
    State(app): State<AppServices>,
    request: Request,
    next: Next,
) -> Response {
    if !app.auth_enabled() || is_public(&request) {
        return next.run(request).await;
    }
    let outcome = match bearer_token(request.headers()) {
        Ok(presented) => app.authenticate(presented).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(()) => next.run(request).await,
        // Fails closed: a store error while reading the verifier is also a
        // refusal (a 500), never a pass.
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// `POST /api/auth/token`: generate a token, replacing any previous one.
///
/// Open while authentication is disabled (the bootstrap) and behind
/// [`require_auth`] once it is enabled. Never exposed over MCP.
pub(super) async fn generate_token(State(state): State<ApiState>) -> Result<Response, ApiError> {
    let generated = state.app.generate_token().await?;
    let mut response = Json(generated).into_response();
    // The only response that ever carries a plaintext token: keep every cache
    // between here and the terminal from storing it.
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// `DELETE /api/auth/token`: revoke the generated token.
pub(super) async fn revoke_token(State(state): State<ApiState>) -> Result<Response, ApiError> {
    Ok(Json(state.app.revoke_token().await?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn a_request_without_an_authorization_header_carries_no_token() {
        assert_eq!(bearer_token(&HeaderMap::new()).unwrap(), None);
    }

    #[test]
    fn a_bearer_header_yields_its_token_whatever_the_scheme_case() {
        assert_eq!(bearer_token(&headers("Bearer abc")).unwrap(), Some("abc"));
        assert_eq!(bearer_token(&headers("bearer abc")).unwrap(), Some("abc"));
    }

    #[test]
    fn a_non_bearer_or_empty_credential_is_malformed_not_missing() {
        for value in ["Basic abc", "Bearer", "Bearer  ", "abc"] {
            assert!(
                matches!(
                    bearer_token(&headers(value)),
                    Err(Error::Unauthorized {
                        reason: "malformed"
                    })
                ),
                "{value:?}"
            );
        }
    }
}
