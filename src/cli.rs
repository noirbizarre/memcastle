//! Argument types only.
//!
//! No behaviour lives here: parsing is one concern and doing the work is
//! another, and keeping them apart is what lets the library be used without
//! the CLI. See `main.rs` for what each variant actually does.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Parser, Subcommand, ValueHint};
use memcastle::domain::{JobStatus, MemoryMode};

/// The colours of `--help` and of clap's own error messages.
///
/// Plain ANSI palette colours (not RGB), so they follow the user's terminal
/// theme. The binary sets clap's colour choice before parsing so `FORCE_COLOR`
/// and `NO_COLOR` apply even to help and parse errors.
fn styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
        .literal(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Green.on_default())
        .error(AnsiColor::Red.on_default() | Effects::BOLD)
        .valid(AnsiColor::Green.on_default())
        .invalid(AnsiColor::Yellow.on_default())
}

/// The names `--mode` accepts, as a clap value parser: it lists them in the
/// error for an unknown one and offers them to shell completion, which a bare
/// `FromStr` cannot.
fn mode_parser() -> impl TypedValueParser<Value = MemoryMode> {
    PossibleValuesParser::new([
        MemoryMode::Full.as_str(),
        MemoryMode::ReadOnly.as_str(),
        MemoryMode::Disabled.as_str(),
    ])
    .try_map(|name| name.parse::<MemoryMode>())
}

/// The names `job list --status` accepts; see [`mode_parser`].
fn status_parser() -> PossibleValuesParser {
    PossibleValuesParser::new([
        JobStatus::Queued.as_str(),
        JobStatus::Running.as_str(),
        JobStatus::Paused.as_str(),
        JobStatus::Completed.as_str(),
        JobStatus::Failed.as_str(),
        JobStatus::Cancelled.as_str(),
    ])
}

/// Local-first, always-on memory server for AI coding agents over MCP/HTTP
#[derive(Debug, Parser)]
#[command(
    name = "memcastle",
    version,
    about,
    long_about = None,
    styles = styles(),
    // `memcastle help` only repeats `--help`, and adds a `help` entry to every
    // command group (`job help`, `diary help`, ...) that does the same.
    disable_help_subcommand = true,
)]
pub struct Cli {
    /// Increase verbosity: `-v` logs memcastle at debug level, `-vv` at
    /// trace, and either prints the full cause chain of an error.
    /// `MEMCASTLE_LOG` and `RUST_LOG` take precedence over it.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Run this command in a memory mode — `full` (the default),
    /// `read_only` or `disabled` — exactly as an agent session in that mode
    /// would: the daemon rejects what the mode forbids. Useful to check what
    /// a restricted session can and cannot do.
    #[arg(long, global = true, env = "MEMCASTLE_MODE", value_parser = mode_parser())]
    pub mode: Option<MemoryMode>,

    /// Path to a config file. Defaults to `$XDG_CONFIG_HOME/memcastle/config.toml`
    /// (`~/.config/memcastle/config.toml`) if it exists.
    #[arg(long, global = true, env = "MEMCASTLE_CONFIG", value_hint = ValueHint::FilePath)]
    pub config: Option<PathBuf>,

    /// The palace directory to use, overriding `palace.path` and
    /// `MEMCASTLE_PALACE_PATH`. Defaults to `$XDG_DATA_HOME/memcastle/default`
    /// (`~/.local/share/memcastle/default`). Must be an absolute path.
    /// Deliberately not bound to the environment variable here: that one is
    /// applied by the config loader, one layer below this flag.
    #[arg(long, global = true, value_name = "PATH", value_hint = ValueHint::DirPath)]
    pub palace: Option<PathBuf>,

    /// Print JSON even in a terminal. Without it a terminal gets a readable
    /// rendering and anything else (a pipe, a file) gets JSON. Commands with no
    /// data to report (`daemon start`, `source build`, `auth generate`, ...)
    /// print the same text either way.
    #[arg(long, global = true)]
    pub json: bool,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon in the foreground, logging to standard error. This is
    /// what a process manager runs; `daemon start` runs it in the background.
    Serve(ServeArgs),
    /// Start, stop or restart the daemon as a background process.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Bring this palace's data up to date, or just report on it — the
    /// exact same runner `serve` uses on every startup, never a second
    /// migration system. Connects to storage directly, like `serve`; does
    /// not require (and does not talk to) a running daemon.
    Migrate(MigrateArgs),
    /// Report whether the daemon is running, where it listens, which palace
    /// it serves and whether its datastore is healthy and migrated.
    /// Exit codes: 0 running and healthy, 1 running but degraded (or an
    /// error), 3 not running.
    Status,
    /// Open the interactive operations console for this daemon.
    Tui,
    /// Diagnose local configuration and prerequisites, plus daemon health when available.
    /// Exit codes: 0 if no blocking errors (including when offline), 1 otherwise.
    Doctor,
    /// Search palace drawer content.
    Search(SearchArgs),
    /// Retrieve palace content matching a query, returned verbatim — the
    /// recall-oriented counterpart to `search` (see
    /// `AppServices::recall`'s doc comment for why both exist).
    Recall(RecallArgs),
    /// Build an agent identity's session-start context: its most recent
    /// diary entry (when a wing is given) plus recent checkpoint-originated
    /// highlights, bounded by a deterministic item/byte budget.
    #[command(alias = "wake_up")]
    WakeUp(WakeUpArgs),
    /// Submit a mining job: `mine <source> [place] [key=value]...`, or
    /// `mine <path>` for a directory. Mining is incremental: a source
    /// remembers where the last run stopped, and unchanged documents are not
    /// filed again.
    Mine(MineArgs),
    /// List the sources the daemon can mine and the ones it has mined, with
    /// where each one's last run stopped.
    Sources,
    /// Develop, package and install mining sources: WebAssembly components
    /// that read an origin (a chat export, an issue tracker, an agent's
    /// session files) and hand MemCastle documents to file. `init`, `build`,
    /// `test` and `package` are local and need no daemon.
    #[command(subcommand)]
    Source(SourceCommand),
    /// Configure what the daemon mines: named miners kept as `[[miners]]` in the
    /// configuration file, each a source, a locator, a scope and a credential.
    /// Administrative: changing a miner is never available to MCP clients, which
    /// can only read them.
    #[command(subcommand)]
    Miner(MinerCommand),
    /// Configure what asks for a mining run on its own (a timetable, a poll, a
    /// webhook, a file watcher). Every trigger starts disabled.
    #[command(subcommand)]
    Trigger(TriggerCommand),
    /// Install, update and remove the integrations MemCastle ships for coding
    /// agents (Pi, OpenCode). Local: needs no daemon.
    #[command(subcommand)]
    Integration(IntegrationCommand),
    /// Capture a note: a thought written down as it comes, filed under the
    /// current project and kept verbatim. The text is an argument, a file
    /// (`--file`, `-` for standard input), piped standard input, or written
    /// in `$VISUAL`/`$EDITOR` (`--edit`, or no text on a terminal).
    Note(NoteArgs),
    /// Submit a checkpoint job: persist an already-classified batch of
    /// memory writes.
    Checkpoint(CheckpointArgs),
    /// Submit an audit job: a read-only palace consistency report.
    Audit(AuditArgs),
    /// Submit an embedding job: compute the vector of every drawer that has
    /// none, so semantic search covers it. Needs an `[embeddings]` provider.
    Embed(EmbedArgs),
    /// Submit an extraction job: read every mined drawer not yet read and add
    /// the entities and relationships it names to the knowledge graph. Needs
    /// an `[extraction]` provider.
    Extract(ExtractArgs),
    /// Submit a repair job: a narrow, dry-run-first set of destructive
    /// palace-consistency fixes (see `memcastle::repair`'s module doc for
    /// exactly what it does).
    Repair(RepairArgs),
    /// Read or write diary entries scoped to an agent identity.
    #[command(subcommand)]
    Diary(DiaryCommand),
    /// Inspect and control jobs. `jobs` is an alias.
    #[command(subcommand, alias = "jobs")]
    Job(JobCommand),
    /// List, show, create and delete wings: the top-level buckets of the
    /// palace, typically one per project. `wings` is an alias.
    #[command(subcommand, alias = "wings")]
    Wing(WingCommand),
    /// List, show, create and delete rooms, addressed as `<wing>/<room>`.
    /// `rooms` is an alias.
    #[command(subcommand, alias = "rooms")]
    Room(RoomCommand),
    /// List, show, create and delete drawers, addressed as
    /// `<wing>/<room>/<drawer>`, where a drawer is a name or a UUID. `drawers`
    /// is an alias.
    #[command(subcommand, alias = "drawers")]
    Drawer(DrawerCommand),
    /// Inspect graph assertions and their linked evidence by fact UUID.
    #[command(subcommand)]
    Fact(FactCommand),
    /// Manage the daemon's authentication token. Administrative, and never
    /// available to MCP clients.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Open or close the daemon's database admin endpoint, so SurrealDB Studio
    /// can inspect the live embedded database. A development and
    /// administration tool, off unless asked for, and never available to MCP
    /// clients.
    #[command(subcommand)]
    Db(DbCommand),
    /// Print a shell completion script to standard output, for `bash`, `zsh`,
    /// `fish`, `powershell` or `elvish`. Needs neither a daemon nor a
    /// configuration file.
    Completions(CompletionsArgs),
    /// Maintenance operations (stale-data sweep, ...). Not yet implemented.
    Maintenance,
}

