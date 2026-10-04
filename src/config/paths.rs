//! Where MemCastle keeps its files: the Unix XDG Base Directory layout.
//!
//! Linux and macOS resolve to the *same* directories (`~/.config/memcastle`,
//! `~/.local/share/memcastle`, `~/.local/state/memcastle`) unless the matching
//! `XDG_*_HOME` variable says otherwise. This is deliberately not
//! `dirs::config_dir()`/`dirs::data_dir()`, which answer
//! `~/Library/Application Support` on macOS: a config file that lives in a
//! different place per OS is a support burden for a developer tool whose users
//! dotfile-manage the same tree on both. Diverging per platform must be an
//! explicit design decision (see `docs/adr/010-unix-xdg-paths.md`).
//!
//! The resolvers take the environment lookup and home directory as inputs so
//! tests can cover every platform's semantics without mutating the
//! process-wide environment (which races between tests).

use std::path::{Path, PathBuf};

/// The directory name appended to every XDG base directory.
const APP_DIR: &str = "memcastle";

/// One of the XDG base directories MemCastle uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdgDir {
    /// Configuration: `$XDG_CONFIG_HOME`, default `~/.config`.
    Config,
    /// Persistent data (palaces): `$XDG_DATA_HOME`, default `~/.local/share`.
    Data,
    /// State that outlives a process but is not user data (the daemon
    /// registry): `$XDG_STATE_HOME`, default `~/.local/state`.
    State,
}

impl XdgDir {
    /// The environment variable that relocates this directory.
    #[must_use]
    pub const fn variable(self) -> &'static str {
        match self {
            Self::Config => "XDG_CONFIG_HOME",
            Self::Data => "XDG_DATA_HOME",
            Self::State => "XDG_STATE_HOME",
        }
    }

    /// The path under `$HOME` used when the variable is not usable.
    const fn home_relative_default(self) -> &'static str {
        match self {
            Self::Config => ".config",
            Self::Data => ".local/share",
            Self::State => ".local/state",
        }
    }
}

/// `<base>/memcastle` for `dir`, where `<base>` is the XDG variable or, when
/// that is unset, empty or relative, the `home`-relative default.
///
/// The XDG specification says a relative value "should be considered invalid
/// and ignored": honouring it would scatter files relative to whatever
/// directory the command happened to run in, and a daemon and the client that
/// looks for it would disagree about where the palace is.
///
/// Returns `None` only when neither the variable nor a home directory is
/// available, so the caller decides how to fail instead of guessing a path.
#[must_use]
pub fn resolve(
    dir: XdgDir,
    lookup: impl Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let base = lookup(dir.variable())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| home.join(dir.home_relative_default())))?;
    Some(base.join(APP_DIR))
}

/// [`resolve`] against the real process environment and home directory.
fn resolve_from_process(dir: XdgDir) -> Option<PathBuf> {
    resolve(
        dir,
        |name| std::env::var(name).ok(),
        dirs::home_dir().as_deref(),
    )
}

/// The default config file: `$XDG_CONFIG_HOME/memcastle/config.toml`.
///
/// `None` when no home directory can be determined; there is then simply no
/// default config file to read.
#[must_use]
pub fn default_config_file() -> Option<PathBuf> {
    resolve_from_process(XdgDir::Config).map(|dir| dir.join("config.toml"))
}

/// The default palace directory: `$XDG_DATA_HOME/memcastle/default`.
///
/// When nothing resolves, the result is a *relative* path on purpose:
/// `Config::validate` rejects it with an error that says how to fix it,
/// which beats silently creating a palace in the current directory.
#[must_use]
pub fn default_palace_dir() -> PathBuf {
    resolve_from_process(XdgDir::Data)
        .unwrap_or_else(|| PathBuf::from(APP_DIR))
        .join("default")
}

/// The default directory for installed mining sources: `$XDG_DATA_HOME/memcastle/sources`.
///
/// Relative when nothing resolves, for the same reason as [`default_palace_dir`]: validation then asks for an
/// explicit `mining.sources_dir` rather than installing into the working directory.
#[must_use]
pub fn default_sources_dir() -> PathBuf {
    resolve_from_process(XdgDir::Data)
        .unwrap_or_else(|| PathBuf::from(APP_DIR))
        .join("sources")
}

