//! Finding the integrations an installation ships.
//!
//! One code path serves a package and a checkout. Both are an assets root with `integrations/<id>/` beneath it, so the
//! only difference is how the root is chosen, and that is [`crate::assets::Assets`]'s job: `--assets-dir` (or
//! `MEMCASTLE_ASSETS_DIR`, or `assets.dir`), else the installed package, else nothing.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::assets::{AssetSource, Assets, InstallSearch};
use crate::error::{Error, Result};

use super::manifest::{self, IntegrationManifest, MANIFEST_FILE};

/// The directory under the assets root that holds the integrations.
pub const INTEGRATIONS_DIR: &str = "integrations";

/// The directory under the assets root that holds the shared skills.
pub const SKILLS_DIR: &str = "skills";

/// One integration found under the assets root.
#[derive(Debug, Clone)]
pub struct Shipped {
    /// Its manifest.
    pub manifest: IntegrationManifest,
    /// `<root>/integrations/<id>`.
    pub dir: PathBuf,
}

impl Shipped {
    /// The identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.manifest.integration.id
    }
}

/// A directory under `integrations/` whose manifest could not be used.
#[derive(Debug, Clone, Serialize)]
pub struct Broken {
    /// The directory's name.
    pub id: String,
    /// Why the manifest was refused.
    pub message: String,
}

/// Every integration under one assets root.
#[derive(Debug, Clone)]
pub struct Catalog {
    root: PathBuf,
    source: AssetSource,
    integrations: Vec<Shipped>,
    broken: Vec<Broken>,
}

impl Catalog {
    /// Open the catalog of the running process: `override_dir`, else the installed package.
    ///
    /// # Errors
    ///
    /// [`Error::AssetsNotFound`] when `override_dir` is not a directory, [`Error::IntegrationAssetsMissing`] when
    /// there is no assets directory at all (a binary run on its own ships nothing).
    pub fn open(override_dir: Option<&Path>) -> Result<Self> {
        Self::open_with(override_dir, &InstallSearch::from_process())
    }

    /// [`Catalog::open`] against explicit search inputs, for tests.
    ///
    /// # Errors
    ///
    /// As [`Catalog::open`].
    pub fn open_with(override_dir: Option<&Path>, search: &InstallSearch) -> Result<Self> {
        let assets = Assets::resolve(override_dir, search)?;
        let Some(root) = assets.root() else {
            return Err(Error::IntegrationAssetsMissing {
                message: "this MemCastle was not installed from a package that ships integrations, and no \
                          `--assets-dir`, `MEMCASTLE_ASSETS_DIR` or `assets.dir` names a directory that does"
                    .to_string(),
            });
        };
        Self::read(root.to_path_buf(), assets.source().clone())
    }

