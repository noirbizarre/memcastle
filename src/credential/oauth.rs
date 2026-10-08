//! The OAuth 2.0 wire protocol, for a public client: the device grant (RFC 8628), the authorization code grant with
//! PKCE (RFC 7636) and the refresh grant (RFC 6749 section 6).
//!
//! Pure protocol: no storage, no flow state and no knowledge of any one provider. A source's manifest names the
//! endpoints and the client identifier; everything else here is what the specifications say. There is no client secret
//! anywhere, since a public client cannot keep one and a manifest is published.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::domain::{OAuthRequirement, Secret};

/// How long one request to a provider may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a provider request did not give a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthError {
    /// The provider answered, and the answer is no: a revoked or expired grant, a declined sign-in, a wrong code.
    /// Asking again with the same input will not change it.
    Rejected(String),
    /// The provider no longer honours the credential: a revoked or expired grant, or a client it does not know.
    /// The user has to sign in again.
    Revoked(String),
    /// The provider could not be reached or had a problem of its own. Asking again later may work.
    Unreachable(String),
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Revoked(message) | Self::Unreachable(message) => {
                f.write_str(message)
            }
        }
    }
}

/// What a provider handed back for a grant.
pub struct TokenSet {
    /// What a source presents to the provider's API.
    pub access_token: Secret,
    /// What obtains the next access token; absent when the provider issues none, or (on a refresh) keeps the old one.
    pub refresh_token: Option<Secret>,
    /// How long the access token lasts, when the provider says.
    pub expires_in: Option<u64>,
    /// What was granted, when the provider says.
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("expires_in", &self.expires_in)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

/// A device authorization: what the user is told to do, and what is polled with.
pub struct DeviceAuthorization {
    /// The secret the daemon polls with; the user never sees it.
    pub device_code: Secret,
    /// What the user types at the verification page.
    pub user_code: String,
    /// Where the user goes.
    pub verification_uri: String,
    /// The same page with the code filled in, when the provider offers one.
    pub verification_uri_complete: Option<String>,
    /// How long the code lasts, in seconds.
    pub expires_in: u64,
    /// The least time between polls, in seconds, as the provider asked (the broker applies its own floor).
    pub interval: u64,
}

/// One poll of a device authorization.
#[derive(Debug)]
pub enum Poll {
    /// The user has not finished.
    Pending,
    /// The user has not finished, and the provider asks for fewer requests.
    SlowDown,
    /// Signed in.
    Done(TokenSet),
}

/// The provider's JSON, whichever of a token or an error it is.
#[derive(Deserialize)]
struct Answer {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    // Google spells it this way.
    verification_url: Option<String>,
    verification_uri_complete: Option<String>,
    interval: Option<serde_json::Value>,
}

/// A number that a provider may send as a JSON number or as a string.
fn seconds(value: Option<&serde_json::Value>) -> Option<u64> {
    match value? {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A client for provider requests: bounded, and never following a redirect, which would carry a code or a token to a
/// host the manifest did not name.
///
/// # Errors
///
/// [`OAuthError::Unreachable`] when the client cannot be built.
pub fn client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("memcastle/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| OAuthError::Unreachable(format!("cannot build an HTTP client: {e}")))
}

/// POST `form` to `url` and read the JSON answer, mapping what is wrong with it.
async fn post(
    client: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<Answer, OAuthError> {
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|e| {
            OAuthError::Unreachable(format!("cannot reach {}: {}", host(url), e.without_url()))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|e| {
        OAuthError::Unreachable(format!(
            "{} did not answer in full: {}",
            host(url),
            e.without_url()
        ))
    })?;
    let answer: Option<Answer> = serde_json::from_str(&body).ok();
    // The provider's own words first: an error body is more useful than the status line, and some providers send an
    // error with a success status.
    if let Some(Answer {
        error: Some(code),
        error_description,
        ..
    }) = &answer
    {
        let said = error_description.as_deref().map_or_else(
            || code.clone(),
            |description| format!("{code}: {description}"),
        );
        return Err(match code.as_str() {
            "authorization_pending" | "slow_down" => OAuthError::Rejected(code.clone()),
            "temporarily_unavailable" | "server_error" => OAuthError::Unreachable(said),
            "invalid_grant" | "invalid_client" | "unauthorized_client" | "invalid_token" => {
                OAuthError::Revoked(said)
            }
            _ => OAuthError::Rejected(said),
        });
    }
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(OAuthError::Unreachable(format!(
            "{} answered {status}",
            host(url)
        )));
    }
    if !status.is_success() {
        return Err(OAuthError::Rejected(format!(
            "{} answered {status}",
            host(url)
        )));
    }
    answer.ok_or_else(|| OAuthError::Rejected(format!("{} did not answer with JSON", host(url))))
}

/// The host of `url`, for a message: the path and query of a token endpoint say nothing a person needs.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

fn tokens(answer: Answer) -> Result<TokenSet, OAuthError> {
    let access_token = answer
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            OAuthError::Rejected("the provider answered without an access token".to_string())
        })?;
    Ok(TokenSet {
        access_token: Secret::new(access_token),
        refresh_token: answer
            .refresh_token
            .filter(|token| !token.is_empty())
            .map(Secret::new),
        expires_in: seconds(answer.expires_in.as_ref()),
        scopes: answer
            .scope
            .map(|scope| scope.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

/// Ask the provider for a device code (RFC 8628 section 3.1).
///
/// # Errors
///
/// [`OAuthError`] when the provider refuses or cannot be reached, or does not answer as the specification says.
pub async fn start_device(
    client: &reqwest::Client,
    requirement: &OAuthRequirement,
) -> Result<DeviceAuthorization, OAuthError> {
    let url = requirement
        .device_authorization_url
        .as_deref()
        .ok_or_else(|| {
            OAuthError::Rejected("the source declares no device authorization endpoint".into())
        })?;
    let scope = requirement.scopes.join(" ");
    let mut form = vec![("client_id", requirement.client_id.as_str())];
    if !scope.is_empty() {
        form.push(("scope", scope.as_str()));
    }
    let answer = post(client, url, &form).await?;
    let missing =
        |what: &str| OAuthError::Rejected(format!("the provider's device answer has no {what}"));
    Ok(DeviceAuthorization {
        device_code: Secret::new(answer.device_code.ok_or_else(|| missing("device_code"))?),
        user_code: answer.user_code.ok_or_else(|| missing("user_code"))?,
        verification_uri: answer
            .verification_uri
            .or(answer.verification_url)
            .ok_or_else(|| missing("verification_uri"))?,
        verification_uri_complete: answer.verification_uri_complete,
        expires_in: seconds(answer.expires_in.as_ref()).unwrap_or(900),
        // RFC 8628 section 3.2: five seconds when the provider says nothing.
        interval: seconds(answer.interval.as_ref()).unwrap_or(5),
    })
}

/// Poll once for the user's decision (RFC 8628 section 3.4).
///
/// # Errors
///
/// [`OAuthError::Rejected`] when the user declined or the code expired, [`OAuthError::Unreachable`] when the provider
/// could not be reached.
pub async fn poll_device(
    client: &reqwest::Client,
    requirement: &OAuthRequirement,
    device: &DeviceAuthorization,
) -> Result<Poll, OAuthError> {
    let form = [
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", device.device_code.expose()),
        ("client_id", requirement.client_id.as_str()),
    ];
    match post(client, &requirement.token_url, &form).await {
        Ok(answer) => tokens(answer).map(Poll::Done),
        Err(OAuthError::Rejected(code)) if code == "authorization_pending" => Ok(Poll::Pending),
        Err(OAuthError::Rejected(code)) if code == "slow_down" => Ok(Poll::SlowDown),
        Err(OAuthError::Rejected(code)) if code.starts_with("access_denied") => {
            Err(OAuthError::Rejected("the sign-in was declined".to_string()))
        }
        Err(OAuthError::Rejected(code)) if code.starts_with("expired_token") => Err(
            OAuthError::Rejected("the code expired before the sign-in was finished".to_string()),
        ),
        Err(other) => Err(other),
    }
}

/// Exchange an authorization code for tokens (RFC 6749 section 4.1.3, with the PKCE verifier of RFC 7636).
///
/// # Errors
///
/// [`OAuthError`] when the provider refuses the code or cannot be reached.
pub async fn exchange_code(
    client: &reqwest::Client,
    requirement: &OAuthRequirement,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenSet, OAuthError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", requirement.client_id.as_str()),
        ("code_verifier", verifier),
    ];
    tokens(post(client, &requirement.token_url, &form).await?)
}

/// Obtain a new access token from a refresh token (RFC 6749 section 6).
///
/// The provider may rotate the refresh token; the caller keeps whatever comes back in place of the old one.
///
/// # Errors
///
/// [`OAuthError::Rejected`] when the provider no longer honours the refresh token (revoked, expired, issued under other
/// terms), [`OAuthError::Unreachable`] when it could not be reached.
pub async fn refresh(
    client: &reqwest::Client,
    requirement: &OAuthRequirement,
    refresh_token: &str,
) -> Result<TokenSet, OAuthError> {
    let scope = requirement.scopes.join(" ");
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", requirement.client_id.as_str()),
    ];
    if !scope.is_empty() {
        form.push(("scope", scope.as_str()));
    }
    tokens(post(client, &requirement.token_url, &form).await?)
}

/// A PKCE code verifier and its S256 challenge (RFC 7636 section 4).
pub struct Pkce {
    /// Kept by the daemon until the code comes back.
    pub verifier: String,
    /// Sent to the provider's authorization endpoint.
    pub challenge: String,
}

/// `len` random bytes as URL-safe base64, for a verifier or a `state`.
///
/// # Errors
///
/// A message when the operating system has no entropy to give.
pub fn random_token(len: usize) -> Result<String, String> {
    let mut bytes = vec![0_u8; len];
    getrandom::fill(&mut bytes).map_err(|e| format!("no source of randomness: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

impl Pkce {
    /// A fresh pair. 32 bytes gives the 43 characters RFC 7636 asks for at least.
    ///
    /// # Errors
    ///
    /// A message when the operating system has no entropy to give.
    pub fn generate() -> Result<Self, String> {
        let verifier = random_token(32)?;
        Ok(Self {
            challenge: Self::challenge_for(&verifier),
            verifier,
        })
    }

    /// The S256 challenge of `verifier`.
    #[must_use]
    pub fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }
}

/// The URL the user opens for the browser flow.
///
/// # Errors
///
/// A message when the manifest's authorization endpoint is not a URL.
pub fn authorize_url(
    requirement: &OAuthRequirement,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<String, String> {
    let base = requirement
        .authorize_url
        .as_deref()
        .ok_or("the source declares no authorization endpoint")?;
    let mut url = reqwest::Url::parse(base).map_err(|e| format!("authorize_url `{base}`: {e}"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &requirement.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        if !requirement.scopes.is_empty() {
            query.append_pair("scope", &requirement.scopes.join(" "));
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirement() -> OAuthRequirement {
        OAuthRequirement {
            client_id: "client".into(),
            scopes: vec!["read".into(), "write".into()],
            token_url: "https://auth.example.com/token".into(),
            authorize_url: Some("https://auth.example.com/authorize?audience=api".into()),
            device_authorization_url: None,
            callback_path: None,
            on_demand: false,
        }
    }

    #[test]
    fn the_pkce_challenge_is_the_s256_of_the_verifier_as_the_rfc_defines_it() {
        // The worked example of RFC 7636 appendix B.
        assert_eq!(
            Pkce::challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let pair = Pkce::generate().unwrap();
        assert!(pair.verifier.len() >= 43);
        assert_eq!(pair.challenge, Pkce::challenge_for(&pair.verifier));
        assert_ne!(pair.verifier, Pkce::generate().unwrap().verifier);
    }

    #[test]
    fn the_authorization_url_carries_pkce_state_scopes_and_keeps_the_endpoints_own_query() {
        let url = authorize_url(&requirement(), "http://127.0.0.1:9/callback", "st", "ch").unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["audience"], "api");
        assert_eq!(pairs["response_type"], "code");
        assert_eq!(pairs["client_id"], "client");
        assert_eq!(pairs["redirect_uri"], "http://127.0.0.1:9/callback");
        assert_eq!(pairs["state"], "st");
        assert_eq!(pairs["code_challenge"], "ch");
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["scope"], "read write");
    }

    #[test]
    fn a_provider_may_send_a_number_as_a_string_and_the_token_debug_hides_the_tokens() {
        assert_eq!(seconds(Some(&serde_json::json!("30"))), Some(30));
        assert_eq!(seconds(Some(&serde_json::json!(30))), Some(30));
        assert_eq!(seconds(None), None);

        let set = TokenSet {
            access_token: Secret::new("a-secret"),
            refresh_token: Some(Secret::new("r-secret")),
            expires_in: Some(1),
            scopes: vec![],
        };
        assert!(!format!("{set:?}").contains("secret"));
    }

    #[test]
    fn a_message_names_the_provider_host_and_nothing_of_the_endpoint_path() {
        assert_eq!(
            host("https://auth.example.com/oauth/token?x=1"),
            "auth.example.com"
        );
        assert_eq!(host("http://127.0.0.1:9/t"), "127.0.0.1:9");
    }
}