/// Read-only inspection of fact lifecycle decisions.
#[derive(Debug, Subcommand)]
pub enum FactCommand {
    /// Show the assertion and linked confirmations, conflicts and corrections.
    History {
        /// UUID of the assertion to inspect.
        relationship_id: String,
        /// Evaluate lifecycle at this date or RFC 3339 instant.
        #[arg(long)]
        as_of: Option<String>,
    },
}

/// `memcastle daemon` subcommands: the daemon as a background process.
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start a daemon in the background and wait until it is serving. Fails
    /// if one is already running for this palace (see `daemon restart`). Its
    /// log output is discarded: run `serve` in the foreground, or under a
    /// process manager, when you need it. `--config`, `--bind`, `--port`,
    /// `--assets-dir` and the resolved `--palace` are passed on to the new
    /// daemon.
    Start(ServeArgs),
    /// Ask a running daemon to shut down gracefully.
    Stop,
    /// Stop the daemon, then start a fresh one and wait until it is serving
    /// (best-effort; for supervised deployments, prefer restarting through
    /// your process manager). Starts one when none is running. The flags are
    /// the same as `daemon start`.
    Restart(ServeArgs),
}

/// Arguments for `memcastle completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// The shell to generate completions for.
    pub shell: clap_complete::Shell,
}

/// `memcastle integration` subcommands: the lifecycle of an agent integration.
#[derive(Debug, Subcommand)]
pub enum IntegrationCommand {
    /// List the integrations this installation ships, with their versions and
    /// whether each one is installed, outdated or cannot be installed here.
    List(IntegrationListArgs),
    /// Install an integration: copy it, register it with its agent, and check
    /// the result. Running it again changes nothing.
    Install(IntegrationAgentArgs),
    /// Bring an installed integration up to the version this installation
    /// ships. An integration that was never installed is not installed by
    /// `update`.
    Update(IntegrationAgentArgs),
    /// Forget an integration in its agent and delete its installed copy.
    /// Everything else in the agent's configuration is left as it was.
    Remove(IntegrationRemoveArgs),
}

/// The flags every `memcastle integration` subcommand takes.
#[derive(Debug, Args)]
pub struct IntegrationCommonArgs {
    /// Directory holding `integrations/` and `skills/`, overriding
    /// `assets.dir` and `MEMCASTLE_ASSETS_DIR`. Point it at a checkout of
    /// MemCastle to install the integrations as built there, or at an unpacked
    /// package. Must be an absolute path to an existing directory; it is only
    /// read, never written.
    #[arg(long, value_name = "DIR")]
    pub assets_dir: Option<PathBuf>,
}

/// Arguments for `memcastle integration list`.
#[derive(Debug, Args)]
pub struct IntegrationListArgs {
    #[command(flatten)]
    pub common: IntegrationCommonArgs,
}

/// An integration named on the command line.
#[derive(Debug, Args)]
pub struct IntegrationAgentArgs {
    /// The integration, which is named for its agent (`pi`, `opencode`), as
    /// `memcastle integration list` shows it.
    pub agent: String,
    #[command(flatten)]
    pub common: IntegrationCommonArgs,
}

/// Arguments for `memcastle integration remove`.
///
/// It has no `--assets-dir`: removing reads no assets (it must work after the package that shipped the integration is
/// gone), so a flag that chose them would be accepted and silently do nothing.
#[derive(Debug, Args)]
pub struct IntegrationRemoveArgs {
    /// The integration, which is named for its agent (`pi`, `opencode`), as
    /// `memcastle integration list` shows it.
    pub agent: String,
}

/// `memcastle source` subcommands: the lifecycle of a mining source, from a new project to an installed package.
#[derive(Debug, Subcommand)]
pub enum SourceCommand {
    /// Create a new source project that builds, passes the conformance cases
    /// and can be packaged straight away. Local: needs no daemon.
    Init(SourceInitArgs),
    /// Build the project in the current directory (or `PATH`) into a
    /// WebAssembly component, with the command its `memcastle-source.toml`
    /// names under `[build]`. Local: needs no daemon.
    Build(SourceBuildArgs),
    /// Build the project, then run its conformance cases against the
    /// component in the same sandbox the daemon uses. Local: needs no daemon.
    Test(SourceTestArgs),
    /// Build the project, then write the distributable package: a tar.gz
    /// holding the manifest and the component. Local: needs no daemon.
    Package(SourcePackageArgs),
    /// Write a registry index (`memcastle-index.json`) that offers the given
    /// packages, adding to the one already there. Local: needs no daemon.
    Index(SourceIndexArgs),
    /// Generate an ed25519 key for signing packages in a registry index, and
    /// print the public key users put under `mining.trusted_keys`. Local:
    /// needs no daemon.
    Keygen(SourceKeygenArgs),
    /// Search the configured registries (the official one by default).
    Search(SourceSearchArgs),
    /// Install a source into the running daemon: a package file, a project
    /// directory, or a name from a registry. A source that asks
    /// for permissions is installed only after you agree to exactly those.
    Install(SourceInstallArgs),
    /// Update installed sources to the newest version their registry offers.
    /// A version that asks for more permissions than the installed one needs
    /// your agreement again.
    Update(SourceUpdateArgs),
    /// List the sources the daemon can mine, built in and installed, with
    /// their state and the permissions each was given.
    List,
    /// Show one source: its capabilities, state and permissions, and whether
    /// it is signed in when it signs in with OAuth.
    Show(SourceNameArgs),
    /// Sign an installed source in with OAuth, then keep it signed in: the
    /// daemon stores the tokens and renews them when a run needs them. Shows
    /// a code to type or opens your browser, and waits for you to finish.
    /// Run it again to sign in as someone else. Not `memcastle auth`, which
    /// manages the daemon's own token.
    Auth(SourceNameArgs),
    /// Allow an installed source to be mined.
    Enable(SourceNameArgs),
    /// Stop an installed source from being mined, keeping it installed.
    Disable(SourceNameArgs),
    /// Remove an installed source and its files. What it mined stays in the
    /// palace.
    Remove(SourceRemoveArgs),
}