    /// Read the integrations under `root`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when `integrations/` exists and cannot be read.
    pub fn read(root: PathBuf, source: AssetSource) -> Result<Self> {
        let directory = root.join(INTEGRATIONS_DIR);
        let mut integrations = Vec::new();
        let mut broken = Vec::new();
        // No `integrations/` is an empty catalog, not an error: `list` then says so and `install` names the root.
        if directory.is_dir() {
            let entries = std::fs::read_dir(&directory)
                .map_err(|e| Error::io(directory.display().to_string(), e))?;
            for entry in entries {
                let entry = entry.map_err(|e| Error::io(directory.display().to_string(), e))?;
                let dir = entry.path();
                let manifest_path = dir.join(MANIFEST_FILE);
                // `integrations/common` and the like hold no manifest: they are not integrations, and are skipped.
                if !manifest_path.is_file() {
                    continue;
                }
                let id = entry.file_name().to_string_lossy().into_owned();
                match Self::load_one(&id, &dir, &manifest_path) {
                    Ok(manifest) => integrations.push(Shipped { manifest, dir }),
                    Err(message) => broken.push(Broken { id, message }),
                }
            }
        }
        // A directory listing has no defined order, and `list` output must not change between runs.
        integrations.sort_by(|a, b| a.id().cmp(b.id()));
        broken.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Self {
            root,
            source,
            integrations,
            broken,
        })
    }

    fn load_one(
        id: &str,
        dir: &Path,
        manifest_path: &Path,
    ) -> std::result::Result<IntegrationManifest, String> {
        let text = std::fs::read_to_string(manifest_path)
            .map_err(|e| format!("{} cannot be read: {e}", manifest_path.display()))?;
        let manifest = manifest::parse(&text).map_err(|e| e.to_string())?;
        // The directory is the id's only other spelling; letting them differ would make `list` and the files disagree.
        if manifest.integration.id != id {
            return Err(format!(
                "{} says its id is `{}`, but its directory {} is named `{id}`",
                manifest_path.display(),
                manifest.integration.id,
                dir.display()
            ));
        }
        Ok(manifest)
    }

    /// The assets root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How the root was chosen.
    #[must_use]
    pub const fn source(&self) -> &AssetSource {
        &self.source
    }

    /// The shared skills directory: `<root>/skills`.
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join(SKILLS_DIR)
    }

    /// The usable integrations, by id.
    #[must_use]
    pub fn integrations(&self) -> &[Shipped] {
        &self.integrations
    }

    /// The integrations whose manifest was refused.
    #[must_use]
    pub fn broken(&self) -> &[Broken] {
        &self.broken
    }

    /// The integration named `id`.
    ///
    /// # Errors
    ///
    /// [`Error::IntegrationManifestInvalid`] when its manifest was refused, [`Error::IntegrationNotFound`] when
    /// nothing by that name is shipped.
    pub fn get(&self, id: &str) -> Result<&Shipped> {
        if let Some(shipped) = self.integrations.iter().find(|s| s.id() == id) {
            return Ok(shipped);
        }
        if let Some(broken) = self.broken.iter().find(|b| b.id == id) {
            return Err(Error::IntegrationManifestInvalid {
                message: broken.message.clone(),
            });
        }
        Err(Error::IntegrationNotFound {
            name: id.to_string(),
            root: self.root.display().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest for `id`, valid unless an edit breaks it.
    pub(crate) fn manifest_text(id: &str) -> String {
        format!(
            r#"format = 1
[integration]
id = "{id}"
version = "0.1.0"
description = "demo"
[compatibility]
memcastle = ">=0.1"
[agent]
kind = "pi"
[[assets]]
from = "dist"
to = "."
"#
        )
    }

    fn tree(ids: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for id in ids {
            let dir = root.path().join(INTEGRATIONS_DIR).join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(MANIFEST_FILE), manifest_text(id)).unwrap();
        }
        root
    }

    fn read(root: &tempfile::TempDir) -> Catalog {
        Catalog::read(
            root.path().to_path_buf(),
            AssetSource::Override(root.path().to_path_buf()),
        )
        .unwrap()
    }

    #[test]
    fn every_directory_with_a_manifest_is_discovered_in_a_stable_order() {
        let root = tree(&["pi", "opencode"]);
        // `common` holds no manifest, like the test-only package in a checkout.
        std::fs::create_dir_all(root.path().join("integrations/common")).unwrap();

        let catalog = read(&root);

        let ids: Vec<_> = catalog.integrations().iter().map(Shipped::id).collect();
        assert_eq!(ids, ["opencode", "pi"]);
        assert!(catalog.broken().is_empty());
    }

    #[test]
    fn a_root_without_integrations_is_empty_and_names_itself_when_asked_for_one() {
        let root = tempfile::tempdir().unwrap();

        let catalog = read(&root);

        assert!(catalog.integrations().is_empty());
        let error = catalog.get("pi").unwrap_err();
        assert!(
            matches!(error, Error::IntegrationNotFound { .. }),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains(&root.path().display().to_string())
        );
    }

    #[test]
    fn a_broken_manifest_is_reported_for_its_own_integration_and_hides_no_other() {
        let root = tree(&["pi"]);
        let broken = root.path().join("integrations/opencode");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(MANIFEST_FILE), "format = 1\n[nope]\n").unwrap();

        let catalog = read(&root);

        assert!(catalog.get("pi").is_ok());
        assert!(matches!(
            catalog.get("opencode").unwrap_err(),
            Error::IntegrationManifestInvalid { .. }
        ));
    }

    #[test]
    fn a_manifest_whose_id_differs_from_its_directory_is_refused() {
        let root = tree(&["pi"]);
        let dir = root.path().join("integrations/pi");
        std::fs::write(dir.join(MANIFEST_FILE), manifest_text("other")).unwrap();

        let catalog = read(&root);

        let message = catalog.get("pi").unwrap_err().to_string();
        assert!(
            message.contains("`other`") && message.contains("`pi`"),
            "{message}"
        );
    }

    #[test]
    fn an_explicit_directory_is_the_root_whether_a_package_or_a_checkout() {
        let root = tree(&["pi"]);
        let search = InstallSearch::default();

        let catalog = Catalog::open_with(Some(root.path()), &search).unwrap();

        assert_eq!(catalog.root(), root.path());
        assert_eq!(
            catalog.source(),
            &AssetSource::Override(root.path().to_path_buf())
        );
        assert_eq!(catalog.skills_dir(), root.path().join("skills"));
    }

    #[test]
    fn an_installed_package_is_found_when_nothing_is_overridden() {
        let prefix = tempfile::tempdir().unwrap();
        let share = prefix.path().join("share/memcastle");
        std::fs::create_dir_all(prefix.path().join("bin")).unwrap();
        std::fs::create_dir_all(share.join("integrations/pi")).unwrap();
        std::fs::write(
            share.join("integrations/pi").join(MANIFEST_FILE),
            manifest_text("pi"),
        )
        .unwrap();
        let search = InstallSearch {
            exe_dir: Some(prefix.path().join("bin")),
            ..InstallSearch::default()
        };

        let catalog = Catalog::open_with(None, &search).unwrap();

        assert_eq!(catalog.integrations().len(), 1);
        assert_eq!(catalog.source(), &AssetSource::Installed(share));
    }

    #[test]
    fn a_binary_with_no_package_and_no_override_says_how_to_get_integrations() {
        let error = Catalog::open_with(None, &InstallSearch::default()).unwrap_err();

        assert!(
            matches!(error, Error::IntegrationAssetsMissing { .. }),
            "{error}"
        );
    }

    #[test]
    fn an_override_that_is_not_a_directory_is_an_error_and_never_falls_through() {
        let error = Catalog::open_with(
            Some(Path::new("/nonexistent-assets")),
            &InstallSearch::default(),
        )
        .unwrap_err();

        assert!(matches!(error, Error::AssetsNotFound { .. }), "{error}");
    }
}
