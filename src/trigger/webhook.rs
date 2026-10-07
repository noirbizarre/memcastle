//! The `webhook` trigger: the daemon's own listener for calls from an external service.
//!
//! This is a separate socket from the REST API and the MCP endpoint, for the same reason the database admin endpoint is
//! one (docs/adr/015): the daemon's token authenticates *clients*, while a webhook's sender can only be given a shared
//! secret for its own trigger, so the two must never be confused, and every route on the main router stays behind the
//! daemon's authentication layer (invariant 6). The listener is opened only while a webhook trigger is enabled, binds
//! loopback unless the operator opted in, and is bounded in body size and concurrency.
//!
//! A delivery proves itself with the trigger's secret (an HMAC-SHA-256 of the raw body, or the secret itself in a
//! header), compared in constant time. Whatever is wrong (an unknown trigger, a disabled one, a missing secret, a bad or
//! absent signature) is the same empty `401`, so the listener does not say which trigger names exist. The body is only
//! hashed to check the signature: it is never stored, logged or passed on, because a trigger says *that* something
//! changed, and the source decides what.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::domain::{
    FireOutcome, SignatureEncoding, TriggerDefinition, TriggerMechanism, WebhookAuth, WebhookPlan,
};

use super::{FireRequest, Host, Listener, ListenerSettings};

/// The longest delivery id kept: a header is untrusted input, and the id becomes part of a stored record's key.
const MAX_DELIVERY_ID_LEN: usize = 256;

/// One enabled webhook trigger, as the listener sees it.
pub(super) struct Hook {
    pub definition: TriggerDefinition,
    pub plan: WebhookPlan,
}

/// The enabled webhook triggers by name, replaced by the supervisor whenever the configuration changes.
pub(super) type Routes = Arc<RwLock<HashMap<String, Arc<Hook>>>>;

struct Context<H: Host> {
    host: H,
    routes: Routes,
    /// One permit per delivery being worked on; a burst past it is told to retry, not queued without bound.
    permits: Semaphore,
}

/// Bind the listener and serve it until cancelled.
pub(super) async fn bind<H: Host>(
    host: &H,
    settings: ListenerSettings,
    routes: &Routes,
) -> Result<Listener, String> {
    let socket = tokio::net::TcpListener::bind(settings.addr)
        .await
        .map_err(|e| {
            format!(
                "cannot listen on {}: {e}; set `webhook.port` to a free port",
                settings.addr
            )
        })?;
    // The real port: with `0` the OS chose it.
    let addr = socket
        .local_addr()
        .map_err(|e| format!("cannot read the listener's address: {e}"))?;
    let context = Arc::new(Context {
        host: host.clone(),
        routes: Arc::clone(routes),
        permits: Semaphore::new(settings.max_concurrent),
    });
    let router = Router::new()
        .route("/hooks/{name}", post(deliver::<H>))
        .layer(DefaultBodyLimit::max(settings.max_body_bytes))
        .with_state(context);
    let cancel = CancellationToken::new();
    let handle = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            let served = axum::serve(socket, router)
                .with_graceful_shutdown(cancel.cancelled_owned())
                .await;
            if let Err(error) = served {
                warn!(%error, "the webhook listener stopped unexpectedly");
            }
        }
    });
    Ok(Listener {
        settings,
        addr,
        cancel,
        handle,
    })
}

/// The answer to a delivery that is not accepted for any reason the sender should not learn.
fn unauthorized() -> Response {
    StatusCode::UNAUTHORIZED.into_response()
}

async fn deliver<H: Host>(
    State(context): State<Arc<Context<H>>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Taken before any work, and released when the response is built: the limit is on work in flight.
    let Ok(_permit) = context.permits.try_acquire() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "1")],
            "too many deliveries in flight; retry shortly",
        )
            .into_response();
    };
    let hook = context
        .routes
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&name)
        .cloned();
    let Some(hook) = hook else {
        debug!(trigger = %name, "a delivery for a trigger that is not enabled");
        return unauthorized();
    };
    let Some(secret) = context.host.secret(&hook.definition).await else {
        warn!(trigger = %name, "a delivery arrived but the trigger's secret cannot be read; refused");
        return unauthorized();
    };
    if !verify(&hook.plan, &secret, &headers, &body) {
        debug!(trigger = %name, "a delivery failed authentication");
        return unauthorized();
    }
    let delivery = hook
        .plan
        .delivery_header
        .as_deref()
        .and_then(|header| headers.get(header))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= MAX_DELIVERY_ID_LEN)
        .map(str::to_string);
    let outcome = context
        .host
        .fire(FireRequest {
            trigger: name.clone(),
            via: Some(TriggerMechanism::Webhook),
            delivery,
        })
        .await;
    match outcome {
        Ok(FireOutcome::Queued { job }) => accepted("queued", Some(job.to_string())),
        Ok(FireOutcome::Coalesced { job }) => accepted("coalesced", Some(job.to_string())),
        Ok(FireOutcome::Duplicate) => accepted("duplicate", None),
        Err(reason) => {
            // The sender is authenticated but is not the operator: it learns that the run was not queued, and the
            // daemon's log says why.
            warn!(trigger = %name, %reason, "a delivery was authentic but no run could be queued");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "30")],
                "the delivery was accepted but a run could not be queued",
            )
                .into_response()
        }
    }
}

