//! Request-body and query extractors whose rejections use the shared error body.
//!
//! axum's stock `Json` and `Query` reject a malformed request with a bare
//! plain-text 400/422. Every other failure in this API — and every MCP tool
//! error — is a JSON [`crate::error::ErrorBody`] with a diagnostic `code` and
//! a `help` line, so a caller (including `client::DaemonClient`, which reads
//! that body) would get an actionable message for a bad job id but only a
//! status line for a bad request body. These wrappers close that gap: the
//! same mistake now reports `memcastle::input::invalid` over REST as it does
//! over MCP.

use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::Error;

use super::error::ApiError;

/// Like `axum::Json` on the way in, but a body that fails to deserialize is
/// an [`Error::InvalidInput`] rather than axum's plain-text rejection.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::from(json_rejection(&rejection))),
        }
    }
}

/// Like `axum::extract::Query`, but a missing or malformed parameter is an
/// [`Error::InvalidInput`] rather than axum's plain-text rejection.
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::from(query_rejection(&rejection))),
        }
    }
}

fn json_rejection(rejection: &JsonRejection) -> Error {
    // `body_text` carries serde's own message (the unknown variant, the bad
    // UUID, the missing field), which is exactly what the caller must fix.
    Error::invalid_input("body", rejection.body_text())
}

fn query_rejection(rejection: &QueryRejection) -> Error {
    Error::invalid_input("query", rejection.body_text())
}
