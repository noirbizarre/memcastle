//! Typed, validated configuration.
//!
//! Load order: hardcoded defaults -> optional TOML file -> `MEMCASTLE_*`
//! environment overrides -> command-line [`Overrides`] -> [`Config::validate`].
//! Deliberately hand-rolled rather than pulled in from a config-framework
//! crate — there are ten sections of settings, and a framework's abstraction
//! cost would outweigh what it saves here.
//!
//! Default file locations follow the Unix XDG convention on Linux and macOS
//! alike; see [`paths`].

pub mod paths;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::{Backend, StoreSync};

// Defined in `domain` so `store::Backend` can hold it too; re-exported because
// configuration is where callers have always found it.
pub use crate::domain::Secret;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum StoreConfig {
    /// Embedded SurrealKV under `palace.path` — the default developer setup.
    Embedded {
        /// How often writes are forced to disk. Defaults to every commit;
        /// `[store] mode = "embedded"` alone must keep parsing, so it is optional.
        #[serde(default)]
        sync: StoreSync,
    },
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
        /// Root password. A [`Secret`] so a derived `Debug` prints a placeholder,
        /// and never serialised, for the same reason as `auth.token`: the root
        /// password is the more powerful of the two credentials.
        #[serde(skip_serializing)]
        password: Secret,
    },
}

impl Default for StoreConfig {
    fn default() -> Self {
        // Spelled out because `#[default]` only works on a unit variant.
        Self::Embedded {
            sync: StoreSync::default(),
        }
    }
}

impl StoreConfig {
    /// Resolve into the concrete backend `store::SurrealStore::connect` needs.
    #[must_use]
    pub fn into_backend(self, palace_path: &Path) -> Backend {
        match self {
            Self::Embedded { sync } => Backend::Embedded {
                path: palace_path.join("db"),
                sync,
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

/// The default admin endpoint address: loopback only, never a wildcard.
const DEFAULT_DB_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// The default admin endpoint port: SurrealDB's own, so the URL Studio
/// suggests (`ws://127.0.0.1:8000`) works unchanged.
const DEFAULT_DB_PORT: u16 = 8000;

/// Defaults for the database admin endpoint (`memcastle db start`, see
/// `docs/adr/015`).
///
/// Only defaults: the endpoint is never started by configuration. It is started
/// by an explicit `memcastle db start`, which may override each of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DbConfig {
    /// Interface the endpoint listens on. Loopback by default, and anything
    /// else is refused unless [`DbConfig::allow_remote`] is set.
    pub bind: IpAddr,
    /// TCP port. `0` asks the OS for a free port. Must differ from
    /// `server.port`, since the two are separate listeners.
    pub port: u16,
    /// Permit a non-loopback bind. Also requires `auth.enabled`: a database
    /// console on the network without a token would be the whole palace, writable.
    pub allow_remote: bool,
    /// Web page origins, beyond this machine's own, that may connect from a
    /// browser (for example `https://app.surrealdb.com`). Exact matches only.
    pub allowed_origins: Vec<String>,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_DB_BIND,
            port: DEFAULT_DB_PORT,
            allow_remote: false,
            allowed_origins: Vec::new(),
        }
    }
}

/// Where document and query embeddings come from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProvider {
    /// No provider: nothing is embedded by the daemon and search is lexical
    /// unless a caller supplies vectors itself.
    #[default]
    None,
    /// An external program the daemon runs, speaking JSON over stdin/stdout.
    /// The program owns any credentials, so MemCastle never sees them.
    Command,
    /// An OpenAI-compatible `/embeddings` HTTP endpoint (OpenAI, Ollama,
    /// llama.cpp, vLLM, ...).
    Http,
}

impl std::str::FromStr for EmbeddingProvider {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "command" => Ok(Self::Command),
            "http" => Ok(Self::Http),
            other => Err(format!(
                "unknown embedding provider `{other}`; expected one of: none, command, http"
            )),
        }
    }
}

/// The default time, in seconds, one embedding call may take.
pub const DEFAULT_EMBEDDING_TIMEOUT_SECS: u64 = 30;
/// The default number of texts sent to the provider per call.
pub const DEFAULT_EMBEDDING_BATCH_SIZE: usize = 16;

/// Embedding provider settings (`[embeddings]`).
///
/// Embeddings are derived data: the palace works without a provider, and one
/// that is configured but down degrades search to lexical rather than
/// failing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddingsConfig {
    /// Which mechanism produces embeddings.
    pub provider: EmbeddingProvider,
    /// For `provider = "command"`: the program and its arguments, run without a
    /// shell. File-only, since an argument list is awkward in an environment
    /// variable.
    pub command: Vec<String>,
    /// For `provider = "http"`: the API base URL, e.g.
    /// `http://localhost:11434/v1`; `/embeddings` is appended.
    pub url: Option<String>,
    /// For `provider = "http"`: the model name sent with each request. Also
    /// passed to a `command` provider so one script can serve several models.
    pub model: Option<String>,
    /// For `provider = "http"`: a bearer API key, when the endpoint needs one
    /// (local servers usually do not). Prefer `MEMCASTLE_EMBEDDINGS_API_KEY`
    /// so the secret stays out of the file.
    #[serde(skip_serializing)]
    pub api_key: Option<Secret>,
    /// How long, in seconds, one embedding call may take before it is
    /// abandoned (and, for a command, killed).
    pub timeout_secs: u64,
    /// How many texts one call carries; bounds a provider's request size.
    pub batch_size: usize,
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProvider::None,
            command: Vec::new(),
            url: None,
            model: None,
            api_key: None,
            timeout_secs: DEFAULT_EMBEDDING_TIMEOUT_SECS,
            batch_size: DEFAULT_EMBEDDING_BATCH_SIZE,
        }
    }
}

/// Where entities and relationships are extracted from mined text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractionProvider {
    /// No extraction: mined drawers stay unconnected in the knowledge graph.
    #[default]
    None,
    /// A small deterministic extractor built into the daemon: capitalised names and a fixed table of phrases.
    /// Nothing leaves the process, and nothing needs configuring.
    Heuristic,
    /// An external program the daemon runs, speaking JSON over stdin/stdout. The program owns any credentials and
    /// any model.
    Command,
    /// An OpenAI-compatible `/chat/completions` HTTP endpoint (OpenAI, Ollama, llama.cpp, vLLM, ...).
    Http,
}