/// Arguments for `memcastle source init`.
#[derive(Debug, Args)]
pub struct SourceInitArgs {
    /// The source's name: lowercase letters, digits and `-`. It becomes the
    /// directory created, and the name given to `memcastle mine` when mining.
    pub name: String,
    /// The language and toolchain to start from.
    #[arg(long, value_enum, default_value = "rust")]
    pub template: memcastle::source::scaffold::Template,
    /// Create the project here instead of in the current directory.
    #[arg(long, value_name = "DIR")]
    pub parent: Option<PathBuf>,
}

/// Arguments for `memcastle source build`.
#[derive(Debug, Args)]
pub struct SourceBuildArgs {
    /// The project's directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
}

/// Arguments for `memcastle source test`.
#[derive(Debug, Args)]
pub struct SourceTestArgs {
    /// The project's directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Test the component already built instead of building it first.
    #[arg(long)]
    pub no_build: bool,
}

/// Arguments for `memcastle source package`.
#[derive(Debug, Args)]
pub struct SourcePackageArgs {
    /// The project's directory.
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Package the component already built instead of building it first.
    #[arg(long)]
    pub no_build: bool,
    /// Write the package here instead of `dist/<name>-<version>.tar.gz`.
    #[arg(long, short, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

/// Arguments for `memcastle source index`.
#[derive(Debug, Args)]
pub struct SourceIndexArgs {
    /// The package archives to offer, as `memcastle source package` writes them.
    #[arg(required = true)]
    pub archives: Vec<PathBuf>,
    /// The index to write, extended when it already exists.
    #[arg(
        long,
        short,
        value_name = "FILE",
        default_value = "memcastle-index.json"
    )]
    pub output: PathBuf,
    /// Where the archives will be published. Each package's URL is this plus
    /// its file name; without it the URL is the file name, so the archives
    /// must sit beside the index.
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,
    /// Sign each archive with this key, as `memcastle source keygen` wrote it.
    #[arg(long, value_name = "KEYFILE")]
    pub sign: Option<PathBuf>,
    /// What the registry calls itself, for a new index.
    #[arg(long)]
    pub name: Option<String>,
}

/// Arguments for `memcastle source keygen`.
#[derive(Debug, Args)]
pub struct SourceKeygenArgs {
    /// Where to write the private key. Never overwrites a file.
    pub file: PathBuf,
}

/// Arguments for `memcastle source search`.
#[derive(Debug, Args)]
pub struct SourceSearchArgs {
    /// Part of a name or description. Without it, everything on offer.
    pub query: Option<String>,
    /// Search only this registry (a URL or an absolute path) instead of the
    /// configured registries.
    #[arg(long, value_name = "LOCATION")]
    pub registry: Option<String>,
}

/// Arguments for `memcastle source install`.
#[derive(Debug, Args)]
pub struct SourceInstallArgs {
    /// What to install: a package file as written by `memcastle source
    /// package`, a source project directory (it is built and packaged
    /// first), or a name, optionally pinned as `name@1.2.0`, from a
    /// registry. The sources that ship with MemCastle are installed already:
    /// enable them with `memcastle source enable`. Write `./name` for a path
    /// that looks like a name.
    pub source: String,
    /// Install a name from this registry (a URL or an absolute path) only,
    /// instead of the configured registries.
    #[arg(long, value_name = "LOCATION")]
    pub registry: Option<String>,
    /// Enable the source once it is installed.
    #[arg(long)]
    pub enable: bool,
    /// Agree to the permissions the package asks for without being asked.
    /// Without this (or `--consent`) a script is refused rather than
    /// consenting on your behalf.
    #[arg(short, long)]
    pub yes: bool,
    /// The consent digest shown for this package's permissions, for an
    /// unattended install that reviewed them beforehand.
    #[arg(long, value_name = "DIGEST", conflicts_with = "yes")]
    pub consent: Option<String>,
}

/// Arguments for `memcastle source update`.
#[derive(Debug, Args)]
pub struct SourceUpdateArgs {
    /// The source to update. Without it, every installed source that has an
    /// update.
    pub name: Option<String>,
    /// Only report what has an update; change nothing.
    #[arg(long, conflicts_with_all = ["yes"])]
    pub check: bool,
    /// Agree to the new permissions of an update that asks for more, without
    /// being asked. Without this a script is refused rather than consenting on
    /// your behalf.
    #[arg(short, long)]
    pub yes: bool,
}

/// `memcastle miner ...`: the configured miners.
#[derive(Debug, Subcommand)]
pub enum MinerCommand {
    /// List the configured miners and whether each can run.
    List,
    /// Show one miner in full.
    Get(MinerNameArgs),
    /// Create a miner, or change the settings named: everything not named is
    /// left as it is. Written to the configuration file, comments kept.
    Set(Box<MinerSetArgs>),
    /// Switch a miner on. It is checked first: its source must be usable and
    /// its credential must resolve.
    Enable(MinerNameArgs),
    /// Switch a miner off. What it mined, and its cursor, stay.
    Disable(MinerNameArgs),
    /// Remove a miner's definition. What it mined, and its cursor, stay.
    Remove(MinerRemoveArgs),
    /// Read the configuration file again now, and say what changed. The daemon
    /// also notices an edited file by itself on the next request.
    Reload,
    /// Submit the mining job for a miner, from where its source left off.
    Run(MinerRunArgs),
}

/// `memcastle trigger ...`: what asks for a mining run without being told to.
///
/// A trigger only decides *when*: it asks for the same run `memcastle miner run` does. Nothing here starts on its own:
/// a trigger is created disabled, and enabling it checks everything it needs first.
#[derive(Debug, Subcommand)]
pub enum TriggerCommand {
    /// List the configured triggers, whether each is working, and the webhook
    /// listener's state.
    List,
    /// Show one trigger in full: its settings, what it still needs, when it
    /// last fired and what last went wrong.
    Get(TriggerNameArgs),
    /// Create a trigger (disabled), or change the settings named: everything
    /// not named is left as it is. Written to the configuration file, comments
    /// kept.
    Set(Box<TriggerSetArgs>),
    /// Switch a trigger on. Every prerequisite is checked first, and what is
    /// missing is said; nothing starts until they all hold.
    Enable(TriggerNameArgs),
    /// Switch a trigger off. What it mined stays.
    Disable(TriggerNameArgs),
    /// Remove a trigger's definition and what the daemon remembers about it.
    Remove(TriggerRemoveArgs),
    /// Read the configuration file again now, and say what changed. The daemon
    /// also notices an edited file by itself within a few seconds.
    Reload,
    /// Ask for a run through the trigger now, the way it would on its own.
    Fire(TriggerNameArgs),
}

/// A trigger named on the command line.
#[derive(Debug, Args)]
pub struct TriggerNameArgs {
    /// The trigger's name, as `memcastle trigger list` shows it.
    pub name: String,
}

/// Arguments for `memcastle trigger remove`.
#[derive(Debug, Args)]
pub struct TriggerRemoveArgs {
    /// The trigger's name, as `memcastle trigger list` shows it.
    pub name: String,
    #[command(flatten)]
    pub confirm: ConfirmArgs,
}

