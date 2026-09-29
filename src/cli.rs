//! Argument types only.
//!
//! No behaviour lives here: parsing is one concern and doing the work is
//! another, and keeping them apart is what lets the library be used without
//! the CLI. See `main.rs` for what each variant actually does.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Local-first, always-on memory server for AI coding agents over MCP/HTTP
#[derive(Debug, Parser)]
#[command(name = "memcastle", version, about, long_about = None)]
pub struct Cli {
    /// Increase verbosity. Repeat for more.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Path to a config file. Defaults to `~/.memcastle/config.toml` if it exists.
    #[arg(long, global = true, env = "MEMCASTLE_CONFIG")]
    pub config: Option<PathBuf>,

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
    /// Report daemon health and job counts.
    Status,
    /// Ask a running daemon to shut down gracefully.
    Stop,
    /// Stop the daemon, then start a fresh one (best-effort; for supervised
    /// deployments, prefer restarting through your process manager).
    Restart,
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
    /// List wings. Not yet implemented.
    Wings,
    /// List rooms. Not yet implemented.
    Rooms,
    /// List drawers. Not yet implemented.
    Drawers,
    /// Maintenance operations (dedup, stale-data sweep, ...). Not yet implemented.
    Maintenance,
}

/// Arguments for `memcastle serve`.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Override the configured HTTP bind address.
    #[arg(long)]
    pub bind: Option<SocketAddr>,
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
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
    /// Explicit dry run — already the default without `--apply`; only
    /// useful to make a script's intent unambiguous.
    #[arg(long)]
    pub dry_run: bool,
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
}
