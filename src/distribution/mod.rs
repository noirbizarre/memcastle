//! Finding, fetching and verifying source packages from the bundle and from registries (docs/adr/033).
//!
//! `crate::source` is what a package *is* and how to make one; `crate::mining::wasm` is how one *runs*; this module is
//! how one *arrives*: reading an index, choosing a version, downloading the archive, and proving it is the archive the
//! index named and that the user's trust policy allows. It returns bytes and says where they came from, and decides
//! nothing about installing them: that is `crate::app`, which is also the only caller, so nothing here touches the
//! store or the jobs (AGENTS.md, invariants 9 and 10).
//!
//! Three kinds of source share the one model. A built-in one is compiled in and never arrives. A *bundled* one is an
//! ordinary package shipped beside the binary, described by an index that ships with it. A *registry* one comes from an
//! index the user configured. The index format and the verification are the same for the last two; only the trust
//! policy differs (a bundled package is as trusted as the MemCastle that carries it).

mod location;
mod trust;

use std::path::PathBuf;

use crate::assets::InstallSearch;
use crate::config::MiningConfig;
use crate::domain::{IndexedSource, IndexedVersion, SourceIndex, SourceOrigin, sha256_hex};
use crate::error::{Error, Result};
use crate::source::package::SourcePackage;

pub use location::Location;
pub use trust::TrustPolicy;

/// The index's file name, in a registry's directory and in the bundle.
pub const INDEX_FILE: &str = "memcastle-index.json";

/// The largest index read: a list of names and digests, not a payload.
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// The largest archive downloaded: the same ceiling as an upload to the daemon, so what can be fetched can be
/// installed.
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// An archive that was fetched and verified.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The archive's bytes.
    pub archive: Vec<u8>,
    /// Its SHA-256, which equals the one the index published.
    pub archive_digest: String,
    /// The id of the trusted key that signed it, when one did.
    pub signed_by: Option<String>,
}

/// One index and where it came from.
#[derive(Debug, Clone)]
pub struct Registry {
    /// The location as configured, which is what a record of an install keeps to find it again.
    pub label: String,
    /// Whether this is the bundle or a registry the user configured.
    pub origin: SourceOrigin,
    /// The index itself.
    pub index: SourceIndex,
    base: Location,
}

fn unavailable(location: &str, message: impl Into<String>) -> Error {
    Error::SourceRegistryUnavailable {
        location: location.to_string(),
        message: message.into(),
    }
}

impl Registry {
    /// Read the index at `label`.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when it cannot be read or is not an index this MemCastle understands.
    pub async fn load(label: &str, origin: SourceOrigin) -> Result<Self> {
        let base = Location::parse(label)
            .map_err(|reason| unavailable(label, reason))?
            .as_index(INDEX_FILE);
        let bytes = base
            .read(MAX_INDEX_BYTES)
            .await
            .map_err(|reason| unavailable(label, reason))?;
        let text =
            String::from_utf8(bytes).map_err(|_| unavailable(label, "it is not UTF-8 text"))?;
        let index = SourceIndex::parse(&text).map_err(|reason| unavailable(label, reason))?;
        Ok(Self {
            label: label.to_string(),
            origin,
            index,
            base,
        })
    }

    /// Download the archive of `entry` (a version of `name`) and verify it.
    ///
    /// The SHA-256 is always checked; the signature is checked as `policy` says, except for the bundle, whose
    /// provenance is the release that carries it.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when the archive cannot be fetched, [`Error::SourceIntegrity`] when it is
    /// not the archive the index named, and [`Error::SourceUntrusted`] when the policy refuses it.
    pub async fn fetch(
        &self,
        name: &str,
        entry: &IndexedVersion,
        policy: &TrustPolicy,
    ) -> Result<Fetched> {
        let source = self
            .base
            .join(&entry.url)
            .map_err(|reason| unavailable(&self.label, reason))?;
        let archive = source
            .read(MAX_ARCHIVE_BYTES)
            .await
            .map_err(|reason| unavailable(&source.to_string(), reason))?;
        let archive_digest = sha256_hex(&archive);
        if archive_digest != entry.sha256 {
            return Err(Error::SourceIntegrity {
                name: name.to_string(),
                message: format!(
                    "{source} has SHA-256 {archive_digest}, and the index published {}",
                    entry.sha256
                ),
            });
        }
        let signed_by = if self.origin == SourceOrigin::Bundled {
            None
        } else {
            policy.check(name, &archive, entry.signature.as_ref())?
        };
        Ok(Fetched {
            archive,
            archive_digest,
            signed_by,
        })
    }
}

