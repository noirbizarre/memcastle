//! A value that must never reach a log line, a status report or a serialised config.

use serde::Deserialize;

/// A configuration value that must never reach a log line, a status report or a
/// serialised config.
///
/// `Debug` is redacted by hand and `Serialize` is skipped on the owning field:
/// `Config` derives both, and a derived `Debug` on a bare `String` would print
/// the secret the first time someone wrote `tracing::debug!("{config:?}")`.
/// It lives in `domain` (re-exported from `config`) so that `store::Backend`
/// can carry the remote database password without `store` depending on `config`.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The plaintext, for the places that genuinely need it: hashing a token on
    /// the daemon, sending it as a bearer header from the client, and signing in
    /// to a remote database.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A fixed placeholder, not even the length: the length narrows a brute force.
        f.write_str("[REDACTED]")
    }
}
