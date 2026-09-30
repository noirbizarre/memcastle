//! Bearer-token credentials: the token format and the verifier the daemon
//! keeps instead of the token itself.
//!
//! Pure, like the rest of `domain`: no I/O, no randomness. The entropy is
//! passed in by `app`, which is what lets every function here be tested with
//! a fixed input.
//!
//! A generated token is 32 random bytes, so it is high-entropy by
//! construction and a plain SHA-256 is a sufficient verifier: a password KDF
//! exists to slow down guessing of *low*-entropy secrets, and there is nothing
//! to guess here. The algorithm and a version are stored beside the digest so
//! a future change of scheme can be told apart from a corrupt row
//! (`docs/adr/014-optional-token-authentication.md`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

/// How a generated token starts. Recognisable, so a secret scanner or a human
/// reviewing a leaked paste can tell what it is.
pub const TOKEN_PREFIX: &str = "mc_";

/// Bytes of entropy in a generated token.
pub const TOKEN_ENTROPY_BYTES: usize = 32;

/// The digest algorithm a [`TokenVerifier`] was made with.
pub const ALGORITHM: &str = "sha256";

/// The version of the verifier scheme (algorithm plus encoding).
pub const VERIFIER_VERSION: u32 = 1;

/// A token rendered from `entropy`: [`TOKEN_PREFIX`] and lowercase hex.
#[must_use]
pub fn format_token(entropy: &[u8; TOKEN_ENTROPY_BYTES]) -> String {
    let hex: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
    format!("{TOKEN_PREFIX}{hex}")
}

/// The SHA-256 of `token`, as lowercase hex.
///
/// Both the stored verifier and the configured shared secret are compared
/// through this, so the two credential sources share one comparison path.
#[must_use]
pub fn digest_token(token: &str) -> String {
    super::sha256_hex(token.as_bytes())
}

/// Whether two digests are equal, in constant time.
///
/// `==` on strings returns at the first differing byte, which leaks how much
/// of a guess was right. Digests are fixed-width, so the length check that
/// `subtle` does first reveals nothing.
#[must_use]
pub fn digests_match(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// What the daemon persists to recognise a generated token: never the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenVerifier {
    /// The digest algorithm, [`ALGORITHM`] today.
    pub algorithm: String,
    /// The scheme version, [`VERIFIER_VERSION`] today.
    pub version: u32,
    /// The token's digest, lowercase hex.
    pub digest: String,
    /// When the token was generated.
    pub created_at: DateTime<Utc>,
}

impl TokenVerifier {
    /// The verifier for `token`, created at `created_at`.
    #[must_use]
    pub fn for_token(token: &str, created_at: DateTime<Utc>) -> Self {
        Self {
            algorithm: ALGORITHM.to_string(),
            version: VERIFIER_VERSION,
            digest: digest_token(token),
            created_at,
        }
    }

    /// Whether `presented` is the token this verifier was made for.
    ///
    /// A verifier made by an algorithm or version this build does not know
    /// matches nothing: failing closed is the only safe reading of a scheme
    /// that cannot be checked.
    #[must_use]
    pub fn verifies(&self, presented: &str) -> bool {
        if self.algorithm != ALGORITHM || self.version != VERIFIER_VERSION {
            return false;
        }
        digests_match(&self.digest, &digest_token(presented))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTROPY: [u8; TOKEN_ENTROPY_BYTES] = [0xab; TOKEN_ENTROPY_BYTES];

    #[test]
    fn a_token_is_the_prefix_followed_by_sixty_four_hex_digits() {
        let token = format_token(&ENTROPY);
        assert!(token.starts_with(TOKEN_PREFIX));
        let hex = &token[TOKEN_PREFIX.len()..];
        assert_eq!(hex.len(), TOKEN_ENTROPY_BYTES * 2);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_verifier_accepts_its_token_and_nothing_else() {
        let token = format_token(&ENTROPY);
        let verifier = TokenVerifier::for_token(&token, Utc::now());

        assert!(verifier.verifies(&token));
        assert!(!verifier.verifies(&format_token(&[0xcd; TOKEN_ENTROPY_BYTES])));
        assert!(!verifier.verifies(""));
    }

    #[test]
    fn a_verifier_never_contains_the_plaintext_token() {
        let token = format_token(&ENTROPY);
        let json = serde_json::to_string(&TokenVerifier::for_token(&token, Utc::now())).unwrap();
        assert!(!json.contains(&token));
        assert!(!json.contains(&token[TOKEN_PREFIX.len()..]));
    }

    #[test]
    fn a_verifier_from_an_unknown_scheme_fails_closed() {
        let token = format_token(&ENTROPY);
        for verifier in [
            TokenVerifier {
                algorithm: "md5".to_string(),
                ..TokenVerifier::for_token(&token, Utc::now())
            },
            TokenVerifier {
                version: VERIFIER_VERSION + 1,
                ..TokenVerifier::for_token(&token, Utc::now())
            },
        ] {
            assert!(!verifier.verifies(&token));
        }
    }

    #[test]
    fn digests_of_different_length_do_not_match() {
        assert!(!digests_match("abc", "abcd"));
        assert!(digests_match("abc", "abc"));
    }
}
