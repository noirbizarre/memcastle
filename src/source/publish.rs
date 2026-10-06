//! Publishing: turning package archives into a registry index (docs/adr/033).
//!
//! A registry is a directory of archives and one `memcastle-index.json` that names them, so publishing is "add this
//! archive to the index and put both somewhere": no service, no account. This module is the first half, on a
//! developer's machine, with no daemon (AGENTS.md, invariant 1); reading an index to install from it is
//! `crate::distribution`.

use std::path::Path;

use ed25519_dalek::SigningKey;

use crate::domain::{IndexedSource, IndexedVersion, SourceIndex, sha256_hex};
use crate::error::{Error, Result};

use super::{package, signing};

fn invalid(message: impl Into<String>) -> Error {
    Error::SourcePackageInvalid {
        message: message.into(),
    }
}

/// Read the index at `path`, or start an empty one when there is none yet.
///
/// # Errors
///
/// [`Error::Io`] when it exists and cannot be read, and [`Error::SourceRegistryUnavailable`] when it is not an index.
pub fn read_index(path: &Path, name: Option<&str>) -> Result<SourceIndex> {
    match std::fs::read_to_string(path) {
        Ok(text) => SourceIndex::parse(&text).map_err(|message| Error::SourceRegistryUnavailable {
            location: path.display().to_string(),
            message,
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(SourceIndex::new(name.map(str::to_string)))
        }
        Err(source) => Err(Error::io(path.display().to_string(), source)),
    }
}

/// Write `index` to `path`, sources by name and versions newest first, so republishing gives a small diff.
///
/// # Errors
///
/// [`Error::Io`] when it cannot be written.
pub fn write_index(path: &Path, index: &mut SourceIndex) -> Result<()> {
    index.sources.sort_by(|a, b| a.name.cmp(&b.name));
    for source in &mut index.sources {
        source
            .versions
            .sort_by_key(|entry| std::cmp::Reverse(semver::Version::parse(&entry.version).ok()));
    }
    let mut text = serde_json::to_string_pretty(index)
        .map_err(|source| Error::serialization("the registry index", source))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|source| Error::io(path.display().to_string(), source))
}

/// The URL an archive is published at: under `base_url` when there is one, otherwise beside the index.
#[must_use]
pub fn archive_url(base_url: Option<&str>, file_name: &str) -> String {
    match base_url {
        Some(base) => format!("{}/{file_name}", base.trim_end_matches('/')),
        None => file_name.to_string(),
    }
}

/// Add the package in `archive` to `index`, as published at `url` and signed with `key` when there is one.
///
/// Answers the name and version added. Publishing the same archive again refreshes its entry, so the command can be
/// rerun; publishing a *different* archive under a version that is already listed is refused, because a version is
/// what users pin and trust, and an index that quietly changed what it meant would defeat the digest.
///
/// # Errors
///
/// [`Error::SourcePackageInvalid`] for an archive that is not a package, or one that contradicts an entry.
pub fn add_archive(
    index: &mut SourceIndex,
    archive: &[u8],
    url: &str,
    key: Option<&SigningKey>,
) -> Result<(String, String)> {
    let package = package::inspect(archive)?;
    let manifest = &package.manifest;
    let name = manifest.source.name.clone();
    let version = manifest.source.version.clone();
    let entry = IndexedVersion {
        version: version.clone(),
        contract: Some(manifest.compatibility.contract.clone()),
        memcastle: Some(manifest.compatibility.memcastle.clone()),
        url: url.to_string(),
        sha256: sha256_hex(archive),
        size: Some(archive.len() as u64),
        signature: key.map(|key| signing::sign(key, archive)),
        yanked: false,
    };

    let position = if let Some(position) = index.sources.iter().position(|s| s.name == name) {
        position
    } else {
        index.sources.push(IndexedSource {
            name: name.clone(),
            description: manifest.source.description.clone(),
            homepage: None,
            license: None,
            repository: None,
            versions: Vec::new(),
        });
        index.sources.len() - 1
    };
    let source = &mut index.sources[position];
    // The newest manifest speaks for the source; an older one republished later must not revert what it says.
    let newest = source
        .versions
        .iter()
        .all(|other| other_is_not_newer(&other.version, &version));
    if newest {
        source.description.clone_from(&manifest.source.description);
        source.homepage.clone_from(&manifest.source.homepage);
        source.license.clone_from(&manifest.source.license);
    }
    match source
        .versions
        .iter_mut()
        .find(|other| other.version == version)
    {
        Some(existing) if existing.sha256 != entry.sha256 => {
            return Err(invalid(format!(
                "version {version} of `{name}` is already published with a different archive; \
                 a version never changes once published, so bump `source.version` and package again"
            )));
        }
        Some(existing) => {
            // The same archive: keep a withdrawal, refresh the rest (a new signature, a new location).
            let yanked = existing.yanked;
            *existing = IndexedVersion { yanked, ..entry };
        }
        None => source.versions.push(entry),
    }
    index.validate().map_err(invalid)?;
    Ok((name, version))
}

