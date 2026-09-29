//! The error type, and the diagnostics it renders to.
//!
//! `thiserror` defines them, `miette` renders them. A diagnostic must carry the
//! two things the user does not already know: what specifically failed, and
//! what to do about it.

use miette::Diagnostic;
use thiserror::Error;

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong.
///
/// Diagnostic codes are `memcastle::<module>::<kind>`. A code is a public
/// identifier users grep for, so renaming one is a breaking change.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum Error {
    /// Reading or writing a file failed.
    #[error("failed to access `{path}`")]
    #[diagnostic(
        code(memcastle::error::io),
        help("check that the path exists and that this user may read and write it")
    )]
    Io {
        /// The path that could not be accessed.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },

    /// Configuration failed to load or did not pass validation.
    #[error("invalid configuration: {message}")]
    #[diagnostic(
        code(memcastle::config::invalid),
        help("check the config file or the MEMCASTLE_* environment variables")
    )]
    Config {
        /// What was wrong.
        message: String,
    },

    /// A caller-supplied value (a CLI argument, request parameter or header)
    /// was not one this operation accepts. Distinct from [`Error::Config`],
    /// whose help points at the config file — the fix here is the input.
    #[error("invalid {field}: {message}")]
    #[diagnostic(
        code(memcastle::input::invalid),
        help("fix the value and retry; `memcastle --help` lists what each argument accepts")
    )]
    InvalidInput {
        /// Which argument, parameter or header was rejected.
        field: String,
        /// What was wrong with it.
        message: String,
    },

    /// A job id that isn't shaped like one, as opposed to a well-formed id
    /// no job has ([`Error::JobNotFound`]).
    #[error("`{raw}` is not a job id")]
    #[diagnostic(
        code(memcastle::jobs::invalid_id),
        help("job ids are UUIDs, as printed by `memcastle jobs list`")
    )]
    InvalidJobId {
        /// What was given.
        raw: String,
    },

    /// The command exists so the architecture has a place for it, but
    /// nothing implements it yet.
    #[error("`{feature}` is not implemented yet")]
    #[diagnostic(
        code(memcastle::cli::not_implemented),
        help("this command is reserved by the architecture; see docs/architecture.md")
    )]
    NotImplemented {
        /// The unimplemented command or feature.
        feature: String,
    },

    /// `memcastle migrate --check` found migrations that have not been
    /// applied.
    #[error("{count} migration(s) pending: {versions}")]
    #[diagnostic(
        code(memcastle::migrate::pending),
        help(
            "run `memcastle migrate` (or start the daemon, which applies them) to bring the palace up to date"
        )
    )]
    MigrationsPending {
        /// How many migrations are pending.
        count: usize,
        /// The pending versions, for display.
        versions: String,
    },

    /// A SurrealDB operation failed.
    #[error("storage backend error")]
    #[diagnostic(
        code(memcastle::store::backend),
        help(
            "for an embedded palace, check that no other memcastle process holds it (`memcastle status`); for a remote one, check its URL and credentials"
        )
    )]
    Store {
        /// The underlying driver error.
        #[source]
        source: surrealdb::Error,
    },

    /// A row read back from the store didn't shape the way we expected.
    #[error("storage returned malformed data: {message}")]
    #[diagnostic(
        code(memcastle::store::malformed),
        help(
            "the palace may have been written by a different memcastle version; run `memcastle migrate --status`, and `memcastle audit` to look for damage"
        )
    )]
    StoreMalformed {
        /// What was malformed.
        message: String,
    },

    /// SurrealKit's schema `Sync` (or its `dry_run` check) failed. MemCastle
    /// delegates all schema management to SurrealKit (see
    /// `docs/adr/004-versioned-database-migrations.md`) rather than
    /// reimplementing schema diffing, so this wraps whatever SurrealKit
    /// itself reports.
    #[error("schema sync failed: {message}")]
    #[diagnostic(
        code(memcastle::store::schema_sync),
        help("check database/schema/*.surql for a malformed DEFINE statement")
    )]
    SchemaSync {
        /// What SurrealKit reported.
        message: String,
    },

    /// A knowledge-graph label (`Entity::kind` / `Relationship::predicate`)
    /// was empty or whitespace-only after normalization.
    #[error("{field} must not be empty")]
    #[diagnostic(
        code(memcastle::store::empty_label),
        help("give the entity/relationship a short, descriptive label")
    )]
    EmptyLabel {
        /// Which field was rejected (`"kind"` or `"predicate"`).
        field: String,
    },

    /// A job transition was rejected by the state machine.
    #[error("job {id} cannot go from {from:?} to {event:?}")]
    #[diagnostic(
        code(memcastle::jobs::invalid_transition),
        help(
            "valid transitions: queued->running, running->paused, paused->queued, running->completed, running->failed, failed->queued (retry), queued|paused|running->cancelled"
        )
    )]
    InvalidJobTransition {
        /// The job that rejected the transition.
        id: String,
        /// The status it was in.
        from: String,
        /// The event that was rejected.
        event: String,
    },

    /// No job exists with the given id.
    #[error("job {id} not found")]
    #[diagnostic(
        code(memcastle::jobs::not_found),
        help("list the jobs this daemon knows about with `memcastle jobs list`")
    )]
    JobNotFound {
        /// The id that was looked up.
        id: String,
    },

    /// A `JobKind::Repair`'s `based_on_job` didn't resolve to a completed
    /// `JobKind::Audit` job — a caller-facing mistake (wrong id, a job of
    /// the wrong kind, an audit that hasn't finished yet), not a storage
    /// failure. See `crate::repair::run`'s doc comment.
    #[error("based_on_job {id}: {message}")]
    #[diagnostic(
        code(memcastle::repair::invalid_based_on_job),
        help("based_on_job must be the id of a completed `memcastle audit` job")
    )]
    InvalidBasedOnJob {
        /// The id that was given.
        id: String,
        /// What was wrong with it.
        message: String,
    },

    /// A request to a running daemon failed.
    #[error("request to the daemon failed: {message}")]
    #[diagnostic(
        code(memcastle::client::request),
        help("is the daemon running? try `memcastle serve`")
    )]
    Client {
        /// What went wrong.
        message: String,
    },

    /// The daemon answered, but with an error. Carries what the daemon said —
    /// its diagnostic code, message and help — so the CLI shows the real
    /// cause instead of collapsing every rejection into "is the daemon
    /// running?", which is plainly false when it just replied.
    #[error("the daemon rejected the request ({status}{code}): {message}")]
    #[diagnostic(code(memcastle::client::remote))]
    Remote {
        /// The HTTP status the daemon answered with.
        status: u16,
        /// The daemon's own diagnostic code, formatted as `, <code>` (empty
        /// when the response carried none), ready to splice into the message.
        code: String,
        /// The daemon's error message.
        message: String,
        /// The daemon's own advice, if it gave any.
        #[help]
        help: Option<String>,
    },

    /// No daemon is reachable for this palace.
    #[error("no running memcastle daemon found for this palace")]
    #[diagnostic(
        code(memcastle::client::not_running),
        help("start one with `memcastle serve`")
    )]
    DaemonNotRunning,

    /// The HTTP server failed to bind or serve.
    #[error("server error: {message}")]
    #[diagnostic(
        code(memcastle::server::failure),
        help(
            "check that the bind address is free, or pick another with `--bind` or MEMCASTLE_BIND"
        )
    )]
    Server {
        /// What went wrong.
        message: String,
    },

    /// A memory operation was rejected by the calling session/request's
    /// [`crate::domain::MemoryMode`]: `ReadOnly` rejects writes, `Disabled`
    /// rejects everything (see that type's doc comment for the full
    /// allow/deny matrix). Returned before the store is ever touched.
    #[error("`{operation}` is not permitted in {mode:?} mode")]
    #[diagnostic(
        code(memcastle::app::mode_forbidden),
        help(
            "switch the session/request to Full mode to allow writes, or to Full/ReadOnly to allow reads"
        )
    )]
    ModeForbidden {
        /// The operation that was rejected (e.g. `"checkpoint"`, `"diary_write"`).
        operation: String,
        /// The mode that forbade it.
        mode: crate::domain::MemoryMode,
    },

    /// Another run already holds the exclusive migration lock (see
    /// `crate::migrate` and `store::migration_state`).
    #[error("a migration is already in progress (held by {owner})")]
    #[diagnostic(
        code(memcastle::migrate::locked),
        help(
            "wait for the other migration to finish; if you believe the lock is stuck from a crashed run, it expires and can be reclaimed on its own"
        )
    )]
    MigrationLocked {
        /// The lock's current holder.
        owner: String,
    },

    /// A data migration step failed partway through a `crate::migrate::run`.
    /// The version watermark is left at the last step that succeeded, so a
    /// later run resumes from here rather than re-applying it.
    #[error("migration {version} ({name}) failed: {message}")]
    #[diagnostic(
        code(memcastle::migrate::failed),
        help(
            "migrations are immutable once released — fix forward with a new migration, never edit this one"
        )
    )]
    MigrationFailed {
        /// The failing step's version.
        version: u32,
        /// The failing step's name.
        name: String,
        /// What went wrong.
        message: String,
    },
}