/// Arguments for `memcastle trigger set`.
#[derive(Debug, Args)]
pub struct TriggerSetArgs {
    /// The trigger's name: lowercase letters, digits, `-` and `_`.
    pub name: String,
    /// The miner it asks to run (`memcastle miner list`). Needed to create a
    /// trigger.
    #[arg(long)]
    pub miner: Option<String>,
    /// How it decides to: `schedule` (`every`, optionally `at`), `poll`
    /// (`every`), `webhook` (needs a secret) or `watch` (`path`). Needed to
    /// create a trigger.
    #[arg(long = "type", value_parser = clap::builder::PossibleValuesParser::new(["schedule", "poll", "webhook", "watch"]))]
    pub kind: Option<String>,
    /// Read a webhook's shared secret from this environment variable of the
    /// daemon. The secret itself never goes in the configuration file.
    #[arg(long, value_name = "NAME", conflicts_with = "credential_file")]
    pub credential_env: Option<String>,
    /// Read a webhook's shared secret from this file.
    #[arg(long, value_name = "PATH")]
    pub credential_file: Option<String>,
    /// A setting of the trigger: `KEY=VALUE`, JSON when it parses as JSON
    /// (`every=1d`, `at=03:30`, `path=/notes`, `debounce=2s`). Repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub setting: Vec<String>,
    /// Remove a setting.
    #[arg(long, value_name = "KEY")]
    pub unset_setting: Vec<String>,
    /// Clear a field: `credential`.
    #[arg(long, value_name = "FIELD", value_parser = clap::builder::PossibleValuesParser::new(["credential"]))]
    pub unset: Vec<String>,
    /// Also switch it on, in the same step. Without this a trigger is saved
    /// disabled.
    #[arg(long)]
    pub enable: bool,
}

/// A miner named on the command line.
#[derive(Debug, Args)]
pub struct MinerNameArgs {
    /// The miner's name, as `memcastle miner list` shows it.
    pub name: String,
}

/// Arguments for `memcastle miner remove`.
#[derive(Debug, Args)]
pub struct MinerRemoveArgs {
    /// The miner's name, as `memcastle miner list` shows it.
    pub name: String,
    #[command(flatten)]
    pub confirm: ConfirmArgs,
}

/// Arguments for `memcastle miner run`.
#[derive(Debug, Args)]
pub struct MinerRunArgs {
    /// The miner's name, as `memcastle miner list` shows it.
    pub name: String,
    /// Read the source again from the beginning instead of from where the
    /// last run stopped. Unchanged documents are still skipped.
    #[arg(long)]
    pub full: bool,
    /// Replace or add source options for this run only (`KEY=VALUE`).
    #[arg(value_name = "KEY=VALUE")]
    pub options: Vec<String>,
    /// Acknowledge an override that may read more material than the saved miner.
    #[arg(long)]
    pub allow_broaden: bool,
}

/// Arguments for `memcastle miner set`.
#[derive(Debug, Args)]
pub struct MinerSetArgs {
    /// The miner's name: lowercase letters, digits, `-` and `_`.
    pub name: String,
    /// The source adapter to run (`memcastle sources` lists them). Needed to
    /// create a miner.
    #[arg(long)]
    pub source: Option<String>,
    /// The part of the source to read: an absolute path for `directory`, a
    /// channel or repository for others.
    #[arg(long)]
    pub locator: Option<String>,
    /// The wing the mined drawers go to, when it should not be the source's default.
    #[arg(long)]
    pub wing: Option<String>,
    /// Read the source's credential from this environment variable of the
    /// daemon. The secret itself never goes in the configuration file.
    #[arg(long, value_name = "NAME", conflicts_with = "credential_file")]
    pub credential_env: Option<String>,
    /// Read the source's credential from the first line of this file.
    #[arg(long, value_name = "PATH", conflicts_with = "credential_oauth")]
    pub credential_file: Option<String>,
    /// Use the source's OAuth sign-in (`memcastle source auth <SOURCE>`),
    /// which the daemon keeps and renews. Only for a source that signs in
    /// with OAuth.
    #[arg(long, conflicts_with_all = ["credential_env", "credential_file"])]
    pub credential_oauth: bool,
    /// A saved source option: `KEY=VALUE`, JSON when it parses as JSON. Repeatable.
    #[arg(long, value_name = "KEY=VALUE")]
    pub option: Vec<String>,
    /// Remove a saved source option.
    #[arg(long, value_name = "KEY")]
    pub unset_option: Vec<String>,
    /// Clear a field: one of `locator`, `wing`, `credential`.
    #[arg(long, value_name = "FIELD", value_parser = clap::builder::PossibleValuesParser::new(["locator", "wing", "credential"]))]
    pub unset: Vec<String>,
    /// Create the miner switched off (or switch it off).
    #[arg(long)]
    pub disabled: bool,
    /// Allow the change to widen the scope. Without it a change that removes
    /// a filter or adds a value to one is refused.
    #[arg(long)]
    pub allow_broaden: bool,
}

/// A source named on the command line.
#[derive(Debug, Args)]
pub struct SourceNameArgs {
    /// The source's name, as `memcastle source list` shows it.
    pub name: String,
}

/// Arguments for `memcastle source remove`.
#[derive(Debug, Args)]
pub struct SourceRemoveArgs {
    /// The source's name, as `memcastle source list` shows it.
    pub name: String,
    #[command(flatten)]
    pub confirm: ConfirmArgs,
}

/// The `--yes` flag shared by every command that asks before it acts.
#[derive(Debug, Args)]
pub struct ConfirmArgs {
    /// Do not ask for confirmation. Only needed in a terminal: without one
    /// (a script, CI, a pipe) these commands never ask.
    #[arg(short, long)]
    pub yes: bool,
}

/// Arguments for `memcastle serve`, `memcastle daemon start` and
/// `memcastle daemon restart`: the last two only forward them to `serve`.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Interface address to listen on, overriding `server.bind` and
    /// `MEMCASTLE_BIND` (default `127.0.0.1`). An IP address alone: the port
    /// is `--port`. Anything but a loopback address exposes the daemon to the
    /// network: enable authentication (`auth.enabled`) when you do.
    #[arg(long, value_name = "IP", value_parser = memcastle::config::parse_bind_host)]
    pub bind: Option<IpAddr>,
    /// TCP port to listen on, overriding `server.port` and `MEMCASTLE_PORT`
    /// (default 8420). `0` lets the OS pick a free one.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// Directory of runtime assets, overriding `assets.dir` and
    /// `MEMCASTLE_ASSETS_DIR`. It outranks the assets a package installed and
    /// the ones built into the binary, so a local web build can be served
    /// without installing it. Must be an absolute path to an existing
    /// directory; it is only read, never written.
    #[arg(long, value_name = "DIR")]
    pub assets_dir: Option<PathBuf>,
}

/// Arguments for `memcastle migrate`.
#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// Report the current/pending version without applying anything, then
    /// exit with an error if any migration is pending — for CI/ops
    /// scripts that just want to know whether a migration is needed.
    #[arg(long, conflicts_with = "status")]
    pub check: bool,
    /// Report the current/pending version without applying anything.
    #[arg(long, conflicts_with = "check")]
    pub status: bool,
}

