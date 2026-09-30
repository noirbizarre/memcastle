//! The stored token verifier (`auth_state:token`): what the daemon keeps to
//! recognise a token made by `memcastle auth generate`, never the token.
//!
//! Its own file, like `migration_state`: this is infrastructure state, not
//! palace content. A single fixed row, so `save_*` is rotation and `clear_*`
//! is revocation, and both take effect on the very next read.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::auth::TokenVerifier;
use crate::error::{Error, Result};

use super::SurrealStore;

#[derive(Debug, Deserialize)]
struct VerifierRow {
    algorithm: String,
    version: i64,
    digest: String,
    created_at: String,
}

impl SurrealStore {
    /// The stored verifier, or `None` when no token has been generated (or it
    /// was revoked). A pure read.
    pub async fn get_token_verifier(&self) -> Result<Option<TokenVerifier>> {
        let mut response = self
            .db
            .query(
                "SELECT algorithm, version, digest, <string>created_at AS created_at \
                 FROM auth_state:token",
            )
            .await?;
        let rows: Vec<VerifierRow> = super::take_rows(&mut response, 0)?;
        rows.into_iter()
            .next()
            .map(|row| {
                let created_at = DateTime::parse_from_rfc3339(&row.created_at)
                    .map_err(|source| Error::store_malformed(source.to_string()))?
                    .with_timezone(&Utc);
                let version = u32::try_from(row.version)
                    .map_err(|source| Error::store_malformed(source.to_string()))?;
                Ok(TokenVerifier {
                    algorithm: row.algorithm,
                    version,
                    digest: row.digest,
                    created_at,
                })
            })
            .transpose()
    }

    /// Store `verifier` as the one valid generated token, replacing any
    /// previous one: the old token stops working as soon as this returns.
    pub async fn save_token_verifier(&self, verifier: &TokenVerifier) -> Result<()> {
        // UPSERT, so the first generation and every rotation are one statement
        // and there is never a moment with two valid tokens.
        self.db
            .query(
                "UPSERT auth_state:token SET algorithm = $algorithm, version = $version, \
                 digest = $digest, created_at = <datetime>$created_at",
            )
            .bind(("algorithm", verifier.algorithm.clone()))
            .bind(("version", verifier.version))
            .bind(("digest", verifier.digest.clone()))
            .bind(("created_at", super::stored(verifier.created_at)))
            .await?
            .check()?;
        Ok(())
    }

    /// Remove the stored verifier, revoking the generated token. Returns
    /// whether there was one to remove.
    pub async fn clear_token_verifier(&self) -> Result<bool> {
        let existed = self.get_token_verifier().await?.is_some();
        self.db.query("DELETE auth_state:token").await?.check()?;
        Ok(existed)
    }
}
