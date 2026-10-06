//! OAuth credentials for mining sources (docs/adr/039).
//!
//! A source that cannot be reached with a static token declares `[permissions.oauth]`; this module is everything the
//! daemon does about it, so that no source implements its own sign-in, storage or renewal. It runs the sign-in
//! ([`Credentials::begin`] and [`Credentials::wait`]: the device flow, or the browser flow with PKCE), keeps the
//! tokens ([`store`]), and hands the mining runtime a fresh access token when it asks ([`AccessTokens`]), renewing it
//! first when it is about to expire.
//!
//! Nothing here touches the palace store, the jobs or the WebAssembly runtime: the runtime sees only the
//! [`AccessTokens`] trait, and only `app` and `server` construct or call a [`Credentials`]
//! (`tests/credential_isolation.rs`).

mod flow;
pub mod oauth;
pub mod store;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::config::CredentialsConfig;
use crate::domain::{AccessTokens, OAuthRequirement, Secret};
use crate::error::{Error, Result};

pub use flow::{Challenge, FlowKind, FlowStatus};
use oauth::{OAuthError, TokenSet};
use store::{CredentialStore, StoredCredential};

/// An access token with less than this left is renewed before it is handed out, so that it does not expire in the
/// middle of the call that is about to use it.
const REFRESH_MARGIN: TimeDelta = TimeDelta::seconds(60);

/// How long an access token is assumed to last when the provider does not say (RFC 6749 makes `expires_in`
/// optional). Short, so that a wrong guess costs a refresh and not a failed call.
const ASSUMED_LIFETIME: TimeDelta = TimeDelta::seconds(300);

/// How fast a sign-in may poll a provider.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The shortest time between two polls, whatever the provider asks for.
    pub min_poll: Duration,
    /// How much longer to wait after the provider says `slow_down` (RFC 8628 section 3.5 says five seconds).
    pub slow_down_step: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            min_poll: Duration::from_secs(1),
            slow_down_step: Duration::from_secs(5),
        }
    }
}

/// Where a source stands, for `source show` and `source auth`. Carries no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignInState {
    /// Whether there is a credential that would be used.
    pub signed_in: bool,
    /// When the access token last seen stops being valid, if known. A refresh token renews it, so this is not a
    /// deadline for the source.
    pub expires_at: Option<DateTime<Utc>>,
    /// What was granted.
    pub scopes: Vec<String>,
}

/// The end of a successful sign-in. Carries no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedIn {
    /// The source.
    pub source: String,
    /// Always true; present so the answer reads the same as [`SignInState`].
    pub signed_in: bool,
    /// When the access token stops being valid, if known.
    pub expires_at: Option<DateTime<Utc>>,
    /// What was granted.
    pub scopes: Vec<String>,
    /// Where the credential is kept now: `keyring` or `file`.
    pub stored_in: String,
}

struct Cached {
    token: Secret,
    expires_at: DateTime<Utc>,
    fingerprint: String,
}

struct Inner {
    store: Box<dyn CredentialStore>,
    /// Access tokens by source. Memory only: they are short-lived and cheap to obtain again from the refresh token.
    cache: Mutex<HashMap<String, Cached>>,
    /// One lock per source, held across a refresh, so that many callers share one renewal. A provider that rotates
    /// refresh tokens would otherwise see the old one used twice and revoke the whole grant.
    refreshing: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    flows: Mutex<flow::Flows>,
    timing: Timing,
}

/// The daemon's OAuth credentials: sign-in, storage and renewal for every source that needs them.
#[derive(Clone)]
pub struct Credentials {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").finish_non_exhaustive()
    }
}

