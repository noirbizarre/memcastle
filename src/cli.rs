//! Argument types only.
//!
//! No behaviour lives here: parsing is one concern and doing the work is
//! another, and keeping them apart is what lets the library be used without
//! the CLI. See `main.rs` for what each variant actually does.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Local-first, always-on memory server for AI coding agents over MCP/HTTP
#[derive(Debug, Parser)]
#[command(name = "memcastle", version, about, long_about = None)]
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
    #[arg(long, global = true, env = "MEMCASTLE_MODE")]
    pub mode: Option<memcastle::domain::MemoryMode>,

    /// Path to a config file. Defaults to `$XDG_CONFIG_HOME/memcastle/config.toml`
    /// (`~/.config/memcastle/config.toml`) if it exists.
    #[arg(long, global = true, env = "MEMCASTLE_CONFIG")]
    pub config: Option<PathBuf>,

    /// The palace directory to use, overriding `palace.path` and
    /// `MEMCASTLE_PALACE_PATH`. Defaults to `$XDG_DATA_HOME/memcastle/default`
    /// (`~/.local/share/memcastle/default`). Must be an absolute path.
    /// Deliberately not bound to the environment variable here: that one is
    /// applied by the config loader, one layer below this flag.
    #[arg(long, global = true, value_name = "PATH")]
    pub palace: Option<PathBuf>,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon in the foreground. `daemon` is an alias — both start
    /// the same long-running server.
    #[command(alias = "daemon")]
    Serve(ServeArgs),
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
    /// Ask a running daemon to shut down gracefully.
    Stop,
    /// Stop the daemon, then start a fresh one and wait until it is serving
    /// (best-effort; for supervised deployments, prefer restarting through
    /// your process manager). `--config`, `--bind`, `--port`, `--assets-dir`
    /// and the resolved `--palace` are passed on to the new daemon.
    Restart(ServeArgs),
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
    /// Submit a repair job: a narrow, dry-run-first set of destructive
    /// palace-consistency fixes (see `memcastle::repair`'s module doc for
    /// exactly what it does).
    Repair(RepairArgs),
    /// Read or write diary entries scoped to an agent identity.
    #[command(subcommand)]
    Diary(DiaryCommand),
    /// Inspect and control jobs.
    #[command(subcommand)]
    Jobs(JobsCommand),
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
    /// List wings. Not yet implemented.
    Wings,
    /// List rooms. Not yet implemented.
    Rooms,
    /// List drawers. Not yet implemented.
    Drawers,
    /// Maintenance operations (dedup, stale-data sweep, ...). Not yet implemented.
    Maintenance,
}

/// Arguments for `memcastle status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print the report as JSON instead of text, for scripts.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `memcastle serve`.
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

/// Arguments for `memcastle repair`.
#[derive(Debug, Args)]
pub struct RepairArgs {
    /// Actually perform the planned actions. Without this flag, repair
    /// always runs in dry-run mode: it reports what it would do without
    /// mutating anything (see `memcastle::repair`'s module doc).
    #[arg(long)]
    pub apply: bool,
    /// Restrict repair actions to what a specific prior `memcastle audit`
    /// job (its job id) found, rather than scanning the whole palace fresh.
    #[arg(long)]
    pub based_on_job: Option<String>,
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

/// `memcastle auth` subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Generate a high-entropy token and print it to standard output, once.
    /// The daemon keeps only a digest, so the token cannot be shown again:
    /// store it in a secret manager, then set `auth.enabled` and restart.
    /// Generating again replaces the previous token, which is rotation.
    /// While authentication is enabled this needs a valid token
    /// (MEMCASTLE_AUTH_TOKEN) like every other command.
    Generate,
    /// Revoke the generated token, so it stops working immediately. A shared
    /// secret set through MEMCASTLE_AUTH_TOKEN or `auth.token` is not
    /// affected: change the configuration and restart to revoke that one.
    Revoke,
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

/// `memcastle jobs` subcommands.
#[derive(Debug, Subcommand)]
pub enum JobsCommand {
    /// List jobs, optionally filtered by status.
    List {
        /// One of: queued, running, paused, completed, failed, cancelled.
        #[arg(long)]
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
    /// Cancel a queued, paused, or running job.
    Cancel {
        /// The job id.
        id: String,
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

    #[test]
    fn the_daemon_alias_takes_the_same_listener_flags() {
        let serve = serve_args(&["daemon", "--bind", "::1", "--port", "9000"]).unwrap();
        assert_eq!(serve.bind, Some("::1".parse().unwrap()));
        assert_eq!(serve.port, Some(9000));
    }

    #[test]
    fn serve_without_listener_flags_leaves_both_unset_so_the_lower_layers_apply() {
        let serve = serve_args(&["serve"]).unwrap();
        assert_eq!((serve.bind, serve.port), (None, None));
    }

    #[test]
    fn auth_generate_and_revoke_parse_and_take_no_token_argument() {
        for (word, expected) in [("generate", "Generate"), ("revoke", "Revoke")] {
            let cli = Cli::try_parse_from(["memcastle", "auth", word]).unwrap();
            assert!(
                matches!(&cli.command, Command::Auth(auth) if format!("{auth:?}") == expected),
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
}
