//! Typed, validated configuration.
//!
//! Load order: hardcoded defaults -> optional TOML file -> `MEMCASTLE_*`
//! environment overrides -> command-line [`Overrides`] -> [`Config::validate`].
//! Deliberately hand-rolled rather than pulled in from a config-framework
//! crate — there are seven sections of settings, and a framework's abstraction
//! cost would outweigh what it saves here.
//!
//! Default file locations follow the Unix XDG convention on Linux and macOS
//! alike; see [`paths`].

pub mod paths;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::Backend;

/// Settings given on the command line, the highest-precedence layer.
///
/// Kept separate from `cli` (which only defines argument types) so that
/// every command builds the same [`Config`] from the same inputs: a
/// `--palace` that only `serve` honoured would leave `status` looking for
/// the daemon of a different palace.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// `--palace`: replaces `palace.path`.
    pub palace: Option<PathBuf>,
    /// `--bind`: replaces `server.bind`.
    pub bind: Option<IpAddr>,
    /// `--port`: replaces `server.port`.
    pub port: Option<u16>,
    /// `--assets-dir`: replaces `assets.dir`.
    pub assets_dir: Option<PathBuf>,
}

/// Where the palace's own data lives, distinct from `store` (which is where
/// its *content* lives) so a future multi-palace-per-machine setup has
/// somewhere to hang per-palace runtime metadata (the daemon registry file —
/// see `server::lifecycle`) without conflating it with SurrealDB's files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PalaceConfig {
    /// The directory this palace's data (and, for the embedded backend, its
    /// SurrealDB files) live under.
    pub path: PathBuf,
}

impl Default for PalaceConfig {
    fn default() -> Self {
        Self {
            path: default_palace_dir(),
        }
    }
}

/// Backend selection, as read from configuration (before being turned into
/// `store::Backend`) — see [`StoreConfig::into_backend`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum StoreConfig {
    /// Embedded SurrealKV under `palace.path` — the default developer setup.
    #[default]
    Embedded,
    /// A remotely hosted SurrealDB instance.
    Remote {
        /// e.g. `ws://localhost:8000`.
        url: String,
        /// Namespace to select.
        namespace: String,
        /// Database to select.
        database: String,
        /// Root username (only root sign-in is supported today).
        username: String,
        /// Root password.
        password: String,
    },
}

impl StoreConfig {
    /// Resolve into the concrete backend `store::SurrealStore::connect` needs.
    #[must_use]
    pub fn into_backend(self, palace_path: &Path) -> Backend {
        match self {
            Self::Embedded => Backend::Embedded {
                path: palace_path.join("db"),
            },
            Self::Remote {
                url,
                namespace,
                database,
                username,
                password,
            } => Backend::Remote {
                url,
                namespace,
                database,
                username,
                password,
            },
        }
    }
}

/// HTTP server (API + MCP) settings.
///
/// The interface and the port are separate settings so either can be
/// overridden alone (`--port 9000` must not require restating the address).
/// `#[serde(default)]` so a config file that sets only `port` keeps the
/// loopback default for `bind` instead of failing to parse.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Interface address the daemon's HTTP listener binds to. Loopback by
    /// default: authentication is optional and off by default, so listening
    /// on a wildcard address must be an explicit choice (and should come with
    /// `auth.enabled`).
    pub bind: IpAddr,
    /// TCP port the listener binds to. `0` asks the OS for a free port (the
    /// daemon records the real one in its registry file); tests rely on it.
    pub port: u16,
}

/// The default listener address: loopback only, never a wildcard.
const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// The default listener port.
const DEFAULT_PORT: u16 = 8420;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND,
            port: DEFAULT_PORT,
        }
    }
}

impl ServerConfig {
    /// The address the listener binds to and clients fall back to: the one
    /// place `bind` and `port` are combined, so no caller can disagree.
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }
}

/// A configuration value that must never reach a log line, a status report or a
/// serialised config.
///
/// `Debug` is redacted by hand and `Serialize` is skipped on the owning field:
/// `Config` derives both, and a derived `Debug` on a bare `String` would print
/// the token the first time someone wrote `tracing::debug!("{config:?}")`.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The plaintext, for the two places that genuinely need it: hashing it on
    /// the daemon and sending it as a bearer header from the client.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A fixed placeholder, not even the length: the length narrows a brute force.
        f.write_str("[REDACTED]")
    }
}

