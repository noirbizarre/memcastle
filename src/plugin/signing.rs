//! Local publisher tooling: sign a verified plugin archive for an indexed release.

use std::path::Path;

use serde::Serialize;

use crate::domain::{IndexSignature, sha256_hex};
use crate::error::Result;

/// What a static plugin index version needs to verify these archive bytes.
#[derive(Debug, Serialize)]
pub struct SignedArchive {
    /// SHA-256 of the exact archive file signed.
    pub sha256: String,
    /// Ed25519 signature and trusted key ID for `versions[].signature`.
    pub signature: IndexSignature,
}

/// Sign a validated archive with a publisher key created by `source keygen`.
///
/// # Errors
///
/// Refuses an invalid archive or a missing/malformed key, rather than signing bytes that cannot be installed.
pub fn sign_archive(archive: &[u8], key_file: &Path) -> Result<SignedArchive> {
    super::package::unpack(archive)?;
    let key = crate::source::signing::read_signing_key(key_file)?;
    Ok(SignedArchive {
        sha256: sha256_hex(archive),
        signature: crate::source::signing::sign(&key, archive),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::signing;

    #[test]
    fn signing_reports_the_digest_and_signature_a_plugin_catalogue_can_verify() {
        let dir = tempfile::tempdir().unwrap();
        let key = signing::generate().unwrap();
        let path = dir.path().join("publisher.key");
        signing::write_signing_key(&path, &key).unwrap();
        let archive = super::super::package::pack(&std::collections::BTreeMap::from([(
            "plugin.toml".into(),
            b"format = 1\nmemcastle = '>=0.4'\n[plugin]\nid = 'example'\nversion = '0.1.0'\nprovider = 'example'\ndescription = 'example'\nrepository = 'https://github.com/example/example'\nlicense = 'MIT'\n".to_vec(),
        )])).unwrap();
        let report = sign_archive(&archive, &path).unwrap();
        assert_eq!(report.sha256, sha256_hex(&archive));
        signing::verify(&key.verifying_key(), &archive, &report.signature).unwrap();
    }
}
