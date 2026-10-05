//! Runtime asset resolution: where MemCastle finds files that are not user
//! data.
//!
//! There are three kinds of things a release carries (see
//! `docs/adr/013-release-packaging-and-asset-resolution.md`):
//!
//! 1. *Embedded* assets, compiled into the binary because they are small and
//!    must match its version exactly. The SurrealDB schema and the data
//!    migrations are of this kind, but they are compiled in through
//!    `surrealkit::embed_schema!` and [`crate::migrate`], not through this
//!    module: they are never files, so nothing here can be overridden into
//!    disagreeing with the binary.
//! 2. *Installed* assets, which an OS package puts under a standard prefix
//!    (`/usr/share/memcastle/`) and which the package manager updates.
//! 3. *User data and configuration*, under the XDG directories. Never an asset.
//!
//! [`Assets::resolve`] picks one source with a fixed precedence — an explicit
//! override, then the installed directory, then the embedded fallback — so
//! that a developer can serve a local build of a web UI while a packaged or
//! standalone release stays deterministic. Nothing here touches the network:
//! a daemon must start with no Internet access.
//!
//! The search inputs are passed in ([`InstallSearch`]) rather than read from
//! the process, so tests can cover every layout without depending on where
//! the test binary happens to live.

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

use crate::config::paths::{self, XdgDir};

/// The directory name MemCastle's assets use under a prefix's `share`.
const SHARE_DIR: &str = "memcastle";

/// System-wide directories an OS package may install assets into, most
/// specific first: `/usr/local` is where an administrator's own installs go,
/// and must win over a distribution package.
#[cfg(unix)]
const SYSTEM_SHARE_DIRS: &[&str] = &["/usr/local/share/memcastle", "/usr/share/memcastle"];
/// Windows has no standard shared-data prefix; an installed layout there is
/// found through the executable's own prefix alone.
#[cfg(not(unix))]
const SYSTEM_SHARE_DIRS: &[&str] = &[];

/// The directory under the assets root holding the built dashboard.
///
/// The same relative path in a packaged installation (`<prefix>/share/memcastle/web/dist`) and in a worktree
/// (`<checkout>/web/dist`), so one lookup serves both (docs/adr/035).
pub const WEB_DIST_DIR: &str = "web/dist";
/// The dashboard's entry point, relative to the assets root.
pub const WEB_INDEX: &str = "web/dist/index.html";

/// Assets compiled into the binary, as `(relative path, bytes)`.
///
/// The only version-coupled runtime data (the schema and the data migrations) is embedded by other means and is
/// not a file asset. What is here is the page served in place of the dashboard when it is enabled but its build is
/// absent: a built `web/dist/index.html` found in the chosen directory always outranks it, and it is the one
/// embedded asset because a page that names the remedy must exist precisely when the real files do not.
const EMBEDDED: &[(&str, &[u8])] = &[(WEB_INDEX, include_bytes!("web-unavailable.html"))];

/// Where the assets in use come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetSource {
    /// The directory the operator chose (`--assets-dir`, `MEMCASTLE_ASSETS_DIR`
    /// or `assets.dir`).
    Override(PathBuf),
    /// The directory an OS package installed.
    Installed(PathBuf),
    /// Only what is compiled into the binary.
    Embedded,
}

/// Where to look for package-installed assets.
#[derive(Debug, Clone, Default)]
pub struct InstallSearch {
    /// The directory holding the running executable (`/usr/bin`), whose
    /// sibling `share/memcastle` is the prefix-relative layout.
    pub exe_dir: Option<PathBuf>,
    /// System-wide directories to try after the prefix-relative one.
    pub system_dirs: Vec<PathBuf>,
    /// MemCastle's XDG data directory. Candidates at or under it are
    /// discarded: that is where user data lives, and an asset directory that
    /// overlapped it would make the user's palace look like package content.
    pub user_data_dir: Option<PathBuf>,
}

