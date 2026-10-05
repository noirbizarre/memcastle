//! The trust policy: what a package from a registry must prove before it is installed (docs/adr/033).
//!
//! The SHA-256 the index publishes is checked for every package regardless (that is integrity: the bytes are the ones
//! the index named). Trust is the separate question of whether the index itself is believed, which only a signature
//! from a key the user chose can answer, because an index and its archives are usually served from the same place.

use ed25519_dalek::VerifyingKey;

use crate::config::{MiningConfig, TrustMode};
use crate::domain::IndexSignature;
use crate::error::{Error, Result};
use crate::source::signing;

/// The configured trust: the mode and the keys it trusts.
#[derive(Debug, Clone)]
pub struct TrustPolicy {
    mode: TrustMode,
    keys: Vec<VerifyingKey>,
}

impl TrustPolicy {
    /// The policy `[mining]` configures.
    ///
    /// # Errors
    ///
    /// [`Error::SourceSigning`] when a trusted key is not a key (configuration validation reports this earlier; this
    /// keeps a hand-built configuration from trusting nothing silently).
    pub fn from_config(mining: &MiningConfig) -> Result<Self> {
        let keys = mining
            .trusted_keys
            .iter()
            .map(|key| signing::parse_public_key(key))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            mode: mining.trust,
            keys,
        })
    }

    /// Apply the policy to the archive of `name`, and answer the id of the key that vouched for it, if one did.
    ///
    /// - A signature from a trusted key must verify, in either mode: a bad signature from a key the user trusts is
    ///   not "unsigned", it is evidence of tampering.
    /// - A signature from a key the user has not listed says nothing, so it counts as no signature.
    /// - With `required`, a package nobody trusted vouched for is refused.
    ///
    /// # Errors
    ///
    /// [`Error::SourceUntrusted`] saying which of those it was.
    pub fn check(
        &self,
        name: &str,
        archive: &[u8],
        signature: Option<&IndexSignature>,
    ) -> Result<Option<String>> {
        let untrusted = |message: String| Error::SourceUntrusted {
            name: name.to_string(),
            message,
        };
        let vouched = match signature {
            Some(signature) => {
                match self
                    .keys
                    .iter()
                    .find(|key| signing::key_id(key) == signature.key)
                {
                    Some(key) => {
                        signing::verify(key, archive, signature).map_err(untrusted)?;
                        Some(signature.key.clone())
                    }
                    None => None,
                }
            }
            None => None,
        };
        if vouched.is_none() && self.mode == TrustMode::Required {
            return Err(untrusted(match signature {
                Some(signature) => format!(
                    "it is signed by key {}, which is not in `mining.trusted_keys`",
                    signature.key
                ),
                None => "it is not signed, and `mining.trust` is `required`".to_string(),
            }));
        }
        Ok(vouched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: TrustMode, trusted: &[&ed25519_dalek::SigningKey]) -> TrustPolicy {
        TrustPolicy {
            mode,
            keys: trusted.iter().map(|key| key.verifying_key()).collect(),
        }
    }

    #[test]
    fn a_valid_signature_from_a_trusted_key_is_accepted_and_names_the_key() {
        let key = signing::generate().unwrap();
        let signature = signing::sign(&key, b"archive");
        for mode in [TrustMode::Optional, TrustMode::Required] {
            let vouched = policy(mode, &[&key])
                .check("demo", b"archive", Some(&signature))
                .unwrap();
            assert_eq!(vouched, Some(signing::key_id(&key.verifying_key())));
        }
    }

    #[test]
    fn a_trusted_key_whose_signature_does_not_match_is_refused_in_every_mode() {
        let key = signing::generate().unwrap();
        let signature = signing::sign(&key, b"archive");
        for mode in [TrustMode::Optional, TrustMode::Required] {
            let error = policy(mode, &[&key])
                .check("demo", b"tampered", Some(&signature))
                .unwrap_err();
            assert!(matches!(error, Error::SourceUntrusted { .. }), "{error}");
        }
    }

    #[test]
    fn an_unsigned_package_passes_when_trust_is_optional_and_not_when_required() {
        let key = signing::generate().unwrap();
        assert_eq!(
            policy(TrustMode::Optional, &[&key])
                .check("demo", b"a", None)
                .unwrap(),
            None
        );
        let error = policy(TrustMode::Required, &[&key])
            .check("demo", b"a", None)
            .unwrap_err();
        assert!(error.to_string().contains("not signed"), "{error}");
    }

    #[test]
    fn a_signature_from_an_unknown_key_counts_as_unsigned() {
        let trusted = signing::generate().unwrap();
        let stranger = signing::generate().unwrap();
        let signature = signing::sign(&stranger, b"archive");

        assert_eq!(
            policy(TrustMode::Optional, &[&trusted])
                .check("demo", b"archive", Some(&signature))
                .unwrap(),
            None,
            "an unknown signer neither vouches nor condemns"
        );
        let error = policy(TrustMode::Required, &[&trusted])
            .check("demo", b"archive", Some(&signature))
            .unwrap_err();
        assert!(
            error.to_string().contains("not in `mining.trusted_keys`"),
            "{error}"
        );
    }

    #[test]
    fn the_policy_is_built_from_the_configured_keys() {
        let key = signing::generate().unwrap();
        let mining = MiningConfig {
            trust: TrustMode::Required,
            trusted_keys: vec![signing::public_key_text(&key.verifying_key())],
            ..MiningConfig::default()
        };
        let signature = signing::sign(&key, b"archive");
        let policy = TrustPolicy::from_config(&mining).unwrap();
        assert!(
            policy
                .check("demo", b"archive", Some(&signature))
                .unwrap()
                .is_some()
        );

        let bad = MiningConfig {
            trusted_keys: vec!["nope".into()],
            ..MiningConfig::default()
        };
        assert!(TrustPolicy::from_config(&bad).is_err());
    }
}