/// Lock a mutex, whatever a panicking holder left: nothing under these locks is left half-updated.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Credentials {
    /// Credentials kept in `store`.
    #[must_use]
    pub fn new(store: Box<dyn CredentialStore>) -> Self {
        Self::with_timing(store, Timing::default())
    }

    /// As [`Credentials::new`], polling a provider at the given pace.
    #[must_use]
    pub fn with_timing(store: Box<dyn CredentialStore>, timing: Timing) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                cache: Mutex::default(),
                refreshing: Mutex::default(),
                flows: Mutex::default(),
                timing,
            }),
        }
    }

    /// Credentials kept where the configuration says.
    #[must_use]
    pub fn from_config(config: &CredentialsConfig) -> Self {
        Self::new(store::from_config(config))
    }

    fn lock_for(&self, source: &str) -> Arc<Mutex<()>> {
        Arc::clone(
            locked(&self.inner.refreshing)
                .entry(source.to_string())
                .or_default(),
        )
    }

    fn cached(&self, source: &str, fingerprint: &str) -> Option<Secret> {
        let cache = locked(&self.inner.cache);
        let entry = cache.get(source)?;
        (entry.fingerprint == fingerprint && entry.expires_at - Utc::now() > REFRESH_MARGIN)
            .then(|| entry.token.clone())
    }

    fn remember(&self, source: &str, fingerprint: &str, token: &Secret, expires_at: DateTime<Utc>) {
        locked(&self.inner.cache).insert(
            source.to_string(),
            Cached {
                token: token.clone(),
                expires_at,
                fingerprint: fingerprint.to_string(),
            },
        );
    }

    /// Keep what a sign-in or a renewal returned: the credential in the store, the access token in memory.
    ///
    /// `previous_refresh` is what a renewal that rotated nothing keeps using.
    fn install(
        &self,
        source: &str,
        requirement: &OAuthRequirement,
        tokens: &TokenSet,
        previous_refresh: Option<&str>,
        save: bool,
    ) -> Result<(DateTime<Utc>, &'static str, Vec<String>)> {
        let now = Utc::now();
        let expires_at = now
            + tokens
                .expires_in
                .and_then(|secs| i64::try_from(secs).ok())
                .map_or(ASSUMED_LIFETIME, TimeDelta::seconds);
        let refresh_token = tokens
            .refresh_token
            .as_ref()
            .map(|token| token.expose().to_string())
            .or_else(|| previous_refresh.map(str::to_string));
        let scopes = if tokens.scopes.is_empty() {
            requirement.scopes.clone()
        } else {
            tokens.scopes.clone()
        };
        let fingerprint = requirement.fingerprint();
        let mut stored_in = "memory";
        if save {
            let credential = StoredCredential {
                fingerprint: fingerprint.clone(),
                // With no refresh token the access token is all there is to keep, and it is kept until it expires.
                access_token: refresh_token
                    .is_none()
                    .then(|| tokens.access_token.expose().to_string()),
                expires_at: refresh_token.is_none().then_some(expires_at),
                refresh_token,
                scopes: scopes.clone(),
                obtained_at: now,
            };
            stored_in = self.inner.store.save(source, &credential)?;
        }
        self.remember(source, &fingerprint, &tokens.access_token, expires_at);
        Ok((expires_at, stored_in, scopes))
    }

    /// Where `source` stands, without asking the provider for anything.
    ///
    /// # Errors
    ///
    /// [`Error::CredentialStoreFailed`] when the store cannot be read.
    pub fn state(&self, source: &str, requirement: &OAuthRequirement) -> Result<SignInState> {
        let signed_out = SignInState {
            signed_in: false,
            expires_at: None,
            scopes: Vec::new(),
        };
        let Some(stored) = self.inner.store.load(source)? else {
            return Ok(signed_out);
        };
        if stored.fingerprint != requirement.fingerprint() {
            return Ok(signed_out);
        }
        let usable = stored.refresh_token.is_some()
            || stored
                .expires_at
                .is_some_and(|at| at - Utc::now() > REFRESH_MARGIN);
        let cached = locked(&self.inner.cache)
            .get(source)
            .filter(|entry| entry.fingerprint == stored.fingerprint)
            .map(|entry| entry.expires_at);
        Ok(SignInState {
            signed_in: usable,
            expires_at: cached.or(stored.expires_at),
            scopes: stored.scopes,
        })
    }

    /// Forget `source`'s credential, as removing the source does.
    ///
    /// # Errors
    ///
    /// [`Error::CredentialStoreFailed`] when the store cannot be written.
    pub fn forget(&self, source: &str) -> Result<()> {
        locked(&self.inner.cache).remove(source);
        self.cancel_flow(source);
        self.inner.store.delete(source)
    }

    /// Renew `source`'s access token. Holds the source's lock, so a caller that arrives meanwhile waits for this
    /// renewal and then finds its result in the cache.
    fn renew(&self, source: &str, requirement: &OAuthRequirement) -> Result<Secret> {
        let fingerprint = requirement.fingerprint();
        let guard = self.lock_for(source);
        let _held = locked(&guard);
        // Another caller may have renewed while this one waited for the lock.
        if let Some(token) = self.cached(source, &fingerprint) {
            return Ok(token);
        }
        let required = |reason: &str| Error::CredentialRequired {
            source_name: source.to_string(),
            reason: reason.to_string(),
        };
        let stored = self
            .inner
            .store
            .load(source)?
            .ok_or_else(|| required("it has never been signed in"))?;
        if stored.fingerprint != fingerprint {
            return Err(required(
                "it was signed in under other terms than the installed source asks for now (client, endpoints or scopes)",
            ));
        }
        let Some(refresh_token) = stored.refresh_token.clone() else {
            // No refresh token: the access token is all there was, and it is used until it expires.
            return match (&stored.access_token, stored.expires_at) {
                (Some(token), Some(at)) if at - Utc::now() > REFRESH_MARGIN => {
                    let token = Secret::new(token.clone());
                    self.remember(source, &fingerprint, &token, at);
                    Ok(token)
                }
                _ => Err(required(
                    "its access token expired and the provider issued no refresh token to renew it with",
                )),
            };
        };
        match refresh_blocking(requirement.clone(), refresh_token.clone()) {
            Ok(tokens) => {
                let rotated = tokens
                    .refresh_token
                    .as_ref()
                    .is_some_and(|new| new.expose() != refresh_token);
                // The store is written only when there is something new to keep: most renewals rotate nothing.
                self.install(source, requirement, &tokens, Some(&refresh_token), rotated)?;
                tracing::debug!(source, rotated, "renewed an access token");
                Ok(tokens.access_token)
            }
            Err(OAuthError::Revoked(message)) => {
                // The provider will never honour this credential again, so keeping it only makes every later run fail
                // the same way and hides that the fix is to sign in.
                locked(&self.inner.cache).remove(source);
                self.inner.store.delete(source)?;
                tracing::info!(
                    source,
                    "the provider revoked a stored credential; it was removed"
                );
                Err(Error::CredentialRequired {
                    source_name: source.to_string(),
                    reason: format!("the provider no longer honours it ({message})"),
                })
            }
            Err(OAuthError::Rejected(message) | OAuthError::Unreachable(message)) => {
                Err(Error::CredentialRefreshFailed {
                    source_name: source.to_string(),
                    message,
                })
            }
        }
    }
}