impl Error {
    /// Build an [`Error::Io`] with path context.
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Build an [`Error::Config`] from a message.
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    /// Build an [`Error::InvalidInput`].
    pub fn invalid_input(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidInput {
            field: field.into(),
            message: message.into(),
        }
    }

    /// Build an [`Error::InvalidJobId`].
    pub fn invalid_job_id(raw: impl Into<String>) -> Self {
        Self::InvalidJobId { raw: raw.into() }
    }

    /// Build an [`Error::NotImplemented`].
    pub fn not_implemented(feature: impl Into<String>) -> Self {
        Self::NotImplemented {
            feature: feature.into(),
        }
    }

    /// Build an [`Error::Remote`] from a daemon's error response.
    pub fn remote(
        status: u16,
        code: Option<&str>,
        message: impl Into<String>,
        help: Option<String>,
    ) -> Self {
        Self::Remote {
            status,
            code: code.map(|code| format!(", {code}")).unwrap_or_default(),
            message: message.into(),
            help,
        }
    }

    /// Build an [`Error::StoreMalformed`] from a message.
    pub fn store_malformed(message: impl Into<String>) -> Self {
        Self::StoreMalformed {
            message: message.into(),
        }
    }

    /// Build an [`Error::SchemaSync`] from a message.
    pub fn schema_sync(message: impl Into<String>) -> Self {
        Self::SchemaSync {
            message: message.into(),
        }
    }

