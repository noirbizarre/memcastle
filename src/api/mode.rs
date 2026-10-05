//! Extracts the per-request [`MemoryMode`] from the `X-MemCastle-Mode`
//! header — see that type's doc comment for the allow/deny matrix this
//! feeds into. This is the *only* place HTTP decides what the header means;
//! every handler that needs a mode just takes [`ModeHeader`] as an extractor
//! and passes the inner value straight to `AppServices`, never branching on
//! it itself (see `app::AppServices`'s `require_read`/`require_write`).

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::Error;
use crate::domain::MemoryMode;

use super::error::ApiError;

/// The header name clients set to request a non-default memory mode.
/// Header lookup is case-insensitive (`axum`'s `HeaderMap` normalizes
/// names), so the lowercase spelling HTTP/2 uses is just the canonical one in
/// code; the documentation writes it `X-MemCastle-Mode`, and callers may send
/// either.
const HEADER_NAME: &str = MemoryMode::HEADER;

/// The effective [`MemoryMode`] for one HTTP request — `Full` when the
/// header is absent entirely (existing clients that don't send it must see
/// today's unrestricted behavior unchanged), a 400 [`Error::InvalidInput`] when
/// present but not one of `full`/`read_only`/`disabled`.
///
/// Deliberately never silently downgrades an unparsable value to `Full`:
/// a client that typoed a request for `disabled` must not be quietly
/// granted `Full` instead — that would be a safety regression disguised as
/// permissiveness, not a usability nicety.
pub struct ModeHeader(pub MemoryMode);

impl<S> FromRequestParts<S> for ModeHeader
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let Some(value) = parts.headers.get(HEADER_NAME) else {
            return Ok(Self(MemoryMode::Full));
        };
        let raw = value
            .to_str()
            .map_err(|_| ApiError::from(Error::invalid_input(HEADER_NAME, "must be ASCII")))?;
        let mode = raw.parse().map_err(|message: String| {
            ApiError::from(Error::invalid_input(HEADER_NAME, message))
        })?;
        Ok(Self(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    /// `ApiError` has no `Debug` impl — extract the mode with an explicit
    /// `match` rather than `.expect()`/`.unwrap()`, which both require
    /// `E: Debug`.
    async fn extract_ok(request: Request<Body>) -> MemoryMode {
        let (mut parts, _body) = request.into_parts();
        match ModeHeader::from_request_parts(&mut parts, &()).await {
            Ok(ModeHeader(mode)) => mode,
            Err(_) => panic!("expected extraction to succeed"),
        }
    }

    async fn extract_err(request: Request<Body>) -> bool {
        let (mut parts, _body) = request.into_parts();
        ModeHeader::from_request_parts(&mut parts, &())
            .await
            .is_err()
    }

    #[tokio::test]
    async fn a_missing_header_defaults_to_full() {
        let request = Request::builder().body(Body::empty()).unwrap();
        assert_eq!(extract_ok(request).await, MemoryMode::Full);
    }

    #[tokio::test]
    async fn a_read_only_header_value_parses() {
        let request = Request::builder()
            .header(HEADER_NAME, "read_only")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_ok(request).await, MemoryMode::ReadOnly);
    }

    #[tokio::test]
    async fn a_disabled_header_value_parses() {
        let request = Request::builder()
            .header(HEADER_NAME, "disabled")
            .body(Body::empty())
            .unwrap();
        assert_eq!(extract_ok(request).await, MemoryMode::Disabled);
    }

    #[tokio::test]
    async fn an_invalid_header_value_is_rejected_not_defaulted_to_full() {
        let request = Request::builder()
            .header(HEADER_NAME, "bogus")
            .body(Body::empty())
            .unwrap();
        assert!(
            extract_err(request).await,
            "an unparsable mode header must be an error, never silently `Full`"
        );
    }
}