fn other_is_not_newer(other: &str, version: &str) -> bool {
    match (
        semver::Version::parse(other),
        semver::Version::parse(version),
    ) {
        (Ok(other), Ok(version)) => other <= version,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(version: &str, extra: &str) -> Vec<u8> {
        let manifest = format!(
            "[source]\nname = \"demo\"\nversion = \"{version}\"\ndescription = \"Reads demo {extra}\"\nlicense = \"MIT\"\n\n\
             [compatibility]\ncontract = \"{}\"\nmemcastle = \">=0.1\"\n",
            crate::domain::CONTRACT_VERSION
        );
        package::pack(&manifest, b"\0asm\x0d\0\x01\0 body", &[]).unwrap()
    }

    #[test]
    fn publishing_an_archive_records_what_the_index_needs_to_choose_and_verify_it() {
        let mut index = SourceIndex::new(None);
        let bytes = archive("1.0.0", "");
        let (name, version) = add_archive(&mut index, &bytes, "demo-1.0.0.tar.gz", None).unwrap();

        assert_eq!((name.as_str(), version.as_str()), ("demo", "1.0.0"));
        let entry = &index.find("demo").unwrap().versions[0];
        assert_eq!(entry.sha256, sha256_hex(&bytes));
        assert_eq!(entry.size, Some(bytes.len() as u64));
        assert_eq!(
            entry.contract.as_deref(),
            Some(crate::domain::CONTRACT_VERSION)
        );
        assert_eq!(index.find("demo").unwrap().license.as_deref(), Some("MIT"));
    }

    #[test]
    fn a_signed_archive_carries_a_signature_that_verifies_against_the_key() {
        let key = signing::generate().unwrap();
        let mut index = SourceIndex::new(None);
        let bytes = archive("1.0.0", "");
        add_archive(&mut index, &bytes, "u", Some(&key)).unwrap();

        let signature = index.find("demo").unwrap().versions[0]
            .signature
            .clone()
            .unwrap();
        assert!(signing::verify(&key.verifying_key(), &bytes, &signature).is_ok());
    }

    #[test]
    fn publishing_the_same_archive_again_is_idempotent_and_keeps_a_withdrawal() {
        let mut index = SourceIndex::new(None);
        let bytes = archive("1.0.0", "");
        add_archive(&mut index, &bytes, "old-url", None).unwrap();
        index.sources[0].versions[0].yanked = true;
        add_archive(&mut index, &bytes, "new-url", None).unwrap();

        let versions = &index.find("demo").unwrap().versions;
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].url, "new-url");
        assert!(versions[0].yanked);
    }

    #[test]
    fn a_different_archive_under_a_published_version_is_refused() {
        let mut index = SourceIndex::new(None);
        add_archive(&mut index, &archive("1.0.0", ""), "u", None).unwrap();
        let error = add_archive(&mut index, &archive("1.0.0", "changed"), "u", None).unwrap_err();
        assert!(error.to_string().contains("never changes"), "{error}");
    }

    #[test]
    fn an_older_version_published_later_does_not_rewrite_what_the_source_says() {
        let mut index = SourceIndex::new(None);
        add_archive(&mut index, &archive("2.0.0", "newer"), "u2", None).unwrap();
        add_archive(&mut index, &archive("1.0.0", "older"), "u1", None).unwrap();
        assert!(index.find("demo").unwrap().description.contains("newer"));
        assert_eq!(index.find("demo").unwrap().versions.len(), 2);
    }

    #[test]
    fn something_that_is_not_a_package_is_not_published() {
        let mut index = SourceIndex::new(None);
        assert!(add_archive(&mut index, b"not an archive", "u", None).is_err());
        assert!(index.sources.is_empty());
    }

    #[test]
    fn an_index_round_trips_through_a_file_with_versions_newest_first() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memcastle-index.json");
        let mut index = read_index(&path, Some("mine")).unwrap();
        add_archive(&mut index, &archive("1.9.0", ""), "a", None).unwrap();
        add_archive(&mut index, &archive("1.10.0", ""), "b", None).unwrap();
        write_index(&path, &mut index).unwrap();

        let again = read_index(&path, None).unwrap();
        let versions: Vec<_> = again
            .find("demo")
            .unwrap()
            .versions
            .iter()
            .map(|v| v.version.as_str())
            .collect();
        assert_eq!(versions, ["1.10.0", "1.9.0"]);
        assert_eq!(again.name.as_deref(), Some("mine"));
    }

    #[test]
    fn urls_are_joined_to_a_base_or_left_relative() {
        assert_eq!(
            archive_url(Some("https://x.test/s/"), "a.tar.gz"),
            "https://x.test/s/a.tar.gz"
        );
        assert_eq!(archive_url(None, "a.tar.gz"), "a.tar.gz");
    }
}