impl std::str::FromStr for ExtractionProvider {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "heuristic" => Ok(Self::Heuristic),
            "command" => Ok(Self::Command),
            "http" => Ok(Self::Http),
            other => Err(format!(
                "unknown extraction provider `{other}`; expected one of: none, heuristic, command, http"
            )),
        }
    }
}

impl ExtractionProvider {
    /// The name recorded as a fact's extractor.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Heuristic => "heuristic",
            Self::Command => "command",
            Self::Http => "http",
        }
    }
}

/// The default time, in seconds, one extraction call may take. Longer than embedding's: a model reads and writes.
pub const DEFAULT_EXTRACTION_TIMEOUT_SECS: u64 = 120;
/// The default number of drawers read per extraction pass (and per command call).
pub const DEFAULT_EXTRACTION_BATCH_SIZE: usize = 8;
/// The default most entities kept from one drawer.
pub const DEFAULT_EXTRACTION_MAX_ENTITIES: usize = 32;
/// The default most relationships kept from one drawer.
pub const DEFAULT_EXTRACTION_MAX_RELATIONS: usize = 64;
/// The default confidence below which an extracted relationship is dropped.
pub const DEFAULT_EXTRACTION_MIN_CONFIDENCE: f32 = 0.3;

/// Entity extraction settings (`[extraction]`).
///
/// Extraction is derived data: the palace works without it, and it only ever adds entities and relationships to the
/// knowledge graph, never rewriting a drawer. `command` and `http` send mined text to whatever they reach, which is
/// why the default is `none`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractionConfig {
    /// Which mechanism reads drawers and finds entities and relationships.
    pub provider: ExtractionProvider,
    /// For `provider = "command"`: the program and its arguments, run without a shell. File-only.
    pub command: Vec<String>,
    /// For `provider = "http"`: the API base URL, e.g. `http://localhost:11434/v1`; `/chat/completions` is appended.
    pub url: Option<String>,
    /// For `provider = "http"`: the model name sent with each request. Also passed to a `command` provider.
    pub model: Option<String>,
    /// For `provider = "http"`: a bearer API key. Prefer `MEMCASTLE_EXTRACTION_API_KEY` so the secret stays out of
    /// the file.
    #[serde(skip_serializing)]
    pub api_key: Option<Secret>,
    /// How long, in seconds, one extraction call may take before it is abandoned (and, for a command, killed).
    pub timeout_secs: u64,
    /// How many drawers one pass reads; bounds a provider's request size.
    pub batch_size: usize,
    /// The most entities kept from one drawer.
    pub max_entities: usize,
    /// The most relationships kept from one drawer.
    pub max_relations: usize,
    /// Relationships below this confidence, in `[0, 1]`, are dropped.
    pub min_confidence: f32,
}

impl Default for ExtractionConfig {
    fn default() -> Self {
        Self {
            provider: ExtractionProvider::None,
            command: Vec::new(),
            url: None,
            model: None,
            api_key: None,
            timeout_secs: DEFAULT_EXTRACTION_TIMEOUT_SECS,
            batch_size: DEFAULT_EXTRACTION_BATCH_SIZE,
            max_entities: DEFAULT_EXTRACTION_MAX_ENTITIES,
            max_relations: DEFAULT_EXTRACTION_MAX_RELATIONS,
            min_confidence: DEFAULT_EXTRACTION_MIN_CONFIDENCE,
        }
    }
}

/// The default similarity, in `(0, 1]`, at or above which two drawers in one room are recorded as near-duplicates.
pub const DEFAULT_DEDUP_NEAR_THRESHOLD: f32 = 0.9;

/// Deduplication settings (`[dedup]`, docs/adr/025).
///
/// Deduplication never merges memory: an exact duplicate in the same room is not stored twice, a near-duplicate is
/// stored and linked to what it resembles, and an entity spelled differently converges on one entity only when the
/// match is unambiguous. These settings only decide how eagerly that happens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DedupConfig {
    /// Whether writes are checked against what the room already holds. Off means every write is stored and nothing
    /// is linked; entity names still converge on casing and punctuation, which is spelling, not a judgement.
    pub enabled: bool,
    /// The similarity at or above which a new drawer is linked to an existing one in its room as a near-duplicate.
    pub near_threshold: f32,
    /// Whether an entity observed with a one-character typo converges on the entity it resembles when that match is
    /// unique, and whether ambiguous resemblances are recorded as candidates.
    pub entity_fuzzy: bool,
}

impl Default for DedupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            near_threshold: DEFAULT_DEDUP_NEAR_THRESHOLD,
            entity_fuzzy: true,
        }
    }
}

/// The default chunk size, in characters. Under the 8,000 characters the embedding sweep reads of a drawer
/// (docs/adr/021), so a chunk is embedded whole.
pub const DEFAULT_MINING_CHUNK_CHARS: usize = 6_000;
/// The default largest file a directory mine reads, in bytes. Chunking is what makes a large file mineable.
pub const DEFAULT_MINING_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// The default number of documents one mining run ingests before stopping to be run again.
pub const DEFAULT_MINING_MAX_DOCUMENTS: usize = 2_000;
/// The default ceiling on a WebAssembly source's linear memory, in MiB.
pub const DEFAULT_MINING_SOURCE_MEMORY_MIB: u32 = 256;
/// The default ceiling on one call into a WebAssembly source, in seconds.
pub const DEFAULT_MINING_SOURCE_TIMEOUT_SECS: u64 = 60;

/// Mining settings (`[mining]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MiningConfig {
    /// How many characters one drawer holds at most; a longer document is cut into several drawers.
    pub chunk_chars: usize,
    /// Files larger than this many bytes are skipped by directory mining.
    pub max_file_bytes: u64,
    /// How many documents one job ingests. A source with more is mined across runs: the cursor continues where the
    /// last one stopped, and the job's result says `truncated`.
    pub max_documents: usize,
    /// Where installed WebAssembly sources live (docs/adr/026). Unset means `$XDG_DATA_HOME/memcastle/sources`.
    pub sources_dir: Option<PathBuf>,
    /// The most linear memory, in MiB, one call into a WebAssembly source may use. A source's own `[limits]` can ask
    /// for less, never more.
    pub source_memory_mib: u32,
    /// The longest, in seconds, one call into a WebAssembly source may run. A source's own `[limits]` can ask for
    /// less, never more.
    pub source_timeout_secs: u64,
}