impl InstallSearch {
    /// The search for the running process.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            // Canonicalised so a symlink such as Homebrew's `bin/memcastle`
            // resolves to the prefix the package really installed into.
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.canonicalize().ok().or(Some(exe)))
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            system_dirs: SYSTEM_SHARE_DIRS.iter().map(PathBuf::from).collect(),
            user_data_dir: paths::resolve(
                XdgDir::Data,
                |name| std::env::var(name).ok(),
                dirs::home_dir().as_deref(),
            ),
        }
    }

    /// The directories an installed package may have used, in precedence
    /// order, with any that overlap user data removed.
    ///
    /// The executable's prefix comes first so a relocated install (an
    /// unpacked tarball, a Homebrew cellar) finds its own assets before a
    /// different copy elsewhere on the machine. Without the user-data
    /// exclusion, `~/.local/bin/memcastle` would resolve to
    /// `~/.local/share/memcastle`, which is the user's data directory.
    #[must_use]
    pub fn candidates(&self) -> Vec<PathBuf> {
        let prefix_relative = self
            .exe_dir
            .as_deref()
            .and_then(Path::parent)
            .map(|prefix| prefix.join("share").join(SHARE_DIR));
        prefix_relative
            .into_iter()
            .chain(self.system_dirs.iter().cloned())
            .filter(|dir| {
                self.user_data_dir
                    .as_deref()
                    .is_none_or(|data| !dir.starts_with(data))
            })
            .collect()
    }
}

/// A file found by [`Assets::find`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asset {
    /// A file on disk, from the override or installed directory.
    File(PathBuf),
    /// Bytes compiled into the binary.
    Embedded(&'static [u8]),
}

/// The resolved asset source for one daemon run.
#[derive(Debug, Clone)]
pub struct Assets {
    source: AssetSource,
}

impl Assets {
    /// Resolve the source: `override_dir` if given, else the first installed
    /// directory that exists, else the embedded assets.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AssetsNotFound`] when `override_dir` is not a
    /// directory. An explicit choice never falls through to another source,
    /// because a typo would then go unnoticed.
    pub fn resolve(override_dir: Option<&Path>, search: &InstallSearch) -> Result<Self> {
        if let Some(dir) = override_dir {
            if !dir.is_dir() {
                return Err(Error::assets_not_found(dir.display().to_string()));
            }
            return Ok(Self {
                source: AssetSource::Override(dir.to_path_buf()),
            });
        }
        // Only the first existing directory is used, not a merge of several:
        // a lookup that depended on which of two directories held a file
        // would differ between machines.
        let source = search
            .candidates()
            .into_iter()
            .find(|dir| dir.is_dir())
            .map_or(AssetSource::Embedded, AssetSource::Installed);
        Ok(Self { source })
    }

    /// Resolve against the running process's own layout.
    ///
    /// # Errors
    ///
    /// As [`Assets::resolve`].
    pub fn resolve_for_process(override_dir: Option<&Path>) -> Result<Self> {
        Self::resolve(override_dir, &InstallSearch::from_process())
    }

    /// Where the assets come from.
    #[must_use]
    pub const fn source(&self) -> &AssetSource {
        &self.source
    }

    /// The directory the assets come from, or `None` when only the embedded ones exist.
    ///
    /// This is the one data root: `sources/`, `integrations/` and `skills/` are all found beneath it, in a packaged
    /// installation and in a development worktree alike (docs/adr/034).
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        match &self.source {
            AssetSource::Override(dir) | AssetSource::Installed(dir) => Some(dir),
            AssetSource::Embedded => None,
        }
    }

    /// Whether a built dashboard is present, as opposed to the embedded "not installed" page.
    #[must_use]
    pub fn web_is_built(&self) -> bool {
        matches!(self.find(WEB_INDEX), Some(Asset::File(_)))
    }

    /// Find one asset by its path relative to the asset root.
    ///
    /// The chosen directory is searched first, then the embedded table, so an
    /// override can replace a built-in asset but does not have to supply every
    /// one. A path that is absolute or climbs out with `..` finds nothing:
    /// the lookup must not become a way to read arbitrary files.
    #[must_use]
    pub fn find(&self, relative: &str) -> Option<Asset> {
        let relative_path = Path::new(relative);
        if relative_path.as_os_str().is_empty()
            || !relative_path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return None;
        }
        if let AssetSource::Override(dir) | AssetSource::Installed(dir) = &self.source {
            let file = dir.join(relative_path);
            if file.is_file() {
                return Some(Asset::File(file));
            }
        }
        EMBEDDED
            .iter()
            .find(|(name, _)| *name == relative)
            .map(|(_, bytes)| Asset::Embedded(bytes))
    }
}

