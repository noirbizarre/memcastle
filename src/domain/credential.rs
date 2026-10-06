//! What the mining runtime may ask for when a source needs to sign in: a fresh access token, and nothing else.
//!
//! A trait and no more, so that `mining` can hand a token to a source without knowing how it was obtained, where it is
//! kept or which provider issued it, and so that `credential` (which knows all three) never has to know the mining
//! runtime (docs/adr/039).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{OAuthRequirement, Secret};
use crate::error::Result;

/// The access tokens of the sources that sign in with OAuth.
///
/// Implemented by `credential::Credentials`. Synchronous because the one place that asks, a source's host call, is.
pub trait AccessTokens: Send + Sync {
    /// A usable access token for the installed source `source`, renewed first if it is about to expire.
    ///
    /// `requirement` is what the installed manifest declares: a credential obtained under different terms is not used.
    ///
    /// # Errors
    ///
    /// [`crate::Error::CredentialRequired`] when the source was never signed in or the provider no longer honours
    /// its credential, and [`crate::Error::CredentialRefreshFailed`] when the credential is kept but could not be
    /// renewed just now.
    fn access_token(&self, source: &str, requirement: &OAuthRequirement) -> Result<Secret>;

    /// Whether `source` has a credential it could renew from, without asking the provider for anything.
    ///
    /// Cheap enough to check before a job starts, so that a source nobody signed in fails there and not halfway through.
    fn is_signed_in(&self, source: &str, requirement: &OAuthRequirement) -> bool;
}

/// Where a source that signs in with OAuth stands, for `source show` and `source list`. Carries no token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAuth {
    /// Whether there is a credential that would be used.
    pub signed_in: bool,
    /// When the access token last seen stops being valid, when known. A refresh token renews it, so this is not a
    /// deadline for the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// What was granted at the last sign-in.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// The host the source signs in at, for a person to recognise.
    pub provider: String,
    /// The flows the source supports: `device`, `browser`, or both.
    pub flows: Vec<String>,
}