impl MiningConfig {
    /// The directory installed sources live in: the configured one, or the default under the XDG data directory.
    #[must_use]
    pub fn sources_dir(&self) -> PathBuf {
        self.sources_dir
            .clone()
            .unwrap_or_else(paths::default_sources_dir)
    }
}

impl Default for MiningConfig {
    fn default() -> Self {
        Self {
            chunk_chars: DEFAULT_MINING_CHUNK_CHARS,
            max_file_bytes: DEFAULT_MINING_MAX_FILE_BYTES,
            max_documents: DEFAULT_MINING_MAX_DOCUMENTS,
            sources_dir: None,
            source_memory_mib: DEFAULT_MINING_SOURCE_MEMORY_MIB,
            source_timeout_secs: DEFAULT_MINING_SOURCE_TIMEOUT_SECS,
        }
    }
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

/// The default shutdown drain, in seconds. Public so the scheduler's own
/// fallback (used by direct `Scheduler::new` callers and tests) is this value
/// and not a second copy that could drift from the configured default.
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 10;
/// The default job lease TTL, in seconds; shared with the scheduler for the
/// same reason as [`DEFAULT_DRAIN_TIMEOUT_SECS`].
pub const DEFAULT_LEASE_TTL_SECS: u64 = 30;

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
            drain_timeout_secs: DEFAULT_DRAIN_TIMEOUT_SECS,
            // Long enough to ride out a garbage-collection pause or a
            // dropped packet or two, short enough that a dead daemon's jobs
            // move within the minute.
            lease_ttl_secs: DEFAULT_LEASE_TTL_SECS,
        }
    }
}

/// Logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// A `tracing_subscriber::EnvFilter` directive, e.g. `"info"` or
    /// `"memcastle=debug,tower_http=info"`.
    pub level: String,
    /// Output format: human-readable text or one JSON object per line.
    #[serde(default)]
    pub format: LogFormat,
}

/// How log events are rendered on stderr.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines, for a terminal.
    #[default]
    Text,
    /// One JSON object per line, for journald and log shippers.
    Json,
}

