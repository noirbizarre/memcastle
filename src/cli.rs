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
/// theme. clap only emits them on a terminal and honours `NO_COLOR`, so piped
/// help (and the tests that read it) stays plain.
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
    Status(StatusArgs),
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
    /// Submit a mining job for a directory.
    Mine(MineArgs),
    /// Submit a checkpoint job: persist an already-classified batch of
    /// memory writes.
    Checkpoint(CheckpointArgs),
    /// Submit an audit job: a read-only palace consistency report.
    Audit(AuditArgs),
    /// Submit an embedding job: compute the vector of every drawer that has
    /// none, so semantic search covers it. Needs an `[embeddings]` provider.
    Embed(EmbedArgs),
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
    /// Maintenance operations (dedup, stale-data sweep, ...). Not yet implemented.
    Maintenance,
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

/// The `--yes` flag shared by every command that asks before it acts.
#[derive(Debug, Args)]
pub struct ConfirmArgs {
    /// Do not ask for confirmation. Only needed in a terminal: without one
    /// (a script, CI, a pipe) these commands never ask.
    #[arg(short, long)]
    pub yes: bool,
}

/// Arguments for `memcastle status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print the report as JSON instead of text, for scripts.
    #[arg(long)]
    pub json: bool,
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
    #[arg(long, value_parser = PossibleValuesParser::new(["file", "manual", "other"]))]
    pub source_kind: Option<String>,
    /// Search the memory that was valid at this RFC 3339 instant
    /// (e.g. 2026-01-31T12:00:00Z) instead of now.
    #[arg(long, value_name = "TIMESTAMP", conflicts_with = "include_historical")]
    pub as_of: Option<String>,
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
pub struct MineArgs {
    /// The directory to mine.
    pub path: PathBuf,
    /// The wing to file mined drawers under. Defaults to the directory name.
    #[arg(long)]
    pub wing: Option<String>,
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
    pub scope: Option<String>,
}

/// Arguments for `memcastle embed`.
#[derive(Debug, Args)]
pub struct EmbedArgs {
    /// Only embed the drawers of this wing, by name.
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
    Status(StatusArgs),
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
    /// Print the endpoint's details as JSON instead of text, for scripts.
    #[arg(long)]
    pub json: bool,
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
            "job", "diary", "auth", "db", "daemon", "wing", "room", "drawer",
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
    fn job_list_has_no_json_flag_because_a_pipe_already_gets_json() {
        assert!(Cli::try_parse_from(["memcastle", "job", "list", "--json"]).is_err());
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