/// The directory holding per-daemon runtime files: `$XDG_STATE_HOME/memcastle/run`.
///
/// Falls back to the system temp directory when no home exists (a minimal
/// container, say), since the registry is only a discovery hint, and a
/// daemon that cannot register itself would be undiscoverable.
#[must_use]
pub fn run_dir() -> PathBuf {
    resolve_from_process(XdgDir::State)
        .unwrap_or_else(|| std::env::temp_dir().join(APP_DIR))
        .join("run")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::*;

        #[test]
        fn linux_defaults_live_under_the_home_directory_dot_dirs() {
            let home = Path::new("/home/alice");
            let none = env(&[]);
            assert_eq!(
                resolve(XdgDir::Config, &none, Some(home)),
                Some(PathBuf::from("/home/alice/.config/memcastle"))
            );
            assert_eq!(
                resolve(XdgDir::Data, &none, Some(home)),
                Some(PathBuf::from("/home/alice/.local/share/memcastle"))
            );
            assert_eq!(
                resolve(XdgDir::State, &none, Some(home)),
                Some(PathBuf::from("/home/alice/.local/state/memcastle"))
            );
        }

        #[test]
        fn macos_uses_the_same_xdg_paths_and_never_application_support() {
            // macOS is the platform `dirs` would answer differently for; the
            // resolver is OS-independent, so this pins the contract everywhere.
            let home = Path::new("/Users/alice");
            for dir in [XdgDir::Config, XdgDir::Data, XdgDir::State] {
                let path = resolve(dir, env(&[]), Some(home)).unwrap();
                let text = path.display().to_string();
                assert!(
                    !text.contains("Library") && !text.contains("Application Support"),
                    "{text}"
                );
            }
            assert_eq!(
                resolve(XdgDir::Config, env(&[]), Some(home)),
                Some(PathBuf::from("/Users/alice/.config/memcastle"))
            );
            assert_eq!(
                resolve(XdgDir::Data, env(&[]), Some(home)),
                Some(PathBuf::from("/Users/alice/.local/share/memcastle"))
            );
        }

        #[test]
        fn custom_xdg_variables_replace_the_home_defaults() {
            let home = Path::new("/home/alice");
            let vars = env(&[
                ("XDG_CONFIG_HOME", "/etc/xdg-conf"),
                ("XDG_DATA_HOME", "/srv/data"),
                ("XDG_STATE_HOME", "/var/state"),
            ]);
            assert_eq!(
                resolve(XdgDir::Config, &vars, Some(home)),
                Some(PathBuf::from("/etc/xdg-conf/memcastle"))
            );
            assert_eq!(
                resolve(XdgDir::Data, &vars, Some(home)),
                Some(PathBuf::from("/srv/data/memcastle"))
            );
            assert_eq!(
                resolve(XdgDir::State, &vars, Some(home)),
                Some(PathBuf::from("/var/state/memcastle"))
            );
        }

        #[test]
        fn an_empty_or_relative_xdg_variable_is_ignored_per_the_spec() {
            let home = Path::new("/home/alice");
            for bad in ["", "relative/dir", "./here"] {
                assert_eq!(
                    resolve(XdgDir::Data, env(&[("XDG_DATA_HOME", bad)]), Some(home)),
                    Some(PathBuf::from("/home/alice/.local/share/memcastle")),
                    "{bad:?} must fall back to the home default"
                );
            }
        }

        #[test]
        fn a_valid_xdg_variable_works_without_any_home_directory() {
            assert_eq!(
                resolve(XdgDir::Data, env(&[("XDG_DATA_HOME", "/srv/data")]), None),
                Some(PathBuf::from("/srv/data/memcastle"))
            );
        }
    }

    #[test]
    fn nothing_resolves_without_a_home_directory_or_a_usable_variable() {
        assert_eq!(resolve(XdgDir::Data, env(&[]), None), None);
        assert_eq!(
            resolve(XdgDir::Data, env(&[("XDG_DATA_HOME", "relative")]), None),
            None
        );
    }

    #[test]
    fn an_absolute_xdg_variable_is_honoured_on_every_platform() {
        // `tempdir` is absolute on Unix and Windows alike, so this covers the
        // Windows CI job, where the literal `/srv/data` paths above are not
        // absolute.
        let root = tempfile::tempdir().unwrap();
        let base = root.path().display().to_string();
        assert_eq!(
            resolve(XdgDir::Data, env(&[("XDG_DATA_HOME", &base)]), None),
            Some(root.path().join("memcastle"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_the_real_defaults_are_not_under_library() {
        // Runs on CI's macOS job against the real `dirs::home_dir()`; only
        // meaningful when the developer has not exported an XDG override.
        if std::env::var_os("XDG_CONFIG_HOME").is_some()
            || std::env::var_os("XDG_DATA_HOME").is_some()
        {
            return;
        }
        let config = default_config_file().unwrap();
        let data = default_palace_dir();
        assert!(
            config.ends_with(".config/memcastle/config.toml"),
            "{config:?}"
        );
        assert!(data.ends_with(".local/share/memcastle/default"), "{data:?}");
    }
}