/// The retrieval options `search` and `recall` share.
#[derive(Debug, Args)]
pub struct RetrievalArgs {
    /// How to rank: `auto` (hybrid when the daemon can embed the query, else
    /// lexical), `lexical`, `semantic` or `hybrid`.
    #[arg(long, value_parser = PossibleValuesParser::new(memcastle::search::RankingMode::NAMES))]
    pub ranking: Option<String>,
    /// Only drawers carrying this tag; repeat the flag to require several.
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Only drawers from this kind of source.
    #[arg(long, value_parser = PossibleValuesParser::new(["file", "manual", "transcript", "note", "other"]))]
    pub source_kind: Option<String>,
    /// Search the memory that was valid at this instant instead of now: an
    /// RFC 3339 timestamp (2026-01-31T12:00:00Z) or a date (2026-01-31, midnight UTC).
    #[arg(
        long,
        value_name = "WHEN",
        conflicts_with_all = ["include_historical", "from", "until"]
    )]
    pub as_of: Option<String>,
    /// Search the memory that was valid at some moment of an interval, from this
    /// instant (inclusive, same forms as `--as-of`). Needs `--until`.
    #[arg(
        long,
        value_name = "WHEN",
        requires = "until",
        conflicts_with = "include_historical"
    )]
    pub from: Option<String>,
    /// The end of the `--from` interval (exclusive).
    /// `--from 2026-01-01 --until 2026-02-01` is exactly January.
    #[arg(
        long,
        value_name = "WHEN",
        requires = "from",
        conflicts_with = "include_historical"
    )]
    pub until: Option<String>,
    /// Also return memory that has since been superseded.
    #[arg(long)]
    pub include_historical: bool,
    /// Also surface drawers related to the hits through the knowledge graph.
    #[arg(long)]
    pub expand: bool,
}

/// Arguments for `memcastle search`.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// The search query.
    pub query: String,
    /// Maximum number of results.
    #[arg(long, default_value_t = memcastle::app::DEFAULT_SEARCH_LIMIT)]
    pub limit: u32,
    /// Restrict results to drawers filed (transitively) under this wing.
    #[arg(long)]
    pub wing: Option<String>,
    /// Restrict results to drawers filed directly under this room.
    #[arg(long)]
    pub room: Option<String>,
    #[command(flatten)]
    pub retrieval: RetrievalArgs,
}

/// Arguments for `memcastle recall`.
#[derive(Debug, Args)]
pub struct RecallArgs {
    /// The recall query.
    pub query: String,
    /// Maximum number of results.
    #[arg(long, default_value_t = memcastle::app::DEFAULT_SEARCH_LIMIT)]
    pub limit: u32,
    /// Restrict results to drawers filed (transitively) under this wing.
    #[arg(long)]
    pub wing: Option<String>,
    #[command(flatten)]
    pub retrieval: RetrievalArgs,
}

/// Arguments for `memcastle wake-up`.
#[derive(Debug, Args)]
pub struct WakeUpArgs {
    /// The identity to build session-start context for.
    #[arg(long)]
    pub agent_identity: String,
    /// Restrict the diary lookup and recent highlights to this wing.
    /// Omitting this skips the diary lookup entirely (see
    /// `AppServices::wake_up`'s doc comment) but still returns unscoped
    /// recent highlights.
    #[arg(long)]
    pub wing: Option<String>,
    /// Maximum number of recent-highlight drawers to include. Defaults to
    /// `WakeUpBudget::default()`'s value when omitted.
    #[arg(long)]
    pub max_items: Option<usize>,
    /// Maximum total content bytes across recent highlights. Defaults to
    /// `WakeUpBudget::default()`'s value when omitted.
    #[arg(long)]
    pub max_bytes: Option<usize>,
}

/// Arguments for `memcastle mine`.
#[derive(Debug, Args)]
#[command(
    override_usage = "memcastle mine <SOURCE> [PLACE] [KEY=VALUE]... [--wing <WING>] [--full]\n       \
                      memcastle mine <PATH> [KEY=VALUE]... [--wing <WING>] [--full]",
    after_help = "Examples:\n  \
        memcastle mine directory /some/path\n  \
        memcastle mine /some/path                       (shorthand for `directory`)\n  \
        memcastle mine opencode since=2026-09 dir=/path/to/workspace\n  \
        memcastle mine pi /backups/pi/sessions --full\n\n\
        `memcastle sources` lists the sources and the options each accepts."
)]
pub struct MineArgs {
    /// A source to mine (`directory`, `pi`, `opencode`, or an installed one;
    /// `memcastle sources` lists them), or a directory, which is shorthand for
    /// `directory <PATH>`. A word with a `/`, or one that is not shaped like a
    /// source name (`.`, `~/x`), is always a directory.
    #[arg(value_name = "SOURCE|PATH")]
    pub target: String,
    /// What to read and how, in the source's terms. At most one bare word,
    /// the place to read (`directory`'s path, `pi`'s sessions directory), then
    /// any number of `key=value` options. Each source declares the keys it
    /// accepts (`since=2026-09` narrows by date); an unknown key is refused.
    #[arg(value_name = "PLACE|KEY=VALUE")]
    pub args: Vec<String>,
    /// Read the source again from the beginning instead of continuing from
    /// where the last run stopped. Unchanged documents are still skipped, so
    /// nothing is duplicated.
    #[arg(long)]
    pub full: bool,
    /// The wing to file mined drawers under. Defaults to the directory name,
    /// or to the source's own default.
    #[arg(long)]
    pub wing: Option<String>,
}

/// Arguments for `memcastle note`.
#[derive(Debug, Args)]
pub struct NoteArgs {
    /// The note. Several words are joined with spaces, so quoting is
    /// optional; start with `--` to write text that begins with a dash.
    #[arg(value_name = "TEXT", conflicts_with = "file")]
    pub text: Vec<String>,
    /// Read the note from this file; `-` reads standard input.
    #[arg(long, short, value_name = "PATH")]
    pub file: Option<PathBuf>,
    /// Write the note in `$VISUAL` (else `$EDITOR`), starting from TEXT when
    /// it is given. The default when no text is given on a terminal.
    #[arg(long, short, conflicts_with = "file")]
    pub edit: bool,
    /// File the note under this wing instead of the project's
    /// (`MEMCASTLE_WING`, `.config/memcastle.toml`, else the directory's name).
    #[arg(long)]
    pub wing: Option<String>,
    /// File the note in this room instead of the project's
    /// (`MEMCASTLE_ROOM`, `.config/memcastle.toml`, else `notes`).
    #[arg(long)]
    pub room: Option<String>,
}

/// Arguments for `memcastle checkpoint`.
#[derive(Debug, Args)]
pub struct CheckpointArgs {
    /// Path to a JSON file holding the checkpoint payload
    /// (`{"items": [...]}`, matching `domain::CheckpointPayload`). Reads
    /// from stdin if omitted.
    #[arg(long)]
    pub payload: Option<PathBuf>,
    /// Escalate to `Priority::Critical`, preempting all other queued work —
    /// reserved for save-before-crash situations, not routine checkpoints.
    #[arg(long)]
    pub emergency: bool,
}

/// Arguments for `memcastle audit`.
#[derive(Debug, Args)]
pub struct AuditArgs {
    /// Restrict the report's embedding-count fields to one wing by name.
    /// Orphan-drawer and dangling-provenance findings are always
    /// palace-wide regardless of this (see `memcastle::audit`'s module doc).
    #[arg(long)]
    pub wing: Option<String>,
}

/// Arguments for `memcastle embed`.
#[derive(Debug, Args)]
pub struct EmbedArgs {
    /// Only embed the drawers of this wing, by name.
    #[arg(long)]
    pub wing: Option<String>,
}

/// Arguments for `memcastle extract`.
#[derive(Debug, Args)]
pub struct ExtractArgs {
    /// Only read the drawers of this wing, by name.
    #[arg(long)]
    pub wing: Option<String>,
}