/// The package inside an archive must be the one the index said it was, or an index could publish a harmless
/// package's digest under a name that installs something else.
///
/// # Errors
///
/// [`Error::SourceIntegrity`] naming what differs.
pub fn verify_identity(package: &SourcePackage, name: &str, entry: &IndexedVersion) -> Result<()> {
    let manifest = &package.manifest.source;
    if manifest.name != name || manifest.version != entry.version {
        return Err(Error::SourceIntegrity {
            name: name.to_string(),
            message: format!(
                "the index lists it as {name} {}, and the package says it is {} {}",
                entry.version, manifest.name, manifest.version
            ),
        });
    }
    Ok(())
}

/// Where the bundled index is, when this installation has one.
///
/// `mining.bundled_dir` when set; otherwise `sources/` under the first installed asset directory that has an index
/// (the layout of `docs/adr/013`). `None` is normal: a binary run from a build directory ships no bundle.
#[must_use]
pub fn bundled_index(mining: &MiningConfig) -> Option<PathBuf> {
    let directories = match &mining.bundled_dir {
        Some(dir) => vec![dir.clone()],
        None => InstallSearch::from_process()
            .candidates()
            .into_iter()
            .map(|dir| dir.join("sources"))
            .collect(),
    };
    directories
        .into_iter()
        .map(|dir| dir.join(INDEX_FILE))
        .find(|index| index.is_file())
}

/// Every index the daemon consults, in precedence order: the bundle first, then the configured registries.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// The indexes that could be read.
    pub registries: Vec<Registry>,
    /// One line for each index that could not be read: a registry that is down must not hide the others.
    pub warnings: Vec<String>,
}

impl Catalog {
    /// Read the bundle and the configured registries, or only `only` when it is given.
    ///
    /// `only` is an explicit choice (`--registry`), so it replaces the lot rather than adding to it, and a failure to
    /// read it is an error and not a warning.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when `only` cannot be read.
    pub async fn open(mining: &MiningConfig, only: Option<&str>) -> Result<Self> {
        if let Some(location) = only {
            // The one place a location arrives from a request rather than a configuration file: it is checked
            // against the same scheme rules, and what it serves is held to the same trust policy.
            return Ok(Self {
                registries: vec![Registry::load(location, SourceOrigin::Registry).await?],
                warnings: Vec::new(),
            });
        }
        let mut catalog = Self::default();
        if let Some(index) = bundled_index(mining) {
            let label = index.display().to_string();
            match Registry::load(&label, SourceOrigin::Bundled).await {
                Ok(registry) => catalog.registries.push(registry),
                Err(error) => catalog.warnings.push(error.to_string()),
            }
        }
        for location in &mining.registries {
            match Registry::load(location, SourceOrigin::Registry).await {
                Ok(registry) => catalog.registries.push(registry),
                Err(error) => catalog.warnings.push(error.to_string()),
            }
        }
        Ok(catalog)
    }

    /// The first registry that offers `name`, which is where an install of it comes from.
    #[must_use]
    pub fn locate(&self, name: &str) -> Option<(&Registry, &IndexedSource)> {
        self.registries
            .iter()
            .find_map(|registry| registry.index.find(name).map(|source| (registry, source)))
    }

