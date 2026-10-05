//! Signing a package archive and checking such a signature (docs/adr/033).
//!
//! A publisher signs the archive's bytes with an ed25519 key and puts the signature in the registry index; a user who
//! trusts that publisher lists the public key under `mining.trusted_keys`. Nothing here decides *whether* a signature
//! is required: that is the trust policy in `crate::distribution`. This module only makes keys and signatures and says
//! whether one is valid, so the publishing commands and the daemon use the same code.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::domain::{IndexSignature, sha256_hex};
use crate::error::{Error, Result};

fn failed(message: impl Into<String>) -> Error {
    Error::SourceSigning {
        message: message.into(),
    }
}

/// The id of a public key: the first 16 hex characters of the SHA-256 of its 32 bytes.
///
/// Short enough to read in a listing and long enough that two keys do not collide by accident. It identifies a key in
/// an index; it is never what is trusted (the public key itself is).
#[must_use]
pub fn key_id(public: &VerifyingKey) -> String {
    sha256_hex(public.as_bytes())[..16].to_string()
}

/// A new signing key from the operating system's entropy.
///
/// # Errors
///
/// [`Error::EntropyUnavailable`] when the system cannot provide randomness.
pub fn generate() -> Result<SigningKey> {
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(Error::entropy_unavailable)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The public key as published: base64 of its 32 bytes.
#[must_use]
pub fn public_key_text(public: &VerifyingKey) -> String {
    STANDARD.encode(public.as_bytes())
}

/// Parse a public key as [`public_key_text`] writes it.
///
/// # Errors
///
/// [`Error::SourceSigning`] when it is not base64 of a valid 32-byte ed25519 key.
pub fn parse_public_key(text: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| failed("a public key is the base64 of 32 bytes"))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| failed(format!("not a valid public key: {e}")))
}

/// Parse a signing key file's text: the base64 of a 32-byte seed.
///
/// # Errors
///
/// [`Error::SourceSigning`] when it is not.
pub fn parse_signing_key(text: &str) -> Result<SigningKey> {
    let seed: [u8; 32] = STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| failed("a signing key file holds the base64 of a 32-byte seed"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Write `key` to `path`, readable by its owner alone, refusing to overwrite a file.
///
/// A private key that other users can read is no longer private, and overwriting one by mistake loses the ability to
/// publish under the key people already trust.
///
/// # Errors
///
/// [`Error::SourceSigning`] when the file exists, and [`Error::Io`] when it cannot be written.
pub fn write_signing_key(path: &Path, key: &SigningKey) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::AlreadyExists {
            failed(format!(
                "{} already exists; choose another file rather than overwrite a key",
                path.display()
            ))
        } else {
            Error::io(path.display().to_string(), source)
        }
    })?;
    writeln!(file, "{}", STANDARD.encode(key.to_bytes()))
        .map_err(|source| Error::io(path.display().to_string(), source))
}

/// Read a signing key file.
///
/// # Errors
///
/// [`Error::Io`] when it cannot be read, and [`Error::SourceSigning`] when it is not a key.
pub fn read_signing_key(path: &Path) -> Result<SigningKey> {
    let text = std::fs::read_to_string(path)
        .map_err(|source| Error::io(path.display().to_string(), source))?;
    parse_signing_key(&text)
}

/// Sign `archive` with `key`.
#[must_use]
pub fn sign(key: &SigningKey, archive: &[u8]) -> IndexSignature {
    IndexSignature {
        key: key_id(&key.verifying_key()),
        value: STANDARD.encode(key.sign(archive).to_bytes()),
    }
}

/// Whether `signature` is `public`'s valid signature of `archive`.
///
/// # Errors
///
/// A sentence saying why not.
pub fn verify(
    public: &VerifyingKey,
    archive: &[u8],
    signature: &IndexSignature,
) -> std::result::Result<(), String> {
    let bytes: [u8; 64] = STANDARD
        .decode(signature.value.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| "the signature is not the base64 of 64 bytes".to_string())?;
    // `verify_strict` also rejects the malleable and small-order encodings plain `verify` accepts.
    public
        .verify_strict(archive, &Signature::from_bytes(&bytes))
        .map_err(|_| "the signature does not match the package".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_verifies_only_for_the_key_and_the_bytes_it_was_made_over() {
        let key = generate().unwrap();
        let other = generate().unwrap();
        let signature = sign(&key, b"archive");

        assert!(verify(&key.verifying_key(), b"archive", &signature).is_ok());
        assert!(verify(&key.verifying_key(), b"archivf", &signature).is_err());
        assert!(verify(&other.verifying_key(), b"archive", &signature).is_err());
        assert_eq!(signature.key, key_id(&key.verifying_key()));
    }

    #[test]
    fn a_malformed_signature_is_refused_rather_than_trusted() {
        let key = generate().unwrap();
        let bad = IndexSignature {
            key: key_id(&key.verifying_key()),
            value: "not base64!".into(),
        };
        assert!(
            verify(&key.verifying_key(), b"x", &bad)
                .unwrap_err()
                .contains("64 bytes")
        );
    }

    #[test]
    fn keys_survive_being_written_and_read_back() {
        let key = generate().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publisher.key");
        write_signing_key(&path, &key).unwrap();

        let read = read_signing_key(&path).unwrap();
        assert_eq!(read.to_bytes(), key.to_bytes());
        let public = parse_public_key(&public_key_text(&key.verifying_key())).unwrap();
        assert_eq!(public, key.verifying_key());
    }

    #[test]
    fn a_key_file_is_never_overwritten_and_is_private_to_its_owner() {
        let key = generate().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publisher.key");
        write_signing_key(&path, &key).unwrap();

        let again = write_signing_key(&path, &generate().unwrap()).unwrap_err();
        assert!(again.to_string().contains("already exists"), "{again}");
        assert_eq!(read_signing_key(&path).unwrap().to_bytes(), key.to_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "group and others have access: {mode:o}");
        }
    }

    #[test]
    fn something_that_is_not_a_key_is_named_as_such() {
        assert!(parse_public_key("AAAA").is_err());
        assert!(parse_signing_key("AAAA").is_err());
        assert!(parse_public_key("###").is_err());
    }
}