    /// Build an [`Error::Client`] from a message.
    pub fn client(message: impl Into<String>) -> Self {
        Self::Client {
            message: message.into(),
        }
    }

    /// Build an [`Error::Server`] from a message.
    pub fn server(message: impl Into<String>) -> Self {
        Self::Server {
            message: message.into(),
        }
    }

    /// Build an [`Error::MigrationLocked`].
    pub fn migration_locked(owner: impl Into<String>) -> Self {
        Self::MigrationLocked {
            owner: owner.into(),
        }
    }

    /// Build an [`Error::MigrationFailed`].
    pub fn migration_failed(
        version: u32,
        name: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::MigrationFailed {
            version,
            name: name.into(),
            message: message.into(),
        }
    }
}

impl From<surrealdb::Error> for Error {
    fn from(source: surrealdb::Error) -> Self {
        Self::Store { source }
    }
}

impl From<crate::domain::TransitionError> for Error {
    fn from(err: crate::domain::TransitionError) -> Self {
        Self::InvalidJobTransition {
            id: err.id.to_string(),
            from: format!("{:?}", err.from),
            event: format!("{:?}", err.event),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(source: reqwest::Error) -> Self {
        Self::Client {
            message: source.to_string(),
        }
    }
}

impl From<toml::de::Error> for Error {
    fn from(source: toml::de::Error) -> Self {
        Self::Config {
            message: source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use miette::Diagnostic;

    use super::*;

    /// AGENTS.md: a diagnostic must say what to do. Every variant a user can
    /// realistically hit without a wrapped source error to explain it needs
    /// a `help`, and a code in the `memcastle::` namespace.
    #[test]
    fn user_facing_errors_carry_a_help_line_and_a_memcastle_code() {
        let errors = [
            Error::invalid_input("status", "unknown"),
            Error::invalid_job_id("nope"),
            Error::not_implemented("memcastle wings"),
            Error::MigrationsPending {
                count: 1,
                versions: "[2]".to_string(),
            },
            Error::JobNotFound {
                id: "x".to_string(),
            },
            Error::server("boom"),
            Error::store_malformed("bad row"),
            Error::io("/nowhere", std::io::Error::other("denied")),
        ];
        for error in errors {
            let code = error.code().map(|c| c.to_string()).unwrap_or_default();
            assert!(
                code.starts_with("memcastle::"),
                "bad code on {error:?}: {code}"
            );
            assert!(error.help().is_some(), "{code} has no help line");
        }
    }

    #[test]
    fn invalid_input_and_config_errors_point_the_user_at_different_fixes() {
        let input = Error::invalid_input("status", "unknown")
            .help()
            .unwrap()
            .to_string();
        let config = Error::config("bad").help().unwrap().to_string();
        assert!(
            !input.contains("config file"),
            "input help must not blame the config: {input}"
        );
        assert!(config.contains("config file"));
    }
}