impl AccessTokens for Credentials {
    fn access_token(&self, source: &str, requirement: &OAuthRequirement) -> Result<Secret> {
        match self.cached(source, &requirement.fingerprint()) {
            Some(token) => Ok(token),
            None => self.renew(source, requirement),
        }
    }

    fn is_signed_in(&self, source: &str, requirement: &OAuthRequirement) -> bool {
        if self.cached(source, &requirement.fingerprint()).is_some() {
            return true;
        }
        self.state(source, requirement)
            .is_ok_and(|state| state.signed_in)
    }
}

/// Renew on a thread and runtime of its own.
///
/// The caller is a synchronous host call from inside a WebAssembly source, which may be on a runtime worker or not, and
/// `block_on` from a runtime worker panics. A thread and a current-thread runtime work from anywhere, and a renewal
/// is rare enough (about once an hour per source) that starting them is free.
fn refresh_blocking(
    requirement: OAuthRequirement,
    refresh_token: String,
) -> std::result::Result<TokenSet, OAuthError> {
    let unreachable = |message: String| OAuthError::Unreachable(message);
    std::thread::Builder::new()
        .name("memcastle-credential-refresh".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| OAuthError::Unreachable(format!("cannot start a runtime: {e}")))?;
            runtime.block_on(async {
                // A client of its own: one made on another runtime keeps connections that this one cannot drive.
                let client = oauth::client()?;
                oauth::refresh(&client, &requirement, &refresh_token).await
            })
        })
        .map_err(|e| unreachable(format!("cannot start a thread: {e}")))?
        .join()
        .unwrap_or_else(|_| Err(unreachable("the renewal panicked".to_string())))
}

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests;