impl std::fmt::Display for AssetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Override(dir) => write!(f, "override {}", dir.display()),
            Self::Installed(dir) => write!(f, "installed {}", dir.display()),
            Self::Embedded => f.write_str("embedded"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `<prefix>/bin` executable directory with an optional installed
    /// `<prefix>/share/memcastle`.
    fn prefix(root: &Path, installed: bool) -> (PathBuf, PathBuf) {
        let bin = root.join("bin");
        let share = root.join("share").join(SHARE_DIR);
        std::fs::create_dir_all(&bin).unwrap();
        if installed {
            std::fs::create_dir_all(&share).unwrap();
        }
        (bin, share)
    }

    fn search(exe_dir: Option<PathBuf>) -> InstallSearch {
        InstallSearch {
            exe_dir,
            system_dirs: Vec::new(),
            user_data_dir: None,
        }
    }

    #[test]
    fn the_root_is_the_chosen_directory_and_absent_for_the_embedded_assets() {
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);

        let installed = Assets::resolve(None, &search(Some(bin))).unwrap();
        let embedded = Assets::resolve(None, &search(None)).unwrap();

        assert_eq!(installed.root(), Some(share.as_path()));
        assert_eq!(embedded.root(), None);
    }

    #[test]
    fn an_explicit_directory_outranks_an_installed_one() {
        let root = tempfile::tempdir().unwrap();
        let (bin, _) = prefix(root.path(), true);
        let chosen = root.path().join("dev-assets");
        std::fs::create_dir(&chosen).unwrap();

        let assets = Assets::resolve(Some(&chosen), &search(Some(bin))).unwrap();

        assert_eq!(assets.source(), &AssetSource::Override(chosen));
    }

    #[test]
    fn a_missing_explicit_directory_is_an_error_and_never_falls_through() {
        let root = tempfile::tempdir().unwrap();
        let (bin, _) = prefix(root.path(), true);

        let err = Assets::resolve(Some(&root.path().join("typo")), &search(Some(bin))).unwrap_err();

        assert!(matches!(err, Error::AssetsNotFound { .. }), "{err}");
    }

    #[test]
    fn an_explicit_path_that_is_a_file_is_not_a_directory() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        std::fs::write(&file, "x").unwrap();

        let err = Assets::resolve(Some(&file), &search(None)).unwrap_err();

        assert!(matches!(err, Error::AssetsNotFound { .. }), "{err}");
    }

    #[test]
    fn the_prefix_relative_share_directory_is_the_installed_source() {
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);

        let assets = Assets::resolve(None, &search(Some(bin))).unwrap();

        assert_eq!(assets.source(), &AssetSource::Installed(share));
    }

    #[test]
    fn the_prefix_relative_directory_outranks_a_system_one() {
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);
        let system = root.path().join("system");
        std::fs::create_dir(&system).unwrap();

        let assets = Assets::resolve(
            None,
            &InstallSearch {
                exe_dir: Some(bin),
                system_dirs: vec![system],
                user_data_dir: None,
            },
        )
        .unwrap();

        assert_eq!(assets.source(), &AssetSource::Installed(share));
    }

    #[test]
    fn a_system_directory_is_used_when_the_prefix_has_no_assets() {
        let root = tempfile::tempdir().unwrap();
        let (bin, _) = prefix(root.path(), false);
        let system = root.path().join("system");
        std::fs::create_dir(&system).unwrap();

        let assets = Assets::resolve(
            None,
            &InstallSearch {
                exe_dir: Some(bin),
                system_dirs: vec![system.clone()],
                user_data_dir: None,
            },
        )
        .unwrap();

        assert_eq!(assets.source(), &AssetSource::Installed(system));
    }

    #[test]
    fn with_nothing_installed_the_embedded_assets_are_used() {
        let root = tempfile::tempdir().unwrap();
        let (bin, _) = prefix(root.path(), false);

        let assets = Assets::resolve(None, &search(Some(bin))).unwrap();

        assert_eq!(assets.source(), &AssetSource::Embedded);
    }

    #[test]
    fn an_unknown_executable_location_falls_back_to_embedded_assets() {
        let assets = Assets::resolve(None, &search(None)).unwrap();

        assert_eq!(assets.source(), &AssetSource::Embedded);
    }

    #[test]
    fn a_share_directory_inside_the_user_data_directory_is_never_installed_assets() {
        // `~/.local/bin/memcastle` -> `~/.local/share/memcastle`, which is
        // also where the XDG data directory (and the palace) lives.
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);

        let assets = Assets::resolve(
            None,
            &InstallSearch {
                exe_dir: Some(bin),
                system_dirs: Vec::new(),
                user_data_dir: Some(share),
            },
        )
        .unwrap();

        assert_eq!(assets.source(), &AssetSource::Embedded);
    }

    #[test]
    fn a_candidate_under_the_user_data_directory_is_dropped_too() {
        let data = PathBuf::from("/home/me/.local/share/memcastle");
        let candidates = InstallSearch {
            exe_dir: None,
            system_dirs: vec![data.join("default"), PathBuf::from("/usr/share/memcastle")],
            user_data_dir: Some(data),
        }
        .candidates();

        assert_eq!(candidates, vec![PathBuf::from("/usr/share/memcastle")]);
    }

    #[test]
    fn a_file_in_the_chosen_directory_is_found() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("web")).unwrap();
        std::fs::write(root.path().join("web/index.html"), "<html>").unwrap();
        let assets = Assets::resolve(Some(root.path()), &search(None)).unwrap();

        assert_eq!(
            assets.find("web/index.html"),
            Some(Asset::File(root.path().join("web/index.html")))
        );
        assert_eq!(assets.find("web/missing.html"), None);
    }

    #[test]
    fn a_lookup_cannot_escape_the_asset_directory() {
        let root = tempfile::tempdir().unwrap();
        let inner = root.path().join("assets");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(root.path().join("secret"), "x").unwrap();
        let assets = Assets::resolve(Some(&inner), &search(None)).unwrap();
        let absolute = root.path().join("secret");

        assert_eq!(assets.find("../secret"), None);
        assert_eq!(assets.find("a/../../secret"), None);
        assert_eq!(assets.find(absolute.to_str().unwrap()), None);
        assert_eq!(assets.find(""), None);
    }

    /// Write a built dashboard (`web/dist/index.html`) under `root`.
    fn build_web(root: &Path) {
        std::fs::create_dir_all(root.join(WEB_DIST_DIR)).unwrap();
        std::fs::write(root.join(WEB_INDEX), "<html>built</html>").unwrap();
    }

    #[test]
    fn a_packaged_installation_resolves_the_dashboard_from_its_share_directory() {
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);
        build_web(&share);

        let assets = Assets::resolve(None, &search(Some(bin))).unwrap();

        assert_eq!(
            assets.find(WEB_INDEX),
            Some(Asset::File(share.join(WEB_INDEX)))
        );
        assert!(assets.web_is_built());
    }

    #[test]
    fn a_worktree_resolves_the_dashboard_through_the_same_lookup_as_a_package() {
        let checkout = tempfile::tempdir().unwrap();
        build_web(checkout.path());

        let assets = Assets::resolve(Some(checkout.path()), &search(None)).unwrap();

        assert_eq!(
            assets.find(WEB_INDEX),
            Some(Asset::File(checkout.path().join(WEB_INDEX)))
        );
        assert!(assets.web_is_built());
    }

    #[test]
    fn a_worktree_dashboard_outranks_an_installed_one() {
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);
        build_web(&share);
        let checkout = root.path().join("checkout");
        build_web(&checkout);

        let assets = Assets::resolve(Some(&checkout), &search(Some(bin))).unwrap();

        assert_eq!(
            assets.find(WEB_INDEX),
            Some(Asset::File(checkout.join(WEB_INDEX)))
        );
    }

    #[test]
    fn without_a_build_the_embedded_page_stands_in_and_the_dashboard_is_not_built() {
        let root = tempfile::tempdir().unwrap();
        let (bin, share) = prefix(root.path(), true);

        // Installed assets without a `web/` tree, and no assets at all.
        for assets in [
            Assets::resolve(None, &search(Some(bin))).unwrap(),
            Assets::resolve(None, &search(None)).unwrap(),
        ] {
            assert!(matches!(assets.find(WEB_INDEX), Some(Asset::Embedded(_))));
            assert!(!assets.web_is_built());
        }
        assert!(share.is_dir());
    }

    #[test]
    fn the_source_is_described_for_the_startup_log() {
        assert_eq!(AssetSource::Embedded.to_string(), "embedded");
        assert_eq!(
            AssetSource::Installed(PathBuf::from("/usr/share/memcastle")).to_string(),
            "installed /usr/share/memcastle"
        );
        assert_eq!(
            AssetSource::Override(PathBuf::from("/srv/web")).to_string(),
            "override /srv/web"
        );
    }
}
