//! Authentication services: checking a presented bearer token, generating one
//! and revoking it.
//!
//! Deliberately not gated by [`crate::domain::MemoryMode`]: a memory mode is a
//! session's privilege over *memory*, while this is the caller's *identity*,
//! and folding the two together would let a `Full`-mode session mint credentials.
//! Nothing here is reachable from MCP (`docs/adr/014`): `mcp` calls none of it.
//!
//! The boundary is provider-agnostic on purpose: callers hand over "whatever
//! credential the request carried" and get back "allowed or not", so an OAuth
//! or OIDC provider can later replace the body of [`AppServices::authenticate`]
//! without touching the HTTP layer or any route.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::auth::{self, TokenVerifier};
use crate::error::{Error, Result};
use crate::store::SurrealStore;

use super::AppServices;

/// What the daemon was told about authentication when it started. Decided once
/// per process from configuration, so a request can never flip it.
#[derive(Clone, Default)]
pub struct AuthPolicy {
    /// Whether requests must carry a valid token.
    enabled: bool,
    /// The SHA-256 of the configured shared secret. The plaintext is hashed at
    /// startup and dropped, so the daemon holds nothing it could leak later.
    configured_digest: Option<String>,
}

impl AuthPolicy {
    /// A policy requiring authentication iff `enabled`, accepting `secret`
    /// (the configured shared token) in addition to any generated one.
    #[must_use]
    pub fn new(enabled: bool, secret: Option<&str>) -> Self {
        Self {
            enabled,
            configured_digest: secret.map(auth::digest_token),
        }
    }

    /// Refuse to serve when authentication is on but nothing could ever
    /// satisfy it: neither a configured secret nor a stored verifier. Failing
    /// here, at startup, is the alternative to a daemon that locks every
    /// client (and the CLI that would fix it) out.
    ///
    /// Takes the store rather than living on `AppServices` because `server::run`
    /// must ask before it starts the scheduler, which `AppServices` needs.
    ///
    /// # Errors
    ///
    /// [`Error::AuthNotConfigured`] in that situation, or a store error.
    pub async fn ensure_satisfiable(&self, store: &SurrealStore) -> Result<()> {
        if self.enabled
            && self.configured_digest.is_none()
            && store.get_token_verifier().await?.is_none()
        {
            return Err(Error::AuthNotConfigured);
        }
        Ok(())
    }
}

// Not derived: the digest is not the secret, but it is exactly what an
// offline guesser would want, and nothing needs to print it.
impl std::fmt::Debug for AuthPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthPolicy")
            .field("enabled", &self.enabled)
            .field("configured_secret", &self.configured_digest.is_some())
            .finish()
    }
}

/// A freshly generated token: the only place its plaintext ever exists.
#[derive(Clone, Serialize, Deserialize)]
pub struct GeneratedToken {
    /// The plaintext bearer token. Shown once; the daemon keeps only a digest.
    pub token: String,
    /// The digest algorithm the stored verifier uses.
    pub algorithm: String,
    /// The verifier scheme version.
    pub version: u32,
    /// When the token was generated.
    pub created_at: DateTime<Utc>,
}

// Not derived, so a stray `{:?}` (or a future `tracing` field) cannot print it.
impl std::fmt::Debug for GeneratedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedToken")
            .field("token", &"[REDACTED]")
            .field("algorithm", &self.algorithm)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// What revoking the generated token answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeResult {
    /// Whether a stored token existed to revoke.
    pub revoked: bool,
}

impl AppServices {
    /// Replace the authentication policy. Set once by `server::run`.
    #[must_use]
    pub fn with_auth(mut self, policy: AuthPolicy) -> Self {
        self.auth = std::sync::Arc::new(policy);
        self
    }

    /// Whether requests must carry a valid token.
    #[must_use]
    pub fn auth_enabled(&self) -> bool {
        self.auth.enabled
    }

    /// Check the credential a request carried.
    ///
    /// Always succeeds when authentication is disabled. Otherwise `presented`
    /// must be the configured secret or the generated token; every
    /// comparison is constant-time. The stored verifier is read on each call
    /// rather than cached, so a rotation or revocation takes effect on the very
    /// next request instead of after a restart.
    ///
    /// # Errors
    ///
    /// [`Error::Unauthorized`] when the token is missing or matches neither
    /// source, or a store error while reading the verifier (which also denies
    /// the request: an unreadable verifier must not become an open door).
    pub async fn authenticate(&self, presented: Option<&str>) -> Result<()> {
        if !self.auth.enabled {
            return Ok(());
        }
        let Some(presented) = presented else {
            return Err(Error::Unauthorized { reason: "missing" });
        };
        let presented_digest = auth::digest_token(presented);
        let matches_configured = self
            .auth
            .configured_digest
            .as_deref()
            .is_some_and(|digest| auth::digests_match(digest, &presented_digest));
        if matches_configured {
            return Ok(());
        }
        let matches_stored = self
            .store
            .get_token_verifier()
            .await?
            .is_some_and(|verifier| verifier.verifies(presented));
        if matches_stored {
            Ok(())
        } else {
            Err(Error::Unauthorized { reason: "invalid" })
        }
    }