/// Arguments for `memcastle repair`.
#[derive(Debug, Args)]
pub struct RepairArgs {
    /// Actually perform the planned actions. Without this flag, repair
    /// always runs in dry-run mode: it reports what it would do without
    /// mutating anything (see `memcastle::repair`'s module doc).
    /// In a terminal this asks for confirmation first (see `--yes`).
    #[arg(long)]
    pub apply: bool,
    /// Restrict repair actions to what a specific prior `memcastle audit`
    /// job (its job id) found, rather than scanning the whole palace fresh.
    #[arg(long)]
    pub based_on_job: Option<String>,
    /// Skip the confirmation `--apply` asks for in a terminal.
    #[arg(short, long)]
    pub yes: bool,
}

/// `memcastle diary` subcommands.
#[derive(Debug, Subcommand)]
pub enum DiaryCommand {
    /// Write a new diary entry.
    Write {
        /// The identity to scope this entry to — keep this consistent
        /// across writes/reads (see `AppServices::diary_write`'s doc
        /// comment).
        #[arg(long)]
        agent_identity: String,
        /// The wing to file this entry under, in its fixed `"diary"` room.
        #[arg(long)]
        wing: String,
        /// The entry's content.
        content: String,
    },
    /// Read back an identity's most recent diary entries in a wing.
    Read {
        /// The identity whose diary entries to read back.
        #[arg(long)]
        agent_identity: String,
        /// The wing to read this identity's entries from.
        #[arg(long)]
        wing: String,
        /// Maximum number of entries to return, newest first.
        #[arg(long, default_value_t = memcastle::app::DEFAULT_DIARY_LIMIT)]
        limit: u32,
    },
}