fn accepted(status: &str, job: Option<String>) -> Response {
    let body = serde_json::json!({ "status": status, "job": job });
    (StatusCode::ACCEPTED, axum::Json(body)).into_response()
}

/// Whether a delivery proves itself with `secret`.
///
/// For `hmac-sha256` the header carries the HMAC-SHA-256 of the raw body keyed with the secret, after the plan's
/// prefix (`sha256=`) and encoded as hex or base64. For `token` it carries the secret itself. Both are compared in
/// constant time, and a missing or malformed header is simply not authentic.
#[must_use]
pub fn verify(plan: &WebhookPlan, secret: &[u8], headers: &HeaderMap, body: &[u8]) -> bool {
    if secret.is_empty() {
        // An empty secret authenticates nobody: any sender could "know" it.
        return false;
    }
    let Some(presented) = headers
        .get(plan.header.as_str())
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let presented = presented.trim();
    let Some(presented) = presented.strip_prefix(plan.prefix.as_str()) else {
        return false;
    };
    match plan.auth {
        WebhookAuth::Token => {
            // Both sides hashed first, so the comparison is over equal lengths whatever was sent.
            let given = Sha256::digest(presented.trim().as_bytes());
            let wanted = Sha256::digest(secret);
            given.ct_eq(&wanted).into()
        }
        WebhookAuth::HmacSha256 => {
            let Some(signature) = decode(plan.encoding, presented.trim()) else {
                return false;
            };
            let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(secret) else {
                return false;
            };
            mac.update(body);
            // `verify_slice` compares in constant time.
            mac.verify_slice(&signature).is_ok()
        }
    }
}

fn decode(encoding: SignatureEncoding, text: &str) -> Option<Vec<u8>> {
    match encoding {
        SignatureEncoding::Base64 => STANDARD.decode(text).ok(),
        SignatureEncoding::Hex => {
            if !text.len().is_multiple_of(2) || !text.is_ascii() {
                return None;
            }
            (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn plan(auth: WebhookAuth, encoding: SignatureEncoding, prefix: &str) -> WebhookPlan {
        WebhookPlan {
            auth,
            header: "x-sig".to_string(),
            prefix: prefix.to_string(),
            encoding,
            delivery_header: None,
        }
    }

    fn headers(value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert("x-sig", HeaderValue::from_str(value).unwrap());
        map
    }

    fn sign(secret: &[u8], body: &[u8]) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).unwrap();
        mac.update(body);
        mac.finalize().into_bytes().to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn the_rfc_4231_hmac_vector_verifies_and_a_changed_body_does_not() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?".
        let signature = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
        let plan = plan(WebhookAuth::HmacSha256, SignatureEncoding::Hex, "");
        let body = b"what do ya want for nothing?";
        assert!(verify(&plan, b"Jefe", &headers(signature), body));
        assert!(!verify(
            &plan,
            b"Jefe",
            &headers(signature),
            b"what do ya want for something?"
        ));
        assert!(
            !verify(&plan, b"jefe", &headers(signature), body),
            "a different key"
        );
    }

    #[test]
    fn a_prefixed_hex_signature_and_a_base64_one_both_verify() {
        let body = b"{\"event\":\"push\"}";
        let mac = sign(b"s3cret-value", body);
        let github = plan(WebhookAuth::HmacSha256, SignatureEncoding::Hex, "sha256=");
        assert!(verify(
            &github,
            b"s3cret-value",
            &headers(&format!("sha256={}", hex(&mac))),
            body
        ));
        assert!(
            !verify(&github, b"s3cret-value", &headers(&hex(&mac)), body),
            "the prefix is required"
        );
        let base64 = plan(WebhookAuth::HmacSha256, SignatureEncoding::Base64, "");
        assert!(verify(
            &base64,
            b"s3cret-value",
            &headers(&STANDARD.encode(&mac)),
            body
        ));
    }

    #[test]
    fn a_missing_malformed_or_truncated_signature_is_not_authentic() {
        let plan = plan(WebhookAuth::HmacSha256, SignatureEncoding::Hex, "");
        let body = b"x";
        let good = hex(&sign(b"k", body));
        assert!(!verify(&plan, b"k", &HeaderMap::new(), body), "no header");
        assert!(!verify(&plan, b"k", &headers("not hex"), body));
        assert!(
            !verify(&plan, b"k", &headers(&good[..good.len() - 2]), body),
            "truncated"
        );
        assert!(!verify(&plan, b"k", &headers(""), body));
        assert!(
            !verify(&plan, b"k", &headers("é"), body),
            "non-ascii must not panic"
        );
    }

    #[test]
    fn a_token_is_compared_whole_and_an_empty_secret_authenticates_nobody() {
        let plan = plan(WebhookAuth::Token, SignatureEncoding::Hex, "");
        assert!(verify(
            &plan,
            b"shared-token",
            &headers("shared-token"),
            b""
        ));
        assert!(!verify(
            &plan,
            b"shared-token",
            &headers("shared-toke"),
            b""
        ));
        assert!(!verify(
            &plan,
            b"shared-token",
            &headers("shared-token-and-more"),
            b""
        ));
        assert!(!verify(&plan, b"", &headers(""), b""), "an empty secret");
    }
}