    /// Generate a new high-entropy token, store its verifier (replacing any
    /// previous one, which stops working immediately) and return the plaintext
    /// exactly once.
    ///
    /// # Errors
    ///
    /// [`Error::EntropyUnavailable`] if the operating system cannot provide randomness, or
    /// a store error.
    pub async fn generate_token(&self) -> Result<GeneratedToken> {
        let mut entropy = [0_u8; auth::TOKEN_ENTROPY_BYTES];
        // Never fall back to a weaker source: a predictable token is worse
        // than no token, so a failure here fails the request.
        getrandom::fill(&mut entropy).map_err(Error::entropy_unavailable)?;
        let token = auth::format_token(&entropy);
        let verifier = TokenVerifier::for_token(&token, Utc::now());
        self.store.save_token_verifier(&verifier).await?;
        Ok(GeneratedToken {
            token,
            algorithm: verifier.algorithm,
            version: verifier.version,
            created_at: verifier.created_at,
        })
    }

    /// Revoke the generated token by removing its verifier.
    ///
    /// A shared secret from configuration is not affected: it lives outside
    /// the daemon's state and is revoked by changing the configuration.
    ///
    /// # Errors
    ///
    /// A store error.
    pub async fn revoke_token(&self) -> Result<RevokeResult> {
        Ok(RevokeResult {
            revoked: self.store.clear_token_verifier().await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "mc_a_shared_secret_from_the_environment";

    async fn services(enabled: bool, secret: Option<&str>) -> AppServices {
        AppServices::for_tests()
            .await
            .with_auth(AuthPolicy::new(enabled, secret))
    }

    #[tokio::test]
    async fn everything_is_allowed_while_authentication_is_disabled() {
        let app = services(false, None).await;
        assert!(app.authenticate(None).await.is_ok());
        assert!(app.authenticate(Some("anything")).await.is_ok());
    }

    #[tokio::test]
    async fn the_configured_secret_is_accepted_and_others_are_not() {
        let app = services(true, Some(SECRET)).await;

        assert!(app.authenticate(Some(SECRET)).await.is_ok());
        assert!(matches!(
            app.authenticate(Some("wrong")).await,
            Err(Error::Unauthorized { reason: "invalid" })
        ));
        assert!(matches!(
            app.authenticate(None).await,
            Err(Error::Unauthorized { reason: "missing" })
        ));
    }

    #[tokio::test]
    async fn a_generated_token_works_until_it_is_revoked() {
        let app = services(true, Some(SECRET)).await;
        let generated = app.generate_token().await.unwrap();

        assert!(app.authenticate(Some(&generated.token)).await.is_ok());

        assert!(app.revoke_token().await.unwrap().revoked);
        assert!(app.authenticate(Some(&generated.token)).await.is_err());
        // The configured secret is outside the daemon's state, so it survives.
        assert!(app.authenticate(Some(SECRET)).await.is_ok());
        assert!(!app.revoke_token().await.unwrap().revoked);
    }

    #[tokio::test]
    async fn generating_again_rotates_the_token() {
        let app = services(true, Some(SECRET)).await;
        let first = app.generate_token().await.unwrap();
        let second = app.generate_token().await.unwrap();

        assert_ne!(first.token, second.token);
        assert!(app.authenticate(Some(&first.token)).await.is_err());
        assert!(app.authenticate(Some(&second.token)).await.is_ok());
    }

    #[tokio::test]
    async fn the_plaintext_token_is_not_what_the_store_holds() {
        let app = services(false, None).await;
        let generated = app.generate_token().await.unwrap();

        let stored = app.store.get_token_verifier().await.unwrap().unwrap();
        let json = serde_json::to_string(&stored).unwrap();

        assert!(!json.contains(&generated.token));
        assert!(stored.verifies(&generated.token));
    }

    #[tokio::test]
    async fn enabling_authentication_with_nothing_to_check_against_is_refused() {
        let app = services(true, None).await;
        let err = app.auth.ensure_satisfiable(&app.store).await.unwrap_err();
        assert!(matches!(err, Error::AuthNotConfigured));
    }

    #[tokio::test]
    async fn a_stored_verifier_alone_satisfies_the_startup_check() {
        let app = services(true, None).await;
        app.generate_token().await.unwrap();
        assert!(app.auth.ensure_satisfiable(&app.store).await.is_ok());
    }

    #[tokio::test]
    async fn a_disabled_policy_needs_nothing_to_start() {
        let app = services(false, None).await;
        assert!(app.auth.ensure_satisfiable(&app.store).await.is_ok());
    }

    #[test]
    fn debug_output_never_shows_the_token_or_the_digest() {
        let generated = GeneratedToken {
            token: "mc_plaintext".to_string(),
            algorithm: "sha256".to_string(),
            version: 1,
            created_at: Utc::now(),
        };
        assert!(!format!("{generated:?}").contains("mc_plaintext"));
        let policy = format!("{:?}", AuthPolicy::new(true, Some(SECRET)));
        assert!(!policy.contains(SECRET));
        assert!(!policy.contains(&auth::digest_token(SECRET)));
    }
}