/// `memcastle wing` subcommands.
#[derive(Debug, Subcommand)]
pub enum WingCommand {
    /// List wings with their room and drawer counts. A table in a terminal,
    /// JSON when standard output is piped or redirected.
    List,
    /// Show one wing: its totals and its rooms.
    Show {
        /// The wing's name or UUID.
        wing: String,
    },
    /// Create a wing. Creating one that already exists succeeds and changes
    /// nothing.
    Create {
        /// The new wing's name. It cannot contain `/` or look like a UUID.
        wing: String,
        /// A free-text description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a wing and every room and drawer in it, permanently. In a
    /// terminal this shows what will be removed and asks first. Refused while
    /// a mining, checkpoint or applying repair job is pending.
    Delete {
        /// The wing's name or UUID.
        wing: String,
        /// Skip the confirmation asked for in a terminal.
        #[arg(short, long)]
        yes: bool,
    },
}

/// `memcastle room` subcommands.
#[derive(Debug, Subcommand)]
pub enum RoomCommand {
    /// List rooms with their drawer counts, in one wing or (without `--wing`)
    /// in every wing. A table in a terminal, JSON when standard output is
    /// piped or redirected.
    List {
        /// Only this wing's rooms (name or UUID).
        #[arg(long)]
        wing: Option<String>,
    },
    /// Show one room.
    Show {
        /// The room, as `<wing>/<room>`.
        room: String,
    },
    /// Create a room, and its wing if that does not exist yet. Creating a room
    /// that already exists succeeds and changes nothing.
    Create {
        /// The new room, as `<wing>/<room>`.
        room: String,
        /// A free-text description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Delete a room and every drawer in it, permanently. In a terminal this
    /// shows what will be removed and asks first. Refused while a mining,
    /// checkpoint or applying repair job is pending.
    Delete {
        /// The room, as `<wing>/<room>`.
        room: String,
        /// Skip the confirmation asked for in a terminal.
        #[arg(short, long)]
        yes: bool,
    },
}

/// `memcastle drawer` subcommands.
#[derive(Debug, Subcommand)]
pub enum DrawerCommand {
    /// List a room's drawers, newest first, with a preview of each. A table in
    /// a terminal, JSON when standard output is piped or redirected.
    List {
        /// The room, as `<wing>/<room>`.
        #[arg(long)]
        room: String,
        /// Maximum number of drawers to list.
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show one drawer in full.
    Show {
        /// The drawer, as `<wing>/<room>/<name or UUID>`.
        drawer: String,
    },
    /// Write a named drawer, creating its room and wing if they do not exist.
    /// The content is immutable: writing the same name again with the same
    /// content succeeds and changes nothing, with other content it is refused.
    /// The content comes from `--content`, from `--file`, or from standard
    /// input.
    Create {
        /// The drawer, as `<wing>/<room>/<name>`. The name may contain `/`.
        drawer: String,
        /// The content, as an argument.
        #[arg(long, conflicts_with = "file")]
        content: Option<String>,
        /// Read the content from this file; `-` reads standard input.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
    },
    /// Correct a drawer without rewriting history: end its validity now and
    /// open a replacement with the new content (from `--content`, `--file`
    /// or standard input), or only end it with `--invalidate`. The old drawer
    /// stays, content untouched, for point-in-time searches (`--as-of`).
    Supersede {
        /// The drawer to supersede, as `<wing>/<room>/<name or UUID>`.
        drawer: String,
        /// The replacement content, as an argument.
        #[arg(long, conflicts_with_all = ["file", "invalidate"])]
        content: Option<String>,
        /// Read the replacement content from this file; `-` reads standard input.
        #[arg(long, value_name = "PATH", conflicts_with = "invalidate")]
        file: Option<PathBuf>,
        /// End the drawer's validity without a replacement.
        #[arg(long)]
        invalidate: bool,
    },
    /// Show how a piece of knowledge evolved: every version of the drawer's
    /// supersession chain, oldest first, each with its validity period,
    /// provenance and content. Works from any version of the chain. A table
    /// in a terminal, JSON when standard output is piped or redirected.
    History {
        /// Any version of the chain, as `<wing>/<room>/<name or UUID>`. A
        /// superseded version has no name any more: use its UUID.
        drawer: String,
    },
    /// Record that a drawer mentions an entity, creating the entity if
    /// needed, so `search --expand` can reach related drawers through it.
    Mention {
        /// The drawer, as `<wing>/<room>/<name or UUID>`.
        drawer: String,
        /// The entity's name.
        #[arg(long)]
        name: String,
        /// The entity's kind (`person`, `project`, ...).
        #[arg(long)]
        kind: String,
    },
    /// Delete one drawer, permanently. In a terminal this asks first.
    Delete {
        /// The drawer, as `<wing>/<room>/<name or UUID>`.
        drawer: String,
        /// Skip the confirmation asked for in a terminal.
        #[arg(short, long)]
        yes: bool,
    },
}

/// `memcastle auth` subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Generate a high-entropy token and print it to standard output, once.
    /// The daemon keeps only a digest, so the token cannot be shown again:
    /// store it in a secret manager, then set `auth.enabled` and restart.
    /// Generating again replaces the previous token, which is rotation.
    /// While authentication is enabled this needs a valid token
    /// (MEMCASTLE_AUTH_TOKEN) like every other command.
    /// In a terminal this asks for confirmation first.
    Generate(ConfirmArgs),
    /// Revoke the generated token, so it stops working immediately. A shared
    /// secret set through MEMCASTLE_AUTH_TOKEN or `auth.token` is not
    /// affected: change the configuration and restart to revoke that one.
    /// In a terminal this asks for confirmation first.
    Revoke(ConfirmArgs),
}

/// `memcastle db` subcommands.
#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Ask the running daemon to expose its own embedded database to
    /// SurrealDB Studio, then return. The endpoint lives in the daemon (the
    /// only process that may open the database), so this never starts a
    /// second database process: stop it again with `memcastle db stop`.
    /// Running it while the endpoint is already open just reports where it
    /// listens.
    Start(DbStartArgs),
    /// Close the database admin endpoint.
    Stop,
    /// Report whether the database admin endpoint is open and where.
    Status,
}

/// Arguments for `memcastle db start`. Anything left out falls back to the
/// daemon's `[db]` configuration. Settings that contradict an endpoint that is
/// already open are refused: stop it first.
#[derive(Debug, Args)]
pub struct DbStartArgs {
    /// Interface address to listen on, overriding `db.bind` and
    /// `MEMCASTLE_DB_BIND` (default `127.0.0.1`). Anything but a loopback
    /// address also needs `--allow-remote` and an authenticated daemon.
    #[arg(long, value_name = "IP", value_parser = memcastle::config::parse_bind_host)]
    pub bind: Option<IpAddr>,
    /// TCP port to listen on, overriding `db.port` and `MEMCASTLE_DB_PORT`
    /// (default 8000). `0` lets the OS pick a free one.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
    /// Allow a non-loopback `--bind`. The endpoint is a console onto the whole
    /// palace database, so this is refused unless the daemon has
    /// authentication enabled (`auth.enabled`). The token crosses the network
    /// in cleartext: put it behind a TLS proxy or a tunnel.
    #[arg(long)]
    pub allow_remote: bool,
    /// A web page origin allowed to connect from a browser, such as
    /// `https://app.surrealdb.com` for the hosted Surrealist. Pages served
    /// from this machine are always allowed. Repeatable.
    #[arg(long = "allow-origin", value_name = "ORIGIN")]
    pub allow_origin: Vec<String>,
}

/// `memcastle job` subcommands.
#[derive(Debug, Subcommand)]
pub enum JobCommand {
    /// List jobs, optionally filtered by status. A table in a terminal, JSON
    /// when standard output is piped or redirected.
    List {
        /// Only jobs in this status.
        #[arg(long, value_parser = status_parser())]
        status: Option<String>,
    },
    /// Show one job's full detail, including progress and checkpoint.
    Show {
        /// The job id.
        id: String,
    },
    /// Request that a running job pause at its next checkpoint.
    Pause {
        /// The job id.
        id: String,
    },
    /// Resume a paused job.
    Resume {
        /// The job id.
        id: String,
    },
    /// Cancel a queued, paused, or running job. In a terminal this asks for
    /// confirmation first.
    Cancel {
        /// The job id.
        id: String,
        /// Skip the confirmation asked for in a terminal.
        #[arg(short, long)]
        yes: bool,
    },
    /// Retry a failed job.
    Retry {
        /// The job id.
        id: String,
    },
    /// Submit a synthetic demo job — exercises the scheduler (checkpointing,
    /// pause/resume, cancellation) without mining anything real. Handy for
    /// verifying the daemon end-to-end.
    Demo {
        /// How many steps to simulate.
        #[arg(long, default_value_t = 5)]
        steps: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    // Catches the derive mistakes that would otherwise only surface as a panic
    // the first time a user runs the binary.
    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    fn serve_args(args: &[&str]) -> Result<ServeArgs, clap::Error> {
        let cli = Cli::try_parse_from(std::iter::once("memcastle").chain(args.iter().copied()))?;
        match cli.command {
            Command::Serve(serve) => Ok(serve),
            other => panic!("expected serve, got {other:?}"),
        }
    }

    #[test]
    fn serve_accepts_the_bind_and_port_from_the_issue_example() {
        let serve = serve_args(&["serve", "--bind", "127.0.0.1", "--port", "8787"]).unwrap();
        assert_eq!(serve.bind, Some("127.0.0.1".parse().unwrap()));
        assert_eq!(serve.port, Some(8787));
    }

    fn daemon_command(args: &[&str]) -> Result<DaemonCommand, clap::Error> {
        let cli = Cli::try_parse_from(
            ["memcastle", "daemon"]
                .into_iter()
                .chain(args.iter().copied()),
        )?;
        match cli.command {
            Command::Daemon(daemon) => Ok(daemon),
            other => panic!("expected daemon, got {other:?}"),
        }
    }

    #[test]
    fn the_daemon_group_has_start_stop_and_restart() {
        assert!(matches!(
            daemon_command(&["start"]).unwrap(),
            DaemonCommand::Start(_)
        ));
        assert!(matches!(
            daemon_command(&["stop"]).unwrap(),
            DaemonCommand::Stop
        ));
        assert!(matches!(
            daemon_command(&["restart"]).unwrap(),
            DaemonCommand::Restart(_)
        ));
        // A bare `daemon` names no action, so it must not silently start one.
        assert!(Cli::try_parse_from(["memcastle", "daemon"]).is_err());
    }

    #[test]
    fn daemon_start_and_restart_take_the_listener_flags() {
        for word in ["start", "restart"] {
            let command = daemon_command(&[
                word,
                "--bind",
                "::1",
                "--port",
                "9000",
                "--assets-dir",
                "/srv/assets",
            ])
            .unwrap();
            let (DaemonCommand::Start(args) | DaemonCommand::Restart(args)) = command else {
                panic!("`daemon {word}` must carry the listener flags");
            };
            assert_eq!(args.bind, Some("::1".parse().unwrap()), "{word}");
            assert_eq!(args.port, Some(9000), "{word}");
            assert_eq!(
                args.assets_dir,
                Some(PathBuf::from("/srv/assets")),
                "{word}"
            );
        }
    }

    #[test]
    fn daemon_stop_takes_no_listener_flags() {
        assert!(daemon_command(&["stop", "--port", "9000"]).is_err());
    }

    #[test]
    fn the_old_top_level_stop_and_restart_are_gone() {
        for word in ["stop", "restart"] {
            assert!(
                Cli::try_parse_from(["memcastle", word]).is_err(),
                "`{word}` moved under `daemon`"
            );
        }
    }

    #[test]
    fn daemon_is_no_longer_an_alias_of_serve() {
        // `daemon --port` used to run the server in the foreground; it must
        // now be refused rather than mean something else.
        assert!(Cli::try_parse_from(["memcastle", "daemon", "--port", "9000"]).is_err());
    }

    #[test]
    fn serve_without_listener_flags_leaves_both_unset_so_the_lower_layers_apply() {
        let serve = serve_args(&["serve"]).unwrap();
        assert_eq!((serve.bind, serve.port), (None, None));
    }

    #[test]
    fn auth_generate_and_revoke_parse_and_take_no_token_argument() {
        for (word, expected) in [("generate", "Generate("), ("revoke", "Revoke(")] {
            let cli = Cli::try_parse_from(["memcastle", "auth", word]).unwrap();
            assert!(
                matches!(&cli.command, Command::Auth(auth) if format!("{auth:?}").starts_with(expected)),
                "{:?}",
                cli.command
            );
        }
        // The token must never be a command-line argument: it would land in
        // shell history and the process list.
        assert!(Cli::try_parse_from(["memcastle", "auth", "generate", "--token", "x"]).is_err());
        assert!(Cli::try_parse_from(["memcastle", "--token", "x", "status"]).is_err());
    }

    #[test]
    fn a_bind_that_still_carries_a_port_is_rejected_pointing_at_port() {
        let err = serve_args(&["serve", "--bind", "127.0.0.1:8420"]).unwrap_err();
        assert!(err.to_string().contains("--port"), "{err}");
    }

    #[test]
    fn an_out_of_range_or_non_numeric_port_is_rejected() {
        for port in ["70000", "-1", "http"] {
            assert!(
                serve_args(&["serve", "--port", port]).is_err(),
                "{port} must be rejected"
            );
        }
    }

    #[test]
    fn db_start_defaults_leave_everything_to_the_daemons_configuration() {
        let cli = Cli::try_parse_from(["memcastle", "db", "start"]).unwrap();
        let Command::Db(DbCommand::Start(args)) = cli.command else {
            panic!("a `db start` command");
        };
        assert_eq!(args.bind, None);
        assert_eq!(args.port, None);
        assert!(!args.allow_remote);
        assert!(args.allow_origin.is_empty());
    }

    #[test]
    fn db_start_accepts_a_bind_a_port_the_remote_opt_in_and_repeatable_origins() {
        let cli = Cli::try_parse_from([
            "memcastle",
            "db",
            "start",
            "--bind",
            "0.0.0.0",
            "--port",
            "9000",
            "--allow-remote",
            "--allow-origin",
            "https://a.example",
            "--allow-origin",
            "https://b.example",
        ])
        .unwrap();
        let Command::Db(DbCommand::Start(args)) = cli.command else {
            panic!("a `db start` command");
        };
        assert_eq!(args.bind, Some("0.0.0.0".parse().unwrap()));
        assert_eq!(args.port, Some(9000));
        assert!(args.allow_remote);
        assert_eq!(
            args.allow_origin,
            ["https://a.example", "https://b.example"]
        );
    }

    #[test]
    fn db_has_no_serve_subcommand_any_more() {
        // It was renamed to `start`: it starts the adapter, it does not serve
        // the database.
        assert!(Cli::try_parse_from(["memcastle", "db", "serve"]).is_err());
    }

    #[test]
    fn there_is_no_help_subcommand_at_the_top_or_in_any_group() {
        // `--help` is the one way to ask: `help` only duplicated it.
        assert!(Cli::try_parse_from(["memcastle", "help"]).is_err());
        for group in [
            "job", "diary", "auth", "db", "daemon", "wing", "room", "drawer", "source",
        ] {
            assert!(
                Cli::try_parse_from(["memcastle", group, "help"]).is_err(),
                "`{group} help` must not exist"
            );
        }
        let command = Cli::command();
        assert!(command.find_subcommand("help").is_none());
        assert!(
            command
                .get_subcommands()
                .all(|sub| sub.find_subcommand("help").is_none())
        );
    }

    #[test]
    fn help_flags_still_work() {
        for args in [
            vec!["memcastle", "--help"],
            vec!["memcastle", "job", "--help"],
        ] {
            let error = Cli::try_parse_from(&args).unwrap_err();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp,
                "{args:?}"
            );
        }
    }