    /// The registry installed from `label`, for an update to ask again.
    #[must_use]
    pub fn by_label(&self, label: &str) -> Option<&Registry> {
        self.registries
            .iter()
            .find(|registry| registry.label == label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CONTRACT_VERSION, IndexedSource};

    fn entry(archive: &[u8]) -> IndexedVersion {
        IndexedVersion {
            version: "1.0.0".into(),
            contract: CONTRACT_VERSION.into(),
            memcastle: ">=0.1".into(),
            url: "demo-1.0.0.tar.gz".into(),
            sha256: sha256_hex(archive),
            size: None,
            signature: None,
            yanked: false,
        }
    }

    fn write_registry(directory: &std::path::Path, archive: &[u8], published: &IndexedVersion) {
        std::fs::write(directory.join("demo-1.0.0.tar.gz"), archive).unwrap();
        let mut index = SourceIndex::new(Some("test".into()));
        index.sources.push(IndexedSource {
            name: "demo".into(),
            description: "demo".into(),
            homepage: None,
            license: None,
            versions: vec![published.clone()],
        });
        std::fs::write(
            directory.join(INDEX_FILE),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_archive_matching_the_index_is_fetched_from_a_directory_registry() {
        let directory = tempfile::tempdir().unwrap();
        let published = entry(b"archive");
        write_registry(directory.path(), b"archive", &published);

        let registry = Registry::load(directory.path().to_str().unwrap(), SourceOrigin::Registry)
            .await
            .unwrap();
        let (found, source) = (
            registry.index.find("demo").is_some(),
            registry.index.find("demo").unwrap(),
        );
        assert!(found);
        let policy = TrustPolicy::from_config(&MiningConfig::default()).unwrap();
        let fetched = registry
            .fetch("demo", &source.versions[0], &policy)
            .await
            .unwrap();
        assert_eq!(fetched.archive, b"archive");
        assert_eq!(fetched.archive_digest, published.sha256);
        assert_eq!(fetched.signed_by, None);
    }

    #[tokio::test]
    async fn an_archive_that_is_not_the_one_the_index_named_is_an_integrity_failure() {
        let directory = tempfile::tempdir().unwrap();
        let published = entry(b"archive");
        // The registry serves different bytes than it published.
        write_registry(directory.path(), b"substituted", &published);

        let registry = Registry::load(directory.path().to_str().unwrap(), SourceOrigin::Registry)
            .await
            .unwrap();
        let policy = TrustPolicy::from_config(&MiningConfig::default()).unwrap();
        let error = registry
            .fetch("demo", &published, &policy)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::SourceIntegrity { .. }), "{error}");
    }

    #[tokio::test]
    async fn an_unreadable_or_malformed_index_names_its_location() {
        let directory = tempfile::tempdir().unwrap();
        let missing = Registry::load(directory.path().to_str().unwrap(), SourceOrigin::Registry)
            .await
            .unwrap_err();
        assert!(
            matches!(missing, Error::SourceRegistryUnavailable { .. }),
            "{missing}"
        );

        std::fs::write(directory.path().join(INDEX_FILE), "{ not json").unwrap();
        let malformed = Registry::load(directory.path().to_str().unwrap(), SourceOrigin::Registry)
            .await
            .unwrap_err();
        assert!(
            malformed
                .to_string()
                .contains(directory.path().to_str().unwrap()),
            "{malformed}"
        );
    }

    #[tokio::test]
    async fn one_registry_being_down_does_not_hide_the_others() {
        let directory = tempfile::tempdir().unwrap();
        write_registry(directory.path(), b"archive", &entry(b"archive"));
        let mining = MiningConfig {
            registries: vec![
                directory.path().join("gone").to_str().unwrap().to_string(),
                directory.path().to_str().unwrap().to_string(),
            ],
            bundled_dir: Some(directory.path().join("no-bundle")),
            ..MiningConfig::default()
        };

        let catalog = Catalog::open(&mining, None).await.unwrap();
        assert_eq!(catalog.registries.len(), 1);
        assert_eq!(catalog.warnings.len(), 1);
        assert!(catalog.locate("demo").is_some() && catalog.locate("other").is_none());
    }

    #[tokio::test]
    async fn the_bundle_comes_first_and_is_found_in_its_directory() {
        let bundle = tempfile::tempdir().unwrap();
        let registry = tempfile::tempdir().unwrap();
        write_registry(bundle.path(), b"archive", &entry(b"archive"));
        write_registry(registry.path(), b"archive", &entry(b"archive"));
        let mining = MiningConfig {
            registries: vec![registry.path().to_str().unwrap().to_string()],
            bundled_dir: Some(bundle.path().to_path_buf()),
            ..MiningConfig::default()
        };

        let catalog = Catalog::open(&mining, None).await.unwrap();
        let (first, _) = catalog.locate("demo").unwrap();
        assert_eq!(first.origin, SourceOrigin::Bundled);
        assert_eq!(catalog.registries.len(), 2);
    }

    #[tokio::test]
    async fn an_explicitly_chosen_registry_replaces_the_rest_and_must_be_readable() {
        let bundle = tempfile::tempdir().unwrap();
        write_registry(bundle.path(), b"archive", &entry(b"archive"));
        let mining = MiningConfig {
            bundled_dir: Some(bundle.path().to_path_buf()),
            ..MiningConfig::default()
        };
        let nowhere = bundle.path().join("nowhere");
        let error = Catalog::open(&mining, Some(nowhere.to_str().unwrap()))
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::SourceRegistryUnavailable { .. }),
            "{error}"
        );
    }
}
