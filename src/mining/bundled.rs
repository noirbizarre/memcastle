//! The sources that ship with MemCastle, installed from the start (docs/adr/039).
//!
//! A bundled source is an ordinary package, unpacked under `share/memcastle/sources/<name>/` beside the binary: the
//! same `memcastle-source.toml` and `source.wasm` an installed one has, and the host runs it from where it is. It is
//! never copied into `mining.sources_dir()`, so there is nothing to install, update or remove: what is bundled is what
//! this release carries, and it is as trusted as the MemCastle that carries it. Only whether it is enabled is the
//! user's, and that is the one thing the database keeps (`crate::app`).
//!
//! This module reads the bundle and nothing else: no store, no jobs, no network.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::assets::InstallSearch;
use crate::config::MiningConfig;
use crate::domain::{
    SourceManifest, SourceOrigin, SourcePackageRecord, SourcePackageState, sha256_hex,
};
use crate::error::Result;
use crate::source::manifest;
use crate::source::package::{installed_names, read_installed_component, read_installed_manifest};

use super::registry::BUILTIN_NAMES;

/// The directory of bundled packages of this installation.
#[derive(Debug, Clone)]
pub struct Bundle {
    root: PathBuf,
}

/// A bundled package as read from the bundle.
#[derive(Debug, Clone)]
pub struct BundledSource {
    /// The manifest the release carries.
    pub manifest: SourceManifest,
    /// SHA-256 of the component the release carries.
    pub digest: String,
}

impl Bundle {
    /// The bundle this installation has, if any.
    ///
    /// `mining.bundled_dir` when set; otherwise `sources/` under the first installed asset directory that holds a
    /// package (the layout of `docs/adr/013`). `None` is normal: a binary run from a build directory ships no bundle,
    /// and a development worktree's `sources/` holds projects, which have no `source.wasm` of their own.
    #[must_use]
    pub fn find(mining: &MiningConfig) -> Option<Self> {
        let roots = match &mining.bundled_dir {
            Some(dir) => vec![dir.clone()],
            None => InstallSearch::from_process()
                .candidates()
                .into_iter()
                .map(|dir| dir.join("sources"))
                .collect(),
        };
        roots
            .into_iter()
            .find(|root| !installed_names(root).is_empty())
            .map(|root| Self { root })
    }

    /// Where the bundle is.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The names of the bundled sources, in order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        installed_names(&self.root)
    }

    /// Whether the bundle carries `name`.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.names().iter().any(|candidate| candidate == name)
    }

    /// Read `name` from the bundle.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Io`] when a file cannot be read and [`crate::Error::SourceManifestInvalid`] when the manifest
    /// is wrong: the release is broken, and the message says which file.
    pub fn read(&self, name: &str) -> Result<BundledSource> {
        let text = read_installed_manifest(&self.root, name)?;
        let component = read_installed_component(&self.root, name)?;
        Ok(BundledSource {
            // The same validation an installed package gets, including the names a source may not take.
            manifest: manifest::parse(&text, &BUILTIN_NAMES)?,
            digest: sha256_hex(&component),
        })
    }

    /// `name` as an installed record: the bundle's manifest and component, and the state the user left it in.
    ///
    /// `stored` is the database's row for the name, which supplies only the state: the manifest and digest are the
    /// bundle's on every look, so a new MemCastle that carries a new version of a source simply runs it, with no
    /// migration and no stale copy.
    ///
    /// # Errors
    ///
    /// As [`Self::read`].
    pub fn record(
        &self,
        name: &str,
        stored: Option<&SourcePackageRecord>,
    ) -> Result<SourcePackageRecord> {
        let source = self.read(name)?;
        let now = Utc::now();
        Ok(SourcePackageRecord {
            name: name.to_string(),
            // Installed from the start, but not enabled: turning a source on is still the user's decision.
            state: stored.map_or(SourcePackageState::Installed, |record| record.state),
            digest: source.digest,
            manifest: source.manifest,
            installed_at: stored.map_or(now, |record| record.installed_at),
            updated_at: stored.map_or(now, |record| record.updated_at),
            origin: SourceOrigin::Bundled,
            registry: None,
            archive_digest: None,
            signed_by: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{COMPONENT_FILE, MANIFEST_FILE};

    const MANIFEST: &str = r#"
[source]
name = "demo"
version = "1.2.0"
description = "demo"

[compatibility]
contract = "0.2"
memcastle = ">=0.1"
"#;

    fn bundle_with(name: &str) -> (tempfile::TempDir, MiningConfig) {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(MANIFEST_FILE), MANIFEST).unwrap();
        std::fs::write(package.join(COMPONENT_FILE), b"\0asm\x0d\0\x01\0").unwrap();
        let mining = MiningConfig {
            bundled_dir: Some(dir.path().to_path_buf()),
            ..MiningConfig::default()
        };
        (dir, mining)
    }

    #[test]
    fn a_directory_of_unpacked_packages_is_a_bundle_and_lists_them_by_name() {
        let (_dir, mining) = bundle_with("demo");
        let bundle = Bundle::find(&mining).unwrap();

        assert_eq!(bundle.names(), ["demo"]);
        assert!(bundle.contains("demo") && !bundle.contains("other"));
    }

    #[test]
    fn a_directory_of_source_projects_with_no_component_is_not_a_bundle() {
        // A checkout's `sources/` holds projects: a manifest, and the component only under `dist/` once built.
        let (dir, mining) = bundle_with("demo");
        std::fs::remove_file(dir.path().join("demo").join(COMPONENT_FILE)).unwrap();

        assert!(Bundle::find(&mining).is_none());
    }

    #[test]
    fn a_bundled_record_takes_its_manifest_and_digest_from_the_bundle_and_only_its_state_from_the_row()
     {
        let (_dir, mining) = bundle_with("demo");
        let bundle = Bundle::find(&mining).unwrap();

        let fresh = bundle.record("demo", None).unwrap();
        assert_eq!(fresh.state, SourcePackageState::Installed);
        assert_eq!(fresh.origin, SourceOrigin::Bundled);
        assert_eq!(fresh.manifest.source.version, "1.2.0");
        assert_eq!(fresh.digest, sha256_hex(b"\0asm\x0d\0\x01\0"));

        // An older release's row carries an older manifest and digest; none of that is read.
        let mut stored = fresh.clone();
        stored.state = SourcePackageState::Enabled;
        stored.manifest.source.version = "0.1.0".to_string();
        stored.digest = "stale".to_string();
        let current = bundle.record("demo", Some(&stored)).unwrap();
        assert_eq!(current.state, SourcePackageState::Enabled);
        assert_eq!(current.manifest.source.version, "1.2.0");
        assert_eq!(current.digest, fresh.digest);
    }

    #[test]
    fn a_bundled_manifest_that_is_wrong_is_an_error_naming_it() {
        let (dir, mining) = bundle_with("demo");
        std::fs::write(dir.path().join("demo").join(MANIFEST_FILE), "not = [valid").unwrap();
        let bundle = Bundle::find(&mining).unwrap();

        assert!(bundle.read("demo").is_err());
    }
}