    #[test]
    fn completions_take_a_known_shell_and_reject_others() {
        for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let cli = Cli::try_parse_from(["memcastle", "completions", shell]).unwrap();
            assert!(matches!(cli.command, Command::Completions(_)), "{shell}");
        }
        assert!(Cli::try_parse_from(["memcastle", "completions", "tcsh"]).is_err());
        assert!(Cli::try_parse_from(["memcastle", "completions"]).is_err());
    }

    #[test]
    fn the_json_flag_is_global_so_it_is_accepted_before_and_after_any_subcommand() {
        for args in [
            &["memcastle", "--json", "status"][..],
            &["memcastle", "status", "--json"],
            &["memcastle", "job", "list", "--json"],
            &["memcastle", "mine", ".", "--json"],
            &["memcastle", "db", "start", "--json"],
            &["memcastle", "integration", "remove", "pi", "--json"],
            &["memcastle", "integration", "list", "--json"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.json, "{args:?}");
        }
        assert!(!Cli::try_parse_from(["memcastle", "status"]).unwrap().json);
    }

    #[test]
    fn job_list_offers_the_valid_statuses_and_rejects_others() {
        for status in [
            "queued",
            "running",
            "paused",
            "completed",
            "failed",
            "cancelled",
        ] {
            Cli::try_parse_from(["memcastle", "job", "list", "--status", status]).unwrap();
        }
        let error = Cli::try_parse_from(["memcastle", "job", "list", "--status", "done"])
            .unwrap_err()
            .to_string();
        assert!(error.contains("completed"), "{error}");
    }

    #[test]
    fn mode_parses_to_a_memory_mode_and_rejects_unknown_names_listing_the_valid_ones() {
        let cli = Cli::try_parse_from(["memcastle", "--mode", "read_only", "status"]).unwrap();
        assert_eq!(cli.mode, Some(MemoryMode::ReadOnly));
        let error = Cli::try_parse_from(["memcastle", "--mode", "readonly", "status"])
            .unwrap_err()
            .to_string();
        assert!(error.contains("read_only"), "{error}");
    }

    #[test]
    fn every_command_that_asks_for_confirmation_accepts_yes_in_both_spellings() {
        for args in [
            vec!["repair", "--apply", "--yes"],
            vec!["repair", "--apply", "-y"],
            vec!["auth", "generate", "--yes"],
            vec!["auth", "revoke", "-y"],
            vec!["job", "cancel", "some-id", "--yes"],
            vec!["wing", "delete", "work", "--yes"],
            vec!["wing", "delete", "work", "-y"],
            vec!["room", "delete", "work/x", "--yes"],
            vec!["room", "delete", "work/x", "-y"],
            vec!["drawer", "delete", "work/x/y", "--yes"],
            vec!["drawer", "delete", "work/x/y", "-y"],
        ] {
            let all = std::iter::once("memcastle").chain(args.iter().copied());
            Cli::try_parse_from(all).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn serve_has_no_flag_that_starts_the_database_endpoint() {
        // The endpoint is opened only by an explicit `db start`, never as a side
        // effect of starting the daemon.
        for flag in ["--db", "--db-endpoint", "--allow-remote"] {
            assert!(
                Cli::try_parse_from(["memcastle", "serve", flag]).is_err(),
                "`serve {flag}` must not exist"
            );
        }
    }

    #[test]
    fn the_plural_spellings_are_aliases_of_the_singular_groups() {
        for (plural, singular) in [
            ("wings", "wing"),
            ("rooms", "room"),
            ("drawers", "drawer"),
            ("jobs", "job"),
        ] {
            let a = Cli::try_parse_from(["memcastle", plural, "list", "--help"]);
            let b = Cli::try_parse_from(["memcastle", singular, "list", "--help"]);
            assert_eq!(
                a.unwrap_err().kind(),
                b.unwrap_err().kind(),
                "`{plural}` must reach the same command as `{singular}`"
            );
        }
        let cli = Cli::try_parse_from(["memcastle", "wings", "show", "work"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Wing(WingCommand::Show { .. })
        ));
    }

    #[test]
    fn drawer_create_takes_content_from_an_argument_or_a_file_but_not_both() {
        Cli::try_parse_from(["memcastle", "drawer", "create", "w/r/n", "--content", "x"]).unwrap();
        Cli::try_parse_from(["memcastle", "drawer", "create", "w/r/n", "--file", "-"]).unwrap();
        assert!(
            Cli::try_parse_from([
                "memcastle",
                "drawer",
                "create",
                "w/r/n",
                "--content",
                "x",
                "--file",
                "f"
            ])
            .is_err()
        );
    }

    #[test]
    fn drawer_list_needs_a_room() {
        assert!(Cli::try_parse_from(["memcastle", "drawer", "list"]).is_err());
        Cli::try_parse_from(["memcastle", "drawer", "list", "--room", "w/r"]).unwrap();
    }
}