/// The shortest shared secret accepted. A guessable token defeats the point of
/// turning authentication on, and `memcastle auth generate` produces far longer.
pub const MIN_TOKEN_LEN: usize = 16;

/// Daemon authentication settings (see `docs/adr/014`).
///
/// Off by default so the local standalone workflow stays simple.
/// `#[serde(default)]` so a config file that sets only `enabled` still parses.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Whether the daemon requires a bearer token. Decided once per daemon
    /// start, from configuration only: a stored verifier alone never turns it
    /// on, or generating a token would silently change the next restart.
    pub enabled: bool,
    /// A shared secret, from the config file or `MEMCASTLE_AUTH_TOKEN`. The
    /// daemon accepts it in addition to a token made by `auth generate`, and
    /// the CLI presents it to a daemon that requires one. Prefer the
    /// environment variable so the secret stays out of the file.
    #[serde(skip_serializing)]
    pub token: Option<Secret>,
}

/// Parse a bind *interface* (`--bind`, `MEMCASTLE_BIND`).
///
/// # Errors
///
/// Returns a message when `raw` is not an IP address. A `host:port` value —
/// what `--bind` accepted before the port became its own setting — gets a
/// pointer to `--port`, because "invalid IP address syntax" alone would not
/// tell someone with an old service file what changed.
pub fn parse_bind_host(raw: &str) -> std::result::Result<IpAddr, String> {
    let raw = raw.trim();
    raw.parse().map_err(|e| {
        if raw.parse::<SocketAddr>().is_ok() {
            format!(
                "{raw:?} includes a port, but the bind address is now an IP address alone; \
                 set the port with `--port`, MEMCASTLE_PORT or `server.port`"
            )
        } else {
            format!("{raw:?} is not an IP address ({e})")
        }
    })
}

/// Runtime asset settings (see [`crate::assets`]).
///
/// Not a palace or user-data setting: the assets are package content, and the
/// directory named here is never written to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetsConfig {
    /// An explicit assets directory, outranking the installed package assets
    /// and the embedded ones. Unset by default: a standalone binary needs no
    /// assets directory, and a package finds its own.
    pub dir: Option<PathBuf>,
}

/// Job scheduler settings.
///
/// `#[serde(default)]` so a config file that sets only some of these (say just
/// `max_concurrency`) keeps working when a new setting is added, instead of
/// failing to parse for a key it has never heard of.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct JobsConfig {
    /// Maximum number of jobs executing concurrently.
    pub max_concurrency: usize,
    /// How long shutdown waits, in seconds, for running jobs to reach their
    /// next unit-of-work boundary, checkpoint and hand themselves back to the
    /// queue. A job still running after this is left `Running` and re-queued
    /// by crash recovery on the next start, losing only the work since its
    /// last checkpoint. Raise it if your jobs have long units of work and
    /// you would rather wait than redo them; lower it if a supervisor kills
    /// the daemon sooner than this anyway.
    pub drain_timeout_secs: u64,
    /// How long, in seconds, a running job's lease lasts without a heartbeat.
    /// The daemon renews it every third of this. It matters when several
    /// daemons share one remote palace: a daemon that stalls or is cut off
    /// for longer than this loses its jobs to another. Raise it if your
    /// network or host pauses longer than that without being dead; lower it
    /// to fail over faster. Irrelevant, beyond the heartbeat cost, to an
    /// embedded palace, which only one daemon can open.
    pub lease_ttl_secs: u64,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            // A conservative, always-safe default rather than
            // `num_cpus::get()` — mining is I/O- as much as CPU-bound in
            // this bootstrap, and an extra dependency isn't worth it yet.
            max_concurrency: 4,
            // Long enough for every handler's unit of work (a file, a
            // checkpoint item, an audit chunk) to finish, short enough that
            // a stuck job cannot hold up a service manager's stop timeout.
            drain_timeout_secs: 10,
            // Long enough to ride out a garbage-collection pause or a
            // dropped packet or two, short enough that a dead daemon's jobs
            // move within the minute.
            lease_ttl_secs: 30,
        }
    }
}

/// Logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// A `tracing_subscriber::EnvFilter` directive, e.g. `"info"` or
    /// `"memcastle=debug,tower_http=info"`.
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

/// The fully resolved, validated configuration for one run of the binary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Where this palace's data lives.
    #[serde(default)]
    pub palace: PalaceConfig,
    /// Which storage backend to use.
    #[serde(default)]
    pub store: StoreConfig,
    /// HTTP server settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Job scheduler settings.
    #[serde(default)]
    pub jobs: JobsConfig,
    /// Logging settings.
    #[serde(default)]
    pub logging: LoggingConfig,
    /// Runtime asset settings.
    #[serde(default)]
    pub assets: AssetsConfig,
    /// Authentication settings.
    #[serde(default)]
    pub auth: AuthConfig,
}

impl Config {
    /// Load configuration: defaults, then `path` (or the default config file,
    /// `$XDG_CONFIG_HOME/memcastle/config.toml`, if `path` is `None` and it
    /// exists), then `MEMCASTLE_*` environment overrides, then the
    /// command-line `overrides`, then validation.
    ///
    /// An explicit `path` must exist (naming a file that is not there is a
    /// typo, and silently using defaults would hide it); the default file is
    /// optional.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] if a config file exists but fails to parse,
    /// or if the resolved configuration fails [`Config::validate`].
    pub fn load(path: Option<&Path>, overrides: &Overrides) -> Result<Self> {
        let mut config = match path {
            Some(path) => Self::from_file(path)?,
            None => match paths::default_config_file() {
                Some(default_path) if default_path.is_file() => Self::from_file(&default_path)?,
                _ => Self::default(),
            },
        };
        config.apply_env_overrides()?;
        config.apply_cli_overrides(overrides);
        config.validate()?;
        Ok(config)
    }

    /// Apply the command-line layer, which outranks the environment so that a
    /// one-off flag beats a value exported in the shell profile.
    fn apply_cli_overrides(&mut self, overrides: &Overrides) {
        if let Some(palace) = &overrides.palace {
            self.palace.path.clone_from(palace);
        }
        if let Some(bind) = overrides.bind {
            self.server.bind = bind;
        }
        if let Some(port) = overrides.port {
            self.server.port = port;
        }
        if let Some(dir) = &overrides.assets_dir {
            self.assets.dir = Some(dir.clone());
        }
    }

    fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|source| Error::io(path.display().to_string(), source))?;
        Ok(toml::from_str(&text)?)
    }

    /// Apply `MEMCASTLE_*` overrides, for container/server deployments where
    /// a config file is inconvenient.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] naming the variable when a numeric or
    /// address override does not parse. Ignoring it instead would silently
    /// keep the file/default value — a typo'd `MEMCASTLE_BIND` would start
    /// the daemon on the wrong port with nothing to explain why — and
    /// `validate` cannot catch that, since it only ever sees the value that
    /// survived.
    fn apply_env_overrides(&mut self) -> Result<()> {
        self.apply_overrides_from(|name| std::env::var(name).ok())
    }

    /// The testable part of [`Config::apply_env_overrides`]: `lookup` stands
    /// in for the process environment so tests need not mutate it (which is
    /// process-wide and races with other tests).
    fn apply_overrides_from(&mut self, lookup: impl Fn(&str) -> Option<String>) -> Result<()> {
        if let Some(path) = lookup("MEMCASTLE_PALACE_PATH") {
            self.palace.path = PathBuf::from(path);
        }
        if let Some(bind) = lookup("MEMCASTLE_BIND") {
            self.server.bind = parse_bind_host(&bind)
                .map_err(|e| Error::config(format!("MEMCASTLE_BIND: {e}")))?;
        }
        if let Some(port) = lookup("MEMCASTLE_PORT") {
            self.server.port = parse_override("MEMCASTLE_PORT", &port)?;
        }
        if let Some(dir) = lookup("MEMCASTLE_ASSETS_DIR") {
            self.assets.dir = Some(PathBuf::from(dir));
        }
        if let Some(level) = lookup("MEMCASTLE_LOG") {
            self.logging.level = level;
        }
        if let Some(n) = lookup("MEMCASTLE_JOBS_MAX_CONCURRENCY") {
            self.jobs.max_concurrency = parse_override("MEMCASTLE_JOBS_MAX_CONCURRENCY", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS") {
            self.jobs.drain_timeout_secs = parse_override("MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_JOBS_LEASE_TTL_SECS") {
            self.jobs.lease_ttl_secs = parse_override("MEMCASTLE_JOBS_LEASE_TTL_SECS", &n)?;
        }
        if let Some(raw) = lookup("MEMCASTLE_AUTH_ENABLED") {
            self.auth.enabled = parse_override("MEMCASTLE_AUTH_ENABLED", &raw)?;
        }
        // Deliberately not `parse_override`: its error embeds the raw value,
        // and this one is a secret. An empty variable (a secret manager that
        // resolved to nothing) is kept so `validate` rejects it by name
        // rather than silently leaving authentication unconfigured.
        if let Some(token) = lookup("MEMCASTLE_AUTH_TOKEN") {
            self.auth.token = Some(Secret::new(token.trim()));
        }
        Ok(())
    }

    /// The `tracing` filter directive this run should log with.
    ///
    /// Precedence, highest first: `MEMCASTLE_LOG` (already folded into
    /// `logging.level` by [`Config::load`]'s env overrides), then the
    /// conventional `RUST_LOG`, then the command line's `-v`/`-vv`
    /// (`verbosity`), then `logging.level` from the config file or its
    /// default. `RUST_LOG` outranks the file so a one-off
    /// `RUST_LOG=debug memcastle serve` still works against a config that
    /// pins a quieter level, and both environment variables outrank `-v`
    /// because they are the more specific instruction (they can name a
    /// target, which `-v` cannot).
    #[must_use]
    pub fn log_filter(&self, verbosity: u8) -> String {
        Self::resolve_log_filter(
            &self.logging.level,
            std::env::var_os("MEMCASTLE_LOG").is_some(),
            std::env::var("RUST_LOG").ok(),
            verbosity,
        )
    }

    /// The pure part of [`Config::log_filter`], split out so precedence is
    /// testable without mutating process-wide environment variables.
    fn resolve_log_filter(
        configured: &str,
        memcastle_log_is_set: bool,
        rust_log: Option<String>,
        verbosity: u8,
    ) -> String {
        match rust_log {
            Some(rust_log) if !memcastle_log_is_set => return rust_log,
            _ if memcastle_log_is_set => return configured.to_string(),
            _ => {}
        }
        // `-v` raises memcastle's own level and leaves everything else at
        // the configured one: `debug` for every crate would drown the
        // output in SurrealDB and HTTP-stack chatter nobody asked for.
        match verbosity {
            0 => configured.to_string(),
            1 => format!("{configured},memcastle=debug"),
            _ => format!("{configured},memcastle=trace"),
        }
    }

    /// Check invariants a malformed config or override could violate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] describing the first invariant violated.
    pub fn validate(&self) -> Result<()> {
        // A relative palace path means different directories for a daemon and
        // the client started from another working directory, so the client
        // would never find the daemon's registry. It also happens when no
        // home directory exists to derive the XDG default from.
        if !self.palace.path.is_absolute() {
            return Err(Error::config(format!(
                "palace.path {:?} is not an absolute path; set it to one with `--palace`, \
                 MEMCASTLE_PALACE_PATH or `palace.path` in the config file, \
                 or set HOME or XDG_DATA_HOME so the default can be derived",
                self.palace.path.display().to_string()
            )));
        }
        // Same reasoning as the palace path: a relative assets directory
        // resolves against whatever directory the daemon was started from, so
        // a service manager and a shell would serve different files. Whether
        // it exists is checked at startup (`assets::Assets::resolve`), not
        // here, which stays free of I/O.
        if let Some(dir) = self.assets.dir.as_ref().filter(|dir| !dir.is_absolute()) {
            return Err(Error::config(format!(
                "assets.dir {:?} is not an absolute path; set it to one with `--assets-dir`, \
                 MEMCASTLE_ASSETS_DIR or `assets.dir` in the config file, or remove it \
                 to use the installed or embedded assets",
                dir.display().to_string()
            )));
        }
        if self.jobs.max_concurrency == 0 {
            return Err(Error::config("jobs.max_concurrency must be at least 1"));
        }
        // Zero would skip the drain entirely, abandoning every in-flight job
        // to crash recovery on each clean shutdown; a day is far past any
        // supervisor's patience and is almost certainly a units mistake.
        if !(1..=86_400).contains(&self.jobs.drain_timeout_secs) {
            return Err(Error::config(
                "jobs.drain_timeout_secs must be between 1 and 86400 seconds",
            ));
        }
        // The heartbeat renews every third of the lease; under three seconds
        // that is sub-second, which is noise on a network store, and a day is
        // a units mistake that would leave a dead daemon's jobs stuck.
        if !(3..=86_400).contains(&self.jobs.lease_ttl_secs) {
            return Err(Error::config(
                "jobs.lease_ttl_secs must be between 3 and 86400 seconds",
            ));
        }
        // The message never includes the token itself, only what to do about it.
        if let Some(token) = &self.auth.token
            && token.expose().len() < MIN_TOKEN_LEN
        {
            return Err(Error::config(format!(
                "auth.token (or MEMCASTLE_AUTH_TOKEN) must be at least {MIN_TOKEN_LEN} characters; \
                 generate a strong one with `memcastle auth generate`"
            )));
        }
        Ok(())
    }
}

/// Parse one environment override, naming the variable and the offending value
/// on failure so the user knows which of several `MEMCASTLE_*` variables to fix.
fn parse_override<T>(name: &str, raw: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.trim()
        .parse()
        .map_err(|e| Error::config(format!("{name}={raw:?} is not valid: {e}")))
}

/// `$XDG_DATA_HOME/memcastle/default` (`~/.local/share/memcastle/default`), the
/// default palace directory when nothing more specific is configured.
fn default_palace_dir() -> PathBuf {
    paths::default_palace_dir()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_configured_log_level_is_used_when_no_environment_variable_overrides_it() {
        assert_eq!(Config::resolve_log_filter("debug", false, None, 0), "debug");
    }

    #[test]
    fn rust_log_outranks_the_config_file_level() {
        assert_eq!(
            Config::resolve_log_filter("warn", false, Some("trace".to_string()), 0),
            "trace"
        );
    }

    #[test]
    fn memcastle_log_outranks_rust_log() {
        // `MEMCASTLE_LOG` is already folded into the configured level.
        assert_eq!(
            Config::resolve_log_filter("memcastle=debug", true, Some("trace".to_string()), 2),
            "memcastle=debug"
        );
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn a_well_formed_environment_override_replaces_the_default() {
        let mut config = Config::default();
        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_BIND", "127.0.0.2"),
                ("MEMCASTLE_PORT", "9999"),
                ("MEMCASTLE_JOBS_MAX_CONCURRENCY", "7"),
            ]))
            .unwrap();
        assert_eq!(config.server.bind, "127.0.0.2".parse::<IpAddr>().unwrap());
        assert_eq!(config.server.port, 9999);
        assert_eq!(config.jobs.max_concurrency, 7);
    }

    const TOKEN: &str = "mc_0123456789abcdef0123456789abcdef";

    #[test]
    fn authentication_is_disabled_and_tokenless_by_default() {
        let config = Config::default();
        assert!(!config.auth.enabled);
        assert!(config.auth.token.is_none());
    }

    #[test]
    fn the_auth_settings_come_from_the_file_then_the_environment() {
        let mut config: Config =
            toml::from_str(&format!("[auth]\nenabled = false\ntoken = \"{TOKEN}\"")).unwrap();
        assert_eq!(config.auth.token.as_ref().map(Secret::expose), Some(TOKEN));

        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_AUTH_ENABLED", "true"),
                ("MEMCASTLE_AUTH_TOKEN", "mc_from_the_environment_0000"),
            ]))
            .unwrap();
        assert!(config.auth.enabled);
        assert_eq!(
            config.auth.token.as_ref().map(Secret::expose),
            Some("mc_from_the_environment_0000")
        );
    }

    #[test]
    fn a_malformed_auth_enabled_variable_is_named_in_the_error() {
        let mut config = Config::default();
        let err = config
            .apply_overrides_from(env(&[("MEMCASTLE_AUTH_ENABLED", "maybe")]))
            .unwrap_err();
        assert!(err.to_string().contains("MEMCASTLE_AUTH_ENABLED"), "{err}");
    }

    #[test]
    fn the_secret_never_appears_in_debug_output_or_a_serialised_config() {
        let mut config = Config::default();
        config.auth.token = Some(Secret::new(TOKEN));

        assert!(!format!("{config:?}").contains(TOKEN));
        assert!(format!("{config:?}").contains("[REDACTED]"));
        assert!(!toml::to_string(&config).unwrap().contains(TOKEN));
        assert!(!serde_json::to_string(&config).unwrap().contains(TOKEN));
    }

    #[test]
    fn a_short_or_empty_token_is_rejected_without_echoing_it() {
        for short in ["", "hunter2"] {
            let mut config = Config::default();
            config.palace.path = std::env::temp_dir();
            config.auth.token = Some(Secret::new(short));

            let message = config.validate().unwrap_err().to_string();

            assert!(message.contains("MEMCASTLE_AUTH_TOKEN"), "{message}");
            assert!(
                short.is_empty() || !message.contains(short),
                "the rejected token leaked into the error: {message}"
            );
        }
    }

    #[test]
    fn no_assets_directory_is_configured_by_default() {
        assert_eq!(Config::default().assets.dir, None);
    }

    #[test]
    fn the_assets_directory_comes_from_the_file_then_the_environment_then_the_flag() {
        let mut config: Config = toml::from_str("[assets]\ndir = \"/from/file\"").unwrap();
        assert_eq!(config.assets.dir, Some(PathBuf::from("/from/file")));

        config
            .apply_overrides_from(env(&[("MEMCASTLE_ASSETS_DIR", "/from/env")]))
            .unwrap();
        assert_eq!(config.assets.dir, Some(PathBuf::from("/from/env")));

        config.apply_cli_overrides(&Overrides {
            assets_dir: Some(PathBuf::from("/from/flag")),
            ..Overrides::default()
        });
        assert_eq!(config.assets.dir, Some(PathBuf::from("/from/flag")));
    }

    #[test]
    fn a_relative_assets_directory_is_rejected_with_the_way_to_fix_it() {
        let mut config = Config::default();
        config.palace.path = std::env::temp_dir();
        config.assets.dir = Some(PathBuf::from("web/dist"));

        let err = config.validate().unwrap_err();

        assert!(matches!(err, Error::Config { .. }), "{err}");
        let message = err.to_string();
        assert!(
            message.contains("--assets-dir") && message.contains("MEMCASTLE_ASSETS_DIR"),
            "{message}"
        );
    }

    #[test]
    fn the_default_listener_is_loopback_only_on_port_8420() {
        let server = ServerConfig::default();
        assert!(server.bind.is_loopback(), "the default must be local-only");
        assert_eq!(server.socket_addr().to_string(), "127.0.0.1:8420");
    }

    #[test]
    fn a_config_file_that_sets_only_the_port_keeps_the_loopback_bind() {
        let config: Config = toml::from_str("[server]\nport = 9000").unwrap();
        assert_eq!(config.server.port, 9000);
        assert!(config.server.bind.is_loopback());
    }

    #[test]
    fn a_config_file_that_sets_only_the_bind_keeps_the_default_port() {
        let config: Config = toml::from_str("[server]\nbind = \"::1\"").unwrap();
        assert_eq!(config.server.bind, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(config.server.port, 8420);
        assert_eq!(config.server.socket_addr().to_string(), "[::1]:8420");
    }

    #[test]
    fn a_bind_that_still_carries_a_port_is_rejected_with_a_pointer_to_the_port_setting() {
        let mut config = Config::default();
        let err = config
            .apply_overrides_from(env(&[("MEMCASTLE_BIND", "127.0.0.1:8420")]))
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("MEMCASTLE_BIND") && message.contains("MEMCASTLE_PORT"),
            "{message}"
        );
    }

    #[test]
    fn a_malformed_port_override_is_an_error_naming_the_variable() {
        for raw in ["abc", "70000", "-1", ""] {
            let mut config = Config::default();
            let err = config
                .apply_overrides_from(env(&[("MEMCASTLE_PORT", raw)]))
                .unwrap_err();
            assert!(
                err.to_string().contains("MEMCASTLE_PORT"),
                "{raw:?} must name the variable: {err}"
            );
        }
    }

    #[test]
    fn a_malformed_bind_override_is_an_error_naming_the_variable() {
        let mut config = Config::default();
        let err = config
            .apply_overrides_from(env(&[("MEMCASTLE_BIND", "garbage")]))
            .unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        assert!(
            err.to_string().contains("MEMCASTLE_BIND"),
            "the message must name the variable: {err}"
        );
    }

    #[test]
    fn a_malformed_numeric_override_is_an_error_not_a_silent_default() {
        for name in [
            "MEMCASTLE_JOBS_MAX_CONCURRENCY",
            "MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS",
            "MEMCASTLE_JOBS_LEASE_TTL_SECS",
        ] {
            let mut config = Config::default();
            let err = config
                .apply_overrides_from(env(&[(name, "abc")]))
                .unwrap_err();
            assert!(err.to_string().contains(name), "{name}: {err}");
        }
    }

    #[test]
    fn a_logging_level_in_the_config_file_is_read() {
        let config: Config = toml::from_str("[logging]\nlevel = \"debug\"").unwrap();
        assert_eq!(config.logging.level, "debug");
    }

    use super::*;

    #[test]
    fn defaults_validate_successfully() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn a_relative_palace_path_is_rejected_with_the_way_to_fix_it() {
        let mut config = Config::default();
        config.palace.path = PathBuf::from("relative/palace");
        let err = config.validate().unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        let message = err.to_string();
        assert!(
            message.contains("--palace") && message.contains("MEMCASTLE_PALACE_PATH"),
            "{message}"
        );
    }

    #[test]
    fn the_command_line_outranks_the_environment_which_outranks_the_file() {
        let root = tempfile::tempdir().unwrap();
        let (file, env_path, flag) = (
            root.path().join("file"),
            root.path().join("env"),
            root.path().join("flag"),
        );
        let mut config: Config = toml::from_str(&format!(
            "[palace]\npath = {:?}\n[server]\nbind = \"127.0.0.1\"\nport = 1111",
            file.display().to_string()
        ))
        .unwrap();
        assert_eq!(config.palace.path, file, "the file outranks the default");
        assert_eq!(config.server.port, 1111);

        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_PALACE_PATH", &env_path.display().to_string()),
                ("MEMCASTLE_BIND", "127.0.0.2"),
                ("MEMCASTLE_PORT", "2222"),
            ]))
            .unwrap();
        assert_eq!(config.palace.path, env_path);
        assert_eq!(config.server.socket_addr().to_string(), "127.0.0.2:2222");

        config.apply_cli_overrides(&Overrides {
            palace: Some(flag.clone()),
            bind: Some("127.0.0.3".parse().unwrap()),
            port: Some(3333),
            ..Overrides::default()
        });
        assert_eq!(config.palace.path, flag);
        assert_eq!(config.server.socket_addr().to_string(), "127.0.0.3:3333");
    }

    #[test]
    fn the_bind_and_the_port_are_overridden_independently_at_every_layer() {
        // File sets both, env replaces only the port, the flag replaces only
        // the bind: each layer must leave the other setting alone.
        let mut config: Config =
            toml::from_str("[server]\nbind = \"127.0.0.2\"\nport = 1111").unwrap();
        config
            .apply_overrides_from(env(&[("MEMCASTLE_PORT", "2222")]))
            .unwrap();
        assert_eq!(config.server.socket_addr().to_string(), "127.0.0.2:2222");
        config.apply_cli_overrides(&Overrides {
            bind: Some("127.0.0.3".parse().unwrap()),
            ..Overrides::default()
        });
        assert_eq!(config.server.socket_addr().to_string(), "127.0.0.3:2222");
    }

    #[test]
    fn absent_command_line_overrides_leave_the_loaded_values_alone() {
        let mut config = Config::default();
        let before = config.clone();
        config.apply_cli_overrides(&Overrides::default());
        assert_eq!(config.palace.path, before.palace.path);
        assert_eq!(config.server.bind, before.server.bind);
        assert_eq!(config.server.port, before.server.port);
    }

    #[test]
    fn an_explicit_config_file_that_does_not_exist_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let err =
            Config::load(Some(&root.path().join("nope.toml")), &Overrides::default()).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    #[test]
    fn zero_concurrency_is_rejected() {
        let mut config = Config::default();
        config.jobs.max_concurrency = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn embedded_store_config_resolves_under_the_palace_path() {
        let backend = StoreConfig::Embedded.into_backend(Path::new("/tmp/palace"));
        match backend {
            Backend::Embedded { path } => assert_eq!(path, Path::new("/tmp/palace/db")),
            Backend::Remote { .. } => panic!("expected an embedded backend"),
        }
    }

    #[test]
    fn the_drain_timeout_defaults_to_ten_seconds() {
        assert_eq!(Config::default().jobs.drain_timeout_secs, 10);
    }

    #[test]
    fn a_zero_or_absurd_drain_timeout_is_rejected() {
        for secs in [0, 86_401] {
            let mut config = Config::default();
            config.jobs.drain_timeout_secs = secs;
            assert!(config.validate().is_err(), "{secs}s must be rejected");
        }
        let mut config = Config::default();
        config.jobs.drain_timeout_secs = 30;
        config.validate().expect("30s is fine");
    }

    #[test]
    fn a_config_file_that_sets_only_max_concurrency_keeps_the_default_drain_timeout() {
        let config: Config = toml::from_str("[jobs]\nmax_concurrency = 2").unwrap();
        assert_eq!(config.jobs.max_concurrency, 2);
        assert_eq!(config.jobs.drain_timeout_secs, 10);
    }

    #[test]
    fn the_drain_timeout_is_read_from_the_config_file() {
        let config: Config = toml::from_str("[jobs]\ndrain_timeout_secs = 45").unwrap();
        assert_eq!(config.jobs.drain_timeout_secs, 45);
    }

    #[test]
    fn the_lease_ttl_defaults_to_thirty_seconds_and_rejects_nonsense() {
        assert_eq!(Config::default().jobs.lease_ttl_secs, 30);
        for secs in [0, 2, 86_401] {
            let mut config = Config::default();
            config.jobs.lease_ttl_secs = secs;
            assert!(config.validate().is_err(), "{secs}s must be rejected");
        }
    }

    #[test]
    fn a_config_file_that_sets_only_the_drain_timeout_keeps_the_default_lease_ttl() {
        let config: Config = toml::from_str("[jobs]\ndrain_timeout_secs = 5").unwrap();
        assert_eq!(config.jobs.lease_ttl_secs, 30);
    }

    #[test]
    fn one_v_raises_memcastles_own_level_to_debug_and_two_to_trace() {
        assert_eq!(
            Config::resolve_log_filter("info", false, None, 1),
            "info,memcastle=debug"
        );
        assert_eq!(
            Config::resolve_log_filter("warn", false, None, 2),
            "warn,memcastle=trace"
        );
    }

    #[test]
    fn no_v_leaves_the_configured_level_alone() {
        assert_eq!(Config::resolve_log_filter("warn", false, None, 0), "warn");
    }

    #[test]
    fn the_environment_outranks_v_because_it_can_name_a_target() {
        assert_eq!(
            Config::resolve_log_filter("info", false, Some("surrealdb=debug".to_string()), 2),
            "surrealdb=debug"
        );
        assert_eq!(
            Config::resolve_log_filter("memcastle=warn", true, None, 2),
            "memcastle=warn"
        );
    }
}