impl std::str::FromStr for LogFormat {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err("expected `text` or `json`".to_string()),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::default(),
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
    /// Database admin endpoint defaults.
    #[serde(default)]
    pub db: DbConfig,
    /// Embedding provider settings.
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
    /// Mining settings.
    #[serde(default)]
    pub mining: MiningConfig,
    /// Entity extraction settings.
    #[serde(default)]
    pub extraction: ExtractionConfig,
    /// Memory deduplication and entity resolution settings.
    #[serde(default)]
    pub dedup: DedupConfig,
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
        if let Some(raw) = lookup("MEMCASTLE_STORE_SYNC") {
            let parsed: StoreSync = parse_override("MEMCASTLE_STORE_SYNC", &raw)?;
            // A remote database decides its own durability, so there is
            // nothing here to apply the value to. Validated above regardless,
            // so a typo is reported whichever backend is configured.
            if let StoreConfig::Embedded { sync } = &mut self.store {
                *sync = parsed;
            }
        }
        if let Some(level) = lookup("MEMCASTLE_LOG") {
            self.logging.level = level;
        }
        if let Some(raw) = lookup("MEMCASTLE_LOG_FORMAT") {
            self.logging.format = raw
                .parse()
                .map_err(|e| Error::config(format!("MEMCASTLE_LOG_FORMAT: {e}")))?;
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
        if let Some(bind) = lookup("MEMCASTLE_DB_BIND") {
            self.db.bind = parse_bind_host(&bind)
                .map_err(|e| Error::config(format!("MEMCASTLE_DB_BIND: {e}")))?;
        }
        if let Some(port) = lookup("MEMCASTLE_DB_PORT") {
            self.db.port = parse_override("MEMCASTLE_DB_PORT", &port)?;
        }
        if let Some(raw) = lookup("MEMCASTLE_DB_ALLOW_REMOTE") {
            self.db.allow_remote = parse_override("MEMCASTLE_DB_ALLOW_REMOTE", &raw)?;
        }
        if let Some(raw) = lookup("MEMCASTLE_DB_ALLOWED_ORIGINS") {
            // Comma-separated, the way an environment variable carries a list.
            self.db.allowed_origins = raw
                .split(',')
                .map(str::trim)
                .filter(|origin| !origin.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(raw) = lookup("MEMCASTLE_EMBEDDINGS_PROVIDER") {
            self.embeddings.provider = raw
                .parse()
                .map_err(|e| Error::config(format!("MEMCASTLE_EMBEDDINGS_PROVIDER: {e}")))?;
        }
        if let Some(url) = lookup("MEMCASTLE_EMBEDDINGS_URL") {
            self.embeddings.url = Some(url.trim().to_string());
        }
        if let Some(model) = lookup("MEMCASTLE_EMBEDDINGS_MODEL") {
            self.embeddings.model = Some(model.trim().to_string());
        }
        if let Some(n) = lookup("MEMCASTLE_EMBEDDINGS_TIMEOUT_SECS") {
            self.embeddings.timeout_secs = parse_override("MEMCASTLE_EMBEDDINGS_TIMEOUT_SECS", &n)?;
        }
        if let Some(raw) = lookup("MEMCASTLE_EXTRACTION_PROVIDER") {
            self.extraction.provider = raw
                .parse()
                .map_err(|e| Error::config(format!("MEMCASTLE_EXTRACTION_PROVIDER: {e}")))?;
        }
        if let Some(url) = lookup("MEMCASTLE_EXTRACTION_URL") {
            self.extraction.url = Some(url.trim().to_string());
        }
        if let Some(model) = lookup("MEMCASTLE_EXTRACTION_MODEL") {
            self.extraction.model = Some(model.trim().to_string());
        }
        if let Some(n) = lookup("MEMCASTLE_EXTRACTION_TIMEOUT_SECS") {
            self.extraction.timeout_secs = parse_override("MEMCASTLE_EXTRACTION_TIMEOUT_SECS", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_EXTRACTION_BATCH_SIZE") {
            self.extraction.batch_size = parse_override("MEMCASTLE_EXTRACTION_BATCH_SIZE", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_EXTRACTION_MIN_CONFIDENCE") {
            self.extraction.min_confidence =
                parse_override("MEMCASTLE_EXTRACTION_MIN_CONFIDENCE", &n)?;
        }
        if let Some(v) = lookup("MEMCASTLE_DEDUP_ENABLED") {
            self.dedup.enabled = parse_override("MEMCASTLE_DEDUP_ENABLED", &v)?;
        }
        if let Some(n) = lookup("MEMCASTLE_DEDUP_NEAR_THRESHOLD") {
            self.dedup.near_threshold = parse_override("MEMCASTLE_DEDUP_NEAR_THRESHOLD", &n)?;
        }
        if let Some(v) = lookup("MEMCASTLE_DEDUP_ENTITY_FUZZY") {
            self.dedup.entity_fuzzy = parse_override("MEMCASTLE_DEDUP_ENTITY_FUZZY", &v)?;
        }
        if let Some(n) = lookup("MEMCASTLE_MINING_CHUNK_CHARS") {
            self.mining.chunk_chars = parse_override("MEMCASTLE_MINING_CHUNK_CHARS", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_MINING_MAX_FILE_BYTES") {
            self.mining.max_file_bytes = parse_override("MEMCASTLE_MINING_MAX_FILE_BYTES", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_MINING_MAX_DOCUMENTS") {
            self.mining.max_documents = parse_override("MEMCASTLE_MINING_MAX_DOCUMENTS", &n)?;
        }
        if let Some(dir) = lookup("MEMCASTLE_MINING_SOURCES_DIR") {
            self.mining.sources_dir = Some(PathBuf::from(dir));
        }
        if let Some(n) = lookup("MEMCASTLE_MINING_SOURCE_MEMORY_MIB") {
            self.mining.source_memory_mib =
                parse_override("MEMCASTLE_MINING_SOURCE_MEMORY_MIB", &n)?;
        }
        if let Some(n) = lookup("MEMCASTLE_MINING_SOURCE_TIMEOUT_SECS") {
            self.mining.source_timeout_secs =
                parse_override("MEMCASTLE_MINING_SOURCE_TIMEOUT_SECS", &n)?;
        }
        // Same reasoning as the auth token below: a secret, so no
        // `parse_override`, whose error would echo the value.
        if let Some(key) = lookup("MEMCASTLE_EMBEDDINGS_API_KEY") {
            self.embeddings.api_key = Some(Secret::new(key.trim()));
        }
        if let Some(key) = lookup("MEMCASTLE_EXTRACTION_API_KEY") {
            self.extraction.api_key = Some(Secret::new(key.trim()));
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
        // Caught here rather than at `db start` so a bad `[db]` section is
        // reported when the config is loaded, not the first time it is used.
        // That the endpoint then needs authentication is checked at start, where
        // the daemon's real policy is known.
        if !self.db.bind.is_loopback() && !self.db.allow_remote {
            return Err(Error::config(format!(
                "db.bind {} is not a loopback address; the admin endpoint exposes the whole database, \
                 so listening elsewhere needs `db.allow_remote = true` (or MEMCASTLE_DB_ALLOW_REMOTE=true) \
                 together with `auth.enabled`",
                self.db.bind
            )));
        }
        // Two listeners cannot share a port; port 0 is exempt because the OS
        // picks a distinct one each time.
        if self.db.port != 0 && self.db.port == self.server.port {
            return Err(Error::config(format!(
                "db.port {} is the same as server.port; the admin endpoint is a separate listener, \
                 so give it its own port",
                self.db.port
            )));
        }
        // The message never includes the token itself, only what to do about it.
        if let Some(token) = &self.auth.token
            && token.expose().trim() != token.expose()
        {
            // The environment variable is trimmed on the way in, so only a
            // file value can get here. The authentication layer trims the
            // token a client presents, so a stored one with edge whitespace
            // could never match and every request would be refused with no
            // hint why.
            return Err(Error::config(
                "auth.token must not start or end with whitespace; remove the stray space or newline \
                 from the value in the configuration file",
            ));
        }
        if let Some(token) = &self.auth.token
            && token.expose().len() < MIN_TOKEN_LEN
        {
            return Err(Error::config(format!(
                "auth.token (or MEMCASTLE_AUTH_TOKEN) must be at least {MIN_TOKEN_LEN} characters; \
                 generate a strong one with `memcastle auth generate`"
            )));
        }
        self.validate_embeddings()?;
        self.validate_extraction()?;
        self.validate_mining()?;
        self.validate_dedup()?;
        Ok(())
    }

    /// The `[dedup]` invariants. A threshold of 0 would link every pair of drawers, so the floor is well above
    /// "unrelated"; one above 1 could never match.
    fn validate_dedup(&self) -> Result<()> {
        // `contains` is false for NaN, so a non-number is rejected too.
        if !(0.5..=1.0).contains(&self.dedup.near_threshold) {
            return Err(Error::config(
                "dedup.near_threshold (or MEMCASTLE_DEDUP_NEAR_THRESHOLD) must be between 0.5 and 1",
            ));
        }
        Ok(())
    }

    /// The `[extraction]` invariants: a selected provider must be fully specified, so a half-written section fails
    /// at load, not on the first sweep, and the bounds keep a typo from asking a model for nothing or everything.
    fn validate_extraction(&self) -> Result<()> {
        let extraction = &self.extraction;
        match extraction.provider {
            ExtractionProvider::None | ExtractionProvider::Heuristic => {}
            ExtractionProvider::Command => {
                if extraction
                    .command
                    .first()
                    .is_none_or(|program| program.trim().is_empty())
                {
                    return Err(Error::config(
                        "extraction.command is empty; with `provider = \"command\"` set it to the \
                         program and arguments to run, e.g. `command = [\"/usr/local/bin/extract\"]`",
                    ));
                }
            }
            ExtractionProvider::Http => {
                let url = extraction.url.as_deref().unwrap_or_default();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(Error::config(
                        "extraction.url (or MEMCASTLE_EXTRACTION_URL) must be an http:// or https:// \
                         URL when `provider = \"http\"`, e.g. `http://localhost:11434/v1`",
                    ));
                }
                if extraction
                    .model
                    .as_deref()
                    .is_none_or(|model| model.trim().is_empty())
                {
                    return Err(Error::config(
                        "extraction.model (or MEMCASTLE_EXTRACTION_MODEL) is required when \
                         `provider = \"http\"`",
                    ));
                }
            }
        }
        if !(1..=3_600).contains(&extraction.timeout_secs) {
            return Err(Error::config(
                "extraction.timeout_secs (or MEMCASTLE_EXTRACTION_TIMEOUT_SECS) must be between 1 and 3600 seconds",
            ));
        }
        if !(1..=256).contains(&extraction.batch_size) {
            return Err(Error::config(
                "extraction.batch_size (or MEMCASTLE_EXTRACTION_BATCH_SIZE) must be between 1 and 256",
            ));
        }
        if !(1..=1_000).contains(&extraction.max_entities) {
            return Err(Error::config(
                "extraction.max_entities must be between 1 and 1000",
            ));
        }
        if !(1..=10_000).contains(&extraction.max_relations) {
            return Err(Error::config(
                "extraction.max_relations must be between 1 and 10000",
            ));
        }
        // `contains` is false for NaN, so a non-number is rejected too.
        if !(0.0..=1.0).contains(&extraction.min_confidence) {
            return Err(Error::config(
                "extraction.min_confidence (or MEMCASTLE_EXTRACTION_MIN_CONFIDENCE) must be between 0 and 1",
            ));
        }
        Ok(())
    }

    /// The `[mining]` invariants. The bounds keep a typo from producing either thousands of one-character drawers
    /// or one drawer too large to embed or read.
    fn validate_mining(&self) -> Result<()> {
        let mining = &self.mining;
        if !(200..=100_000).contains(&mining.chunk_chars) {
            return Err(Error::config(
                "mining.chunk_chars (or MEMCASTLE_MINING_CHUNK_CHARS) must be between 200 and 100000",
            ));
        }
        if !(1..=64 * 1024 * 1024).contains(&mining.max_file_bytes) {
            return Err(Error::config(
                "mining.max_file_bytes (or MEMCASTLE_MINING_MAX_FILE_BYTES) must be between 1 and 67108864",
            ));
        }
        if !(1..=1_000_000).contains(&mining.max_documents) {
            return Err(Error::config(
                "mining.max_documents (or MEMCASTLE_MINING_MAX_DOCUMENTS) must be between 1 and 1000000",
            ));
        }
        // Relative to whatever directory the daemon started in, like `palace.path` and `assets.dir`.
        if let Some(dir) = mining.sources_dir.as_ref().filter(|dir| !dir.is_absolute()) {
            return Err(Error::config(format!(
                "mining.sources_dir {:?} is not an absolute path; set an absolute one or remove it \
                 to use the default under the XDG data directory",
                dir.display().to_string()
            )));
        }
        if !(16..=4096).contains(&mining.source_memory_mib) {
            return Err(Error::config(
                "mining.source_memory_mib (or MEMCASTLE_MINING_SOURCE_MEMORY_MIB) must be between 16 and 4096",
            ));
        }
        if !(1..=3600).contains(&mining.source_timeout_secs) {
            return Err(Error::config(
                "mining.source_timeout_secs (or MEMCASTLE_MINING_SOURCE_TIMEOUT_SECS) must be between 1 and 3600",
            ));
        }
        Ok(())
    }

    /// The `[embeddings]` invariants: a provider that is selected must be
    /// fully specified, so a half-written section fails at load, not at the
    /// first search.
    fn validate_embeddings(&self) -> Result<()> {
        let embeddings = &self.embeddings;
        match embeddings.provider {
            EmbeddingProvider::None => {}
            EmbeddingProvider::Command => {
                if embeddings
                    .command
                    .first()
                    .is_none_or(|program| program.trim().is_empty())
                {
                    return Err(Error::config(
                        "embeddings.command is empty; with `provider = \"command\"` set it to the \
                         program and arguments to run, e.g. `command = [\"/usr/local/bin/embed\"]`",
                    ));
                }
            }
            EmbeddingProvider::Http => {
                let url = embeddings.url.as_deref().unwrap_or_default();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(Error::config(
                        "embeddings.url (or MEMCASTLE_EMBEDDINGS_URL) must be an http:// or https:// \
                         URL when `provider = \"http\"`, e.g. `http://localhost:11434/v1`",
                    ));
                }
                if embeddings
                    .model
                    .as_deref()
                    .is_none_or(|model| model.trim().is_empty())
                {
                    return Err(Error::config(
                        "embeddings.model (or MEMCASTLE_EMBEDDINGS_MODEL) is required when \
                         `provider = \"http\"`",
                    ));
                }
            }
        }
        // A zero timeout would fail every call at once; a day is a units mistake.
        if !(1..=3_600).contains(&embeddings.timeout_secs) {
            return Err(Error::config(
                "embeddings.timeout_secs must be between 1 and 3600 seconds",
            ));
        }
        if !(1..=1_024).contains(&embeddings.batch_size) {
            return Err(Error::config(
                "embeddings.batch_size must be between 1 and 1024",
            ));
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
    fn a_file_token_with_edge_whitespace_is_refused_without_echoing_it() {
        let padded = format!(" {TOKEN}\n");
        let config: Config = toml::from_str(&format!("[auth]\ntoken = {padded:?}")).unwrap();

        let message = config.validate().unwrap_err().to_string();

        assert!(message.contains("auth.token"), "{message}");
        assert!(message.contains("whitespace"), "{message}");
        assert!(
            !message.contains(TOKEN),
            "the token must not be echoed: {message}"
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
    fn the_remote_database_password_never_appears_in_debug_output_or_a_serialised_config() {
        const PASSWORD: &str = "root-password-hunter2";
        let config = Config {
            store: StoreConfig::Remote {
                url: "ws://db.example.com".into(),
                namespace: "n".into(),
                database: "d".into(),
                username: "root".into(),
                password: Secret::new(PASSWORD),
            },
            ..Config::default()
        };

        assert!(!format!("{config:?}").contains(PASSWORD));
        assert!(!toml::to_string(&config).unwrap().contains(PASSWORD));
        assert!(!serde_json::to_string(&config).unwrap().contains(PASSWORD));
        // The resolved backend is what the store logs and holds, so it must redact too.
        let backend = config.store.into_backend(Path::new("/unused"));
        assert!(!format!("{backend:?}").contains(PASSWORD));
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
    fn the_log_format_env_override_selects_json_and_rejects_nonsense() {
        let mut config = Config::default();
        assert_eq!(config.logging.format, LogFormat::Text);
        config
            .apply_overrides_from(env(&[("MEMCASTLE_LOG_FORMAT", "JSON")]))
            .unwrap();
        assert_eq!(config.logging.format, LogFormat::Json);

        let err = config
            .apply_overrides_from(env(&[("MEMCASTLE_LOG_FORMAT", "xml")]))
            .unwrap_err();
        assert!(err.to_string().contains("MEMCASTLE_LOG_FORMAT"), "{err}");
    }

    #[test]
    fn a_log_format_in_the_config_file_is_read() {
        let config: Config =
            toml::from_str("[logging]\nlevel = \"info\"\nformat = \"json\"").unwrap();
        assert_eq!(config.logging.format, LogFormat::Json);
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
        let backend = StoreConfig::default().into_backend(Path::new("/tmp/palace"));
        match backend {
            Backend::Embedded { path, sync } => {
                assert_eq!(path, Path::new("/tmp/palace/db"));
                assert_eq!(sync, StoreSync::Every, "durable unless told otherwise");
            }
            Backend::Remote { .. } => panic!("expected an embedded backend"),
        }
    }

    #[test]
    fn the_store_sync_comes_from_the_file_and_the_environment_outranks_it() {
        let mut config: Config =
            toml::from_str("[store]\nmode = \"embedded\"\nsync = \"5s\"").unwrap();
        assert!(matches!(
            config.store,
            StoreConfig::Embedded { sync } if sync == "5s".parse().unwrap()
        ));

        config
            .apply_overrides_from(env(&[("MEMCASTLE_STORE_SYNC", "never")]))
            .unwrap();
        assert!(matches!(
            config.store,
            StoreConfig::Embedded {
                sync: StoreSync::Never
            }
        ));
    }

    #[test]
    fn an_embedded_store_without_a_sync_setting_stays_durable() {
        // The shape every existing config file has.
        let config: Config = toml::from_str("[store]\nmode = \"embedded\"").unwrap();
        assert!(matches!(
            config.store,
            StoreConfig::Embedded {
                sync: StoreSync::Every
            }
        ));
    }

    #[test]
    fn a_bad_store_sync_is_refused_by_name_from_the_file_and_the_environment() {
        assert!(toml::from_str::<Config>("[store]\nmode = \"embedded\"\nsync = \"soon\"").is_err());

        let err = Config::default()
            .apply_overrides_from(env(&[("MEMCASTLE_STORE_SYNC", "soon")]))
            .unwrap_err();
        assert!(err.to_string().contains("MEMCASTLE_STORE_SYNC"), "{err}");
    }

    #[test]
    fn the_store_sync_environment_variable_does_not_touch_a_remote_store() {
        let mut config: Config = toml::from_str(
            "[store]\nmode = \"remote\"\nurl = \"ws://x:8000\"\nnamespace = \"n\"\ndatabase = \"d\"\n\
             username = \"root\"\npassword = \"p\"",
        )
        .unwrap();
        config
            .apply_overrides_from(env(&[("MEMCASTLE_STORE_SYNC", "never")]))
            .unwrap();
        assert!(matches!(config.store, StoreConfig::Remote { .. }));
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
    fn the_database_admin_endpoint_defaults_to_loopback_on_surrealdbs_own_port_and_no_remote() {
        let db = Config::default().db;
        assert_eq!(db.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(db.port, 8000);
        assert!(!db.allow_remote);
        assert!(db.allowed_origins.is_empty());
    }

    #[test]
    fn the_db_settings_come_from_the_file_then_the_environment() {
        let mut config: Config =
            toml::from_str("[db]\nport = 9000\nallowed_origins = [\"https://a.example\"]").unwrap();
        assert_eq!(config.db.port, 9000);
        assert_eq!(config.db.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));

        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_DB_BIND", "127.0.0.3"),
                ("MEMCASTLE_DB_PORT", "9100"),
                ("MEMCASTLE_DB_ALLOW_REMOTE", "true"),
                (
                    "MEMCASTLE_DB_ALLOWED_ORIGINS",
                    "https://b.example, https://c.example,",
                ),
            ]))
            .unwrap();
        assert_eq!(config.db.bind, "127.0.0.3".parse::<IpAddr>().unwrap());
        assert_eq!(config.db.port, 9100);
        assert!(config.db.allow_remote);
        assert_eq!(
            config.db.allowed_origins,
            ["https://b.example", "https://c.example"]
        );
    }

    #[test]
    fn a_malformed_db_variable_is_named_in_the_error() {
        for (name, value) in [
            ("MEMCASTLE_DB_BIND", "nowhere"),
            ("MEMCASTLE_DB_PORT", "eighty"),
            ("MEMCASTLE_DB_ALLOW_REMOTE", "maybe"),
        ] {
            let err = Config::default()
                .apply_overrides_from(env(&[(name, value)]))
                .unwrap_err();
            assert!(err.to_string().contains(name), "{err}");
        }
    }

    #[test]
    fn mining_settings_come_from_the_file_then_the_environment() {
        let mut config: Config = toml::from_str("[mining]\nchunk_chars = 1000").unwrap();
        assert_eq!(config.mining.chunk_chars, 1000);
        assert_eq!(config.mining.max_documents, DEFAULT_MINING_MAX_DOCUMENTS);

        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_MINING_CHUNK_CHARS", "2000"),
                ("MEMCASTLE_MINING_MAX_FILE_BYTES", "4096"),
                ("MEMCASTLE_MINING_MAX_DOCUMENTS", "7"),
            ]))
            .unwrap();
        assert_eq!(config.mining.chunk_chars, 2000);
        assert_eq!(config.mining.max_file_bytes, 4096);
        assert_eq!(config.mining.max_documents, 7);
    }

    #[test]
    fn a_malformed_mining_variable_is_named_in_the_error() {
        let err = Config::default()
            .apply_overrides_from(env(&[("MEMCASTLE_MINING_CHUNK_CHARS", "big")]))
            .unwrap_err();
        assert!(
            err.to_string().contains("MEMCASTLE_MINING_CHUNK_CHARS"),
            "{err}"
        );
    }

    #[test]
    fn out_of_range_mining_settings_are_rejected_by_name() {
        for (edit, name) in [
            (
                (|c: &mut Config| c.mining.chunk_chars = 10) as fn(&mut Config),
                "mining.chunk_chars",
            ),
            (|c| c.mining.max_documents = 0, "mining.max_documents"),
            (|c| c.mining.max_file_bytes = 0, "mining.max_file_bytes"),
            (
                |c| c.mining.sources_dir = Some(PathBuf::from("relative")),
                "mining.sources_dir",
            ),
            (
                |c| c.mining.source_memory_mib = 1,
                "mining.source_memory_mib",
            ),
            (
                |c| c.mining.source_timeout_secs = 0,
                "mining.source_timeout_secs",
            ),
        ] {
            let mut config = Config::default();
            config.palace.path = std::env::temp_dir();
            edit(&mut config);
            let err = config.validate().unwrap_err();
            assert!(err.to_string().contains(name), "{err}");
        }
    }

    #[test]
    fn a_non_loopback_db_bind_is_rejected_unless_remote_access_was_allowed() {
        let mut config = Config::default();
        // Absolute on every platform; "/palace" is not on Windows.
        config.palace.path = std::env::temp_dir();
        config.db.bind = "0.0.0.0".parse().unwrap();

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("db.allow_remote"), "{err}");

        config.db.allow_remote = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn the_db_port_may_not_collide_with_the_daemons_own() {
        let mut config = Config::default();
        // Absolute on every platform; "/palace" is not on Windows.
        config.palace.path = std::env::temp_dir();
        config.db.port = config.server.port;
        assert!(config.validate().is_err());

        // Port 0 means "the OS picks", so two zeros never collide.
        config.db.port = 0;
        config.server.port = 0;
        assert!(config.validate().is_ok());
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

    /// A valid config (absolute palace path) to build `[embeddings]` cases on.
    fn valid_config() -> Config {
        let mut config = Config::default();
        config.palace.path = std::env::temp_dir();
        config
    }

    #[test]
    fn no_embedding_provider_is_configured_by_default_and_validates() {
        let config = valid_config();
        assert_eq!(config.embeddings.provider, EmbeddingProvider::None);
        config.validate().expect("a palace needs no provider");
    }

    #[test]
    fn a_partial_embeddings_section_keeps_the_other_defaults() {
        let config: Config = toml::from_str(
            "[embeddings]\nprovider = \"http\"\nurl = \"http://localhost:11434/v1\"\nmodel = \"nomic\"",
        )
        .unwrap();
        assert_eq!(config.embeddings.provider, EmbeddingProvider::Http);
        assert_eq!(
            config.embeddings.timeout_secs,
            DEFAULT_EMBEDDING_TIMEOUT_SECS
        );
        assert_eq!(config.embeddings.batch_size, DEFAULT_EMBEDDING_BATCH_SIZE);
    }

    #[test]
    fn embedding_environment_overrides_replace_the_file_values() {
        let mut config = valid_config();
        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_EMBEDDINGS_PROVIDER", "http"),
                ("MEMCASTLE_EMBEDDINGS_URL", " http://localhost:1234/v1 "),
                ("MEMCASTLE_EMBEDDINGS_MODEL", "m"),
                ("MEMCASTLE_EMBEDDINGS_TIMEOUT_SECS", "5"),
                ("MEMCASTLE_EMBEDDINGS_API_KEY", "sk-secret "),
            ]))
            .unwrap();
        assert_eq!(config.embeddings.provider, EmbeddingProvider::Http);
        assert_eq!(
            config.embeddings.url.as_deref(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(config.embeddings.timeout_secs, 5);
        assert_eq!(
            config.embeddings.api_key.as_ref().unwrap().expose(),
            "sk-secret"
        );
        config
            .validate()
            .expect("a complete http provider is valid");
    }

    #[test]
    fn a_malformed_provider_override_names_the_variable() {
        let mut config = valid_config();
        let message = config
            .apply_overrides_from(env(&[("MEMCASTLE_EMBEDDINGS_PROVIDER", "openai")]))
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("MEMCASTLE_EMBEDDINGS_PROVIDER"),
            "{message}"
        );
        assert!(
            message.contains("command"),
            "the valid names are listed: {message}"
        );
    }

    #[test]
    fn a_selected_provider_must_be_fully_specified() {
        let mut http = valid_config();
        http.embeddings.provider = EmbeddingProvider::Http;
        let message = http.validate().unwrap_err().to_string();
        assert!(message.contains("embeddings.url"), "{message}");

        http.embeddings.url = Some("http://localhost:1/v1".into());
        let message = http.validate().unwrap_err().to_string();
        assert!(message.contains("embeddings.model"), "{message}");

        let mut command = valid_config();
        command.embeddings.provider = EmbeddingProvider::Command;
        let message = command.validate().unwrap_err().to_string();
        assert!(message.contains("embeddings.command"), "{message}");
        command.embeddings.command = vec!["/usr/local/bin/embed".into()];
        command
            .validate()
            .expect("a command provider with a program is valid");
    }

    #[test]
    fn embedding_timeout_and_batch_size_are_range_checked() {
        let mut config = valid_config();
        config.embeddings.timeout_secs = 0;
        assert!(config.validate().is_err());
        config.embeddings.timeout_secs = DEFAULT_EMBEDDING_TIMEOUT_SECS;
        config.embeddings.batch_size = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn an_embedding_api_key_is_neither_printed_nor_serialised() {
        let mut config = valid_config();
        config.embeddings.api_key = Some(Secret::new("sk-very-secret"));
        assert!(!format!("{config:?}").contains("sk-very-secret"));
        assert!(!toml::to_string(&config).unwrap().contains("sk-very-secret"));
    }

    #[test]
    fn extraction_is_off_by_default_and_the_default_section_validates() {
        let config = valid_config();
        assert_eq!(config.extraction.provider, ExtractionProvider::None);
        config.validate().expect("a palace needs no extractor");
    }

    #[test]
    fn the_built_in_heuristic_needs_no_further_settings() {
        let mut config = valid_config();
        config.extraction.provider = ExtractionProvider::Heuristic;
        config
            .validate()
            .expect("heuristic is complete as it stands");
    }

    #[test]
    fn a_selected_extraction_provider_must_be_fully_specified() {
        let mut http = valid_config();
        http.extraction.provider = ExtractionProvider::Http;
        let message = http.validate().unwrap_err().to_string();
        assert!(message.contains("extraction.url"), "{message}");
        http.extraction.url = Some("http://localhost:1/v1".into());
        let message = http.validate().unwrap_err().to_string();
        assert!(message.contains("extraction.model"), "{message}");
        http.extraction.model = Some("m".into());
        http.validate().expect("a complete http provider is valid");

        let mut command = valid_config();
        command.extraction.provider = ExtractionProvider::Command;
        let message = command.validate().unwrap_err().to_string();
        assert!(message.contains("extraction.command"), "{message}");
        command.extraction.command = vec!["/usr/local/bin/extract".into()];
        command
            .validate()
            .expect("a command with a program is valid");
    }

    #[test]
    fn extraction_bounds_are_range_checked() {
        let cases: [fn(&mut ExtractionConfig); 5] = [
            |c| c.timeout_secs = 0,
            |c| c.batch_size = 0,
            |c| c.max_entities = 0,
            |c| c.max_relations = 0,
            |c| c.min_confidence = 1.5,
        ];
        for case in cases {
            let mut config = valid_config();
            case(&mut config.extraction);
            assert!(config.validate().is_err());
        }
        let mut nan = valid_config();
        nan.extraction.min_confidence = f32::NAN;
        assert!(nan.validate().is_err(), "NaN is not a confidence");
    }

    #[test]
    fn dedup_is_on_by_default_with_a_conservative_threshold() {
        let config = valid_config();
        assert!(config.dedup.enabled && config.dedup.entity_fuzzy);
        assert!((config.dedup.near_threshold - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn a_dedup_threshold_that_would_link_unrelated_drawers_is_rejected() {
        for bad in [0.0, 0.2, 1.1, f32::NAN] {
            let mut config = valid_config();
            config.dedup.near_threshold = bad;
            assert!(config.validate().is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn dedup_environment_overrides_replace_the_file_values() {
        let mut config = valid_config();
        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_DEDUP_ENABLED", "false"),
                ("MEMCASTLE_DEDUP_NEAR_THRESHOLD", "0.8"),
                ("MEMCASTLE_DEDUP_ENTITY_FUZZY", "false"),
            ]))
            .unwrap();
        assert!(!config.dedup.enabled && !config.dedup.entity_fuzzy);
        assert!((config.dedup.near_threshold - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn extraction_environment_overrides_replace_the_file_values() {
        let mut config = valid_config();
        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_EXTRACTION_PROVIDER", "http"),
                ("MEMCASTLE_EXTRACTION_URL", " http://localhost:1234/v1 "),
                ("MEMCASTLE_EXTRACTION_MODEL", "m"),
                ("MEMCASTLE_EXTRACTION_TIMEOUT_SECS", "5"),
                ("MEMCASTLE_EXTRACTION_BATCH_SIZE", "2"),
                ("MEMCASTLE_EXTRACTION_MIN_CONFIDENCE", "0.7"),
                ("MEMCASTLE_EXTRACTION_API_KEY", "sk-secret "),
            ]))
            .unwrap();
        assert_eq!(config.extraction.provider, ExtractionProvider::Http);
        assert_eq!(
            config.extraction.url.as_deref(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(config.extraction.timeout_secs, 5);
        assert_eq!(config.extraction.batch_size, 2);
        assert!((config.extraction.min_confidence - 0.7).abs() < f32::EPSILON);
        assert_eq!(
            config.extraction.api_key.as_ref().unwrap().expose(),
            "sk-secret"
        );
        config
            .validate()
            .expect("a complete http provider is valid");
    }

    #[test]
    fn a_malformed_extraction_provider_override_names_the_variable_and_the_choices() {
        let mut config = valid_config();
        let message = config
            .apply_overrides_from(env(&[("MEMCASTLE_EXTRACTION_PROVIDER", "gpt")]))
            .unwrap_err()
            .to_string();
        assert!(
            message.contains("MEMCASTLE_EXTRACTION_PROVIDER"),
            "{message}"
        );
        assert!(message.contains("heuristic"), "{message}");
    }

    #[test]
    fn an_extraction_api_key_is_neither_printed_nor_serialised() {
        let mut config = valid_config();
        config.extraction.api_key = Some(Secret::new("sk-very-secret"));
        assert!(!format!("{config:?}").contains("sk-very-secret"));
        assert!(!toml::to_string(&config).unwrap().contains("sk-very-secret"));
    }

    #[test]
    fn installed_source_settings_come_from_the_file_then_the_environment() {
        let mut config: Config = toml::from_str("[mining]\nsource_timeout_secs = 5").unwrap();
        assert_eq!(config.mining.source_timeout_secs, 5);
        assert_eq!(
            config.mining.source_memory_mib,
            DEFAULT_MINING_SOURCE_MEMORY_MIB
        );

        config
            .apply_overrides_from(env(&[
                ("MEMCASTLE_MINING_SOURCES_DIR", "/srv/sources"),
                ("MEMCASTLE_MINING_SOURCE_MEMORY_MIB", "128"),
                ("MEMCASTLE_MINING_SOURCE_TIMEOUT_SECS", "9"),
            ]))
            .unwrap();

        assert_eq!(config.mining.sources_dir(), PathBuf::from("/srv/sources"));
        assert_eq!(config.mining.source_memory_mib, 128);
        assert_eq!(config.mining.source_timeout_secs, 9);
        // Unset, the directory is the default under the XDG data directory.
        assert!(MiningConfig::default().sources_dir().ends_with("sources"));
    }

    #[test]
    fn a_malformed_source_variable_is_named_in_the_error() {
        for name in [
            "MEMCASTLE_MINING_SOURCE_MEMORY_MIB",
            "MEMCASTLE_MINING_SOURCE_TIMEOUT_SECS",
        ] {
            let err = Config::default()
                .apply_overrides_from(env(&[(name, "lots")]))
                .unwrap_err();
            assert!(err.to_string().contains(name), "{err}");
        }
    }
}
