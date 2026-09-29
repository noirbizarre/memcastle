//! The error type, and the diagnostics it renders to.
//!
//! `thiserror` defines them, `miette` renders them. A diagnostic must carry the
//! two things the user does not already know: what specifically failed, and
//! what to do about it.

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong.
///
/// Diagnostic codes are `memcastle::<module>::<kind>`. A code is a public
/// identifier users grep for, so renaming one is a breaking change.
///
/// `<module>` names the part of the system the user is dealing with, and
/// `<kind>` says what is wrong with it: a condition (`invalid`, `malformed`,
/// `not_found`, `locked`, `pending`, `forbidden`-style words), or `failed` /
/// `<thing>_failed` when an operation itself broke. The
/// `every_error_variant_has_a_well_shaped_unique_code_and_a_help_line` test
/// is the enforced reference: it checks every variant's code against this
/// shape.
#[derive(Debug, Error, Diagnostic)]
#[non_exhaustive]
pub enum Error {
    /// Reading or writing a file failed.
    #[error("failed to access `{path}`")]
    #[diagnostic(
        code(memcastle::io::failed),
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
    #[error("{count} migration(s) pending: {pending}")]
    #[diagnostic(
        code(memcastle::migrate::pending),
        help(
            "run `memcastle migrate` (or start the daemon, which applies them) to bring the palace up to date"
        )
    )]
    MigrationsPending {
        /// How many migrations are pending.
        count: usize,
        /// The names of the pending migration steps, for display.
        pending: String,
    },

    /// A SurrealDB operation failed.
    #[error("storage backend error")]
    #[diagnostic(
        code(memcastle::store::backend_failed),
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

    /// SurrealKit's schema `Sync` failed. MemCastle
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
        code(memcastle::domain::empty_label),
        help("give the entity/relationship a short, descriptive label")
    )]
    EmptyLabel {
        /// Which field was rejected (`"kind"` or `"predicate"`).
        field: String,
    },

    /// A value could not be turned into (or out of) its JSON form. Kept apart
    /// from [`Error::StoreMalformed`], whose help sends the user to migrations
    /// and `memcastle audit` — advice that is wrong for, say, a registry file
    /// or a tool response that failed to serialize.
    #[error("failed to serialize {what}: {message}")]
    #[diagnostic(
        code(memcastle::serialization::failed),
        help("this is a bug in memcastle: please report it with the command that triggered it")
    )]
    Serialization {
        /// What was being serialized.
        what: String,
        /// The serializer's message.
        message: String,
    },

    /// A knowledge-graph mutation named a relationship that does not exist —
    /// a mistyped or already-removed id, which must not be reported as if the
    /// fact had been superseded or retracted.
    #[error("relationship {id} not found")]
    #[diagnostic(
        code(memcastle::graph::relationship_not_found),
        help(
            "check the relationship id in the checkpoint item's `fact`: it must be one this palace holds"
        )
    )]
    RelationshipNotFound {
        /// The id that was looked up.
        id: String,
    },

    /// A job transition was rejected by the state machine.
    #[error("job {id} cannot go from {from:?} to {event:?}")]
    #[diagnostic(
        code(memcastle::jobs::invalid_transition),
        help(
            "valid transitions: queued->running, running->paused, paused->queued, running->completed, running->failed, running->queued (crash recovery), failed->queued (retry), queued|paused|running->cancelled"
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

    /// A request reached (or tried to reach) the daemon and failed in
    /// transport: a timeout, a dropped connection, an undecodable reply.
    /// A refused connection is [`Error::DaemonNotRunning`] instead, so this
    /// help does not claim the daemon is down — it may well be up and slow.
    #[error("request to the daemon failed: {message}")]
    #[diagnostic(
        code(memcastle::client::request_failed),
        help(
            "the daemon may be overloaded or restarting; retry, and check `memcastle status` and the daemon's log"
        )
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
    #[diagnostic(code(memcastle::client::remote_rejected))]
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

    /// A job is recorded as `Running` but nothing in this daemon is running
    /// it. Distinct from [`Error::Server`], whose help is about the bind
    /// address and would send the user to the wrong place.
    #[error("job {id} is marked running but has no worker")]
    #[diagnostic(
        code(memcastle::jobs::orphaned),
        help(
            "restart the daemon (`memcastle restart`): startup recovery re-queues jobs left running"
        )
    )]
    JobOrphaned {
        /// The orphaned job.
        id: String,
    },

    /// A worker tried to write a job it no longer holds the lease on: the
    /// lease lapsed (a stalled or partitioned daemon) and another daemon
    /// reaped the job, so this worker's copy is stale and its write was
    /// refused rather than allowed to clobber the new owner's.
    #[error("job {id} is no longer leased to this daemon")]
    #[diagnostic(
        code(memcastle::jobs::lease_lost),
        help(
            "another daemon took the job over after its lease expired; if this daemon was only slow, raise jobs.lease_ttl_secs"
        )
    )]
    LeaseLost {
        /// The job whose lease was lost.
        id: String,
    },

    /// The HTTP server failed while starting or serving (a failed *bind* is
    /// [`Error::ServerBind`]).
    #[error("server error: {message}")]
    #[diagnostic(
        code(memcastle::server::failed),
        help("run `memcastle serve -v` in the foreground to see what the daemon was doing")
    )]
    Server {
        /// What went wrong.
        message: String,
    },

    /// The daemon could not listen on the configured address and port.
    ///
    /// Its own variant, not [`Error::Server`], because the fix depends on
    /// *why* the bind failed (port taken, port privileged, address not on
    /// this machine) and the help text is chosen from the OS error kind.
    #[error("cannot listen on {addr}: {source}")]
    #[diagnostic(code(memcastle::server::bind_failed), help("{}", bind_help(source)))]
    ServerBind {
        /// The address the daemon tried to bind.
        addr: std::net::SocketAddr,
        /// The OS error.
        #[source]
        source: std::io::Error,
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

/// What every interface reports about a failure: the message, and the two
/// things the message alone does not carry — the diagnostic `code` users grep
/// for and the `help` that says what to do. The REST API serves it as the
/// body of an error response and MCP as the text of an error result, so a
/// failure reads the same whichever way it was reached.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The message.
    pub error: String,
    /// The diagnostic code, `memcastle::<module>::<kind>`.
    pub code: Option<String>,
    /// What to do about it.
    pub help: Option<String>,
}

impl Error {
    /// This error as the body every interface reports — see [`ErrorBody`].
    #[must_use]
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: self.to_string(),
            code: self.code().map(|code| code.to_string()),
            help: self.help().map(|help| help.to_string()),
        }
    }

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

    /// Parse a job id a caller supplied, raising [`Error::InvalidJobId`] for
    /// one that is not shaped like a job id. The one parser REST, MCP and the
    /// CLI share, so the same mistake gets the same diagnostic everywhere.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidJobId`] if `raw` is not a job id.
    pub fn parse_job_id(raw: &str) -> Result<crate::domain::JobId> {
        raw.parse().map_err(|_| Self::invalid_job_id(raw))
    }

    /// Parse a job status filter a caller supplied, raising
    /// [`Error::InvalidInput`] (naming `status` and the accepted values)
    /// for an unknown one. Shared by REST, MCP and the CLI for the same reason
    /// as [`Error::parse_job_id`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `raw` is not a job status.
    pub fn parse_job_status(raw: &str) -> Result<crate::domain::JobStatus> {
        raw.parse()
            .map_err(|message: String| Self::invalid_input("status", message))
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

    /// Build an [`Error::Serialization`].
    pub fn serialization(what: impl Into<String>, message: impl ToString) -> Self {
        Self::Serialization {
            what: what.into(),
            message: message.to_string(),
        }
    }

    /// Build an [`Error::Server`] from a message.
    pub fn server(message: impl Into<String>) -> Self {
        Self::Server {
            message: message.into(),
        }
    }

    /// Build an [`Error::ServerBind`].
    #[must_use]
    pub fn server_bind(addr: std::net::SocketAddr, source: std::io::Error) -> Self {
        Self::ServerBind { addr, source }
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

/// The fix for a failed bind, chosen by what the OS said: each cause has a
/// different remedy, and "check the address" would send someone whose port is
/// simply taken to the wrong setting.
fn bind_help(source: &std::io::Error) -> &'static str {
    match source.kind() {
        std::io::ErrorKind::AddrInUse => {
            "another process (possibly another memcastle daemon) already listens on that port: \
             stop it, or pick a free port with `--port`, MEMCASTLE_PORT or `server.port`"
        }
        std::io::ErrorKind::PermissionDenied => {
            "binding this port needs privileges (ports below 1024 usually do): \
             pick a higher port with `--port`, MEMCASTLE_PORT or `server.port`"
        }
        std::io::ErrorKind::AddrNotAvailable => {
            "this machine has no interface with that address: \
             check `--bind`, MEMCASTLE_BIND or `server.bind` (`127.0.0.1` is always available)"
        }
        _ => {
            "check the listener address and port (`--bind`/`--port`, MEMCASTLE_BIND/MEMCASTLE_PORT \
             or `server.bind`/`server.port`)"
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

    /// One sample of every variant. The exhaustive `match` in
    /// [`covered`] has no wildcard arm, so adding a variant fails to compile
    /// until it is listed there — and the reminder beside it says to add a
    /// sample here, which is what puts the new code under the tests below.
    fn samples() -> Vec<Error> {
        vec![
            Error::io("/nowhere", std::io::Error::other("denied")),
            Error::config("bad"),
            Error::invalid_input("status", "unknown"),
            Error::invalid_job_id("nope"),
            Error::not_implemented("memcastle wings"),
            Error::MigrationsPending {
                count: 1,
                pending: "canonical-timestamps".to_string(),
            },
            Error::Store {
                source: surrealdb::Error::internal("boom".to_string()),
            },
            Error::store_malformed("bad row"),
            Error::schema_sync("bad define"),
            Error::EmptyLabel {
                field: "kind".to_string(),
            },
            Error::serialization("a thing", "nope"),
            Error::RelationshipNotFound {
                id: "x".to_string(),
            },
            Error::InvalidJobTransition {
                id: "x".to_string(),
                from: "Queued".to_string(),
                event: "Pause".to_string(),
            },
            Error::JobNotFound {
                id: "x".to_string(),
            },
            Error::InvalidBasedOnJob {
                id: "x".to_string(),
                message: "nope".to_string(),
            },
            Error::Client {
                message: "timeout".to_string(),
            },
            Error::remote(400, Some("memcastle::x::y"), "nope", None),
            Error::DaemonNotRunning,
            Error::JobOrphaned {
                id: "x".to_string(),
            },
            Error::LeaseLost {
                id: "x".to_string(),
            },
            Error::server("boom"),
            Error::server_bind(
                std::net::SocketAddr::from(([127, 0, 0, 1], 8420)),
                std::io::Error::from(std::io::ErrorKind::AddrInUse),
            ),
            Error::ModeForbidden {
                operation: "checkpoint".to_string(),
                mode: crate::domain::MemoryMode::Disabled,
            },
            Error::migration_locked("someone"),
            Error::migration_failed(1, "step", "boom"),
        ]
    }

    /// Compile-time guard that [`samples`] lists every variant: no wildcard.
    /// When this stops compiling, add the new variant here *and* a sample.
    fn covered(error: &Error) {
        match error {
            Error::Io { .. }
            | Error::Config { .. }
            | Error::InvalidInput { .. }
            | Error::InvalidJobId { .. }
            | Error::NotImplemented { .. }
            | Error::MigrationsPending { .. }
            | Error::Store { .. }
            | Error::StoreMalformed { .. }
            | Error::SchemaSync { .. }
            | Error::EmptyLabel { .. }
            | Error::Serialization { .. }
            | Error::RelationshipNotFound { .. }
            | Error::InvalidJobTransition { .. }
            | Error::JobNotFound { .. }
            | Error::InvalidBasedOnJob { .. }
            | Error::Client { .. }
            | Error::Remote { .. }
            | Error::DaemonNotRunning
            | Error::JobOrphaned { .. }
            | Error::LeaseLost { .. }
            | Error::Server { .. }
            | Error::ServerBind { .. }
            | Error::ModeForbidden { .. }
            | Error::MigrationLocked { .. }
            | Error::MigrationFailed { .. } => {}
        }
    }

    /// The single reference for the `memcastle::<module>::<kind>` shape:
    /// exactly three segments, lowercase snake_case, so a code is greppable
    /// and a new one cannot drift from the convention.
    fn assert_code_shape(code: &str) {
        let segments: Vec<&str> = code.split("::").collect();
        assert_eq!(
            segments.len(),
            3,
            "`{code}` is not memcastle::<module>::<kind>"
        );
        assert_eq!(
            segments[0], "memcastle",
            "`{code}` is not in the memcastle namespace"
        );
        for segment in &segments[1..] {
            assert!(
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "`{code}` has a segment that is not lower snake_case"
            );
        }
    }

    /// AGENTS.md: a diagnostic must say what to do. Every variant carries a
    /// code of the documented shape, unique across the enum, and a help line
    /// (`Remote` forwards the daemon's own, so it may legitimately have none).
    #[test]
    fn every_error_variant_has_a_well_shaped_unique_code_and_a_help_line() {
        let mut seen = std::collections::HashSet::new();
        for error in samples() {
            covered(&error);
            let code = error.code().map(|c| c.to_string()).unwrap_or_default();
            assert_code_shape(&code);
            assert!(
                seen.insert(code.clone()),
                "`{code}` is used by two variants"
            );
            if !matches!(error, Error::Remote { .. }) {
                assert!(error.help().is_some(), "{code} has no help line");
            }
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

    #[test]
    fn a_failed_bind_names_the_address_and_the_help_follows_the_os_error() {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8420));
        let cases = [
            (std::io::ErrorKind::AddrInUse, "--port"),
            (std::io::ErrorKind::PermissionDenied, "privileges"),
            (std::io::ErrorKind::AddrNotAvailable, "--bind"),
            (std::io::ErrorKind::Other, "--bind"),
        ];
        for (kind, expected) in cases {
            let error = Error::server_bind(addr, std::io::Error::from(kind));
            assert!(error.to_string().contains("127.0.0.1:8420"), "{error}");
            assert_eq!(
                error.code().map(|c| c.to_string()).as_deref(),
                Some("memcastle::server::bind_failed")
            );
            let help = error.help().unwrap().to_string();
            assert!(help.contains(expected), "{kind:?}: {help}");
        }
    }

    #[test]
    fn an_error_body_carries_the_message_the_code_and_the_help() {
        let body = Error::invalid_job_id("nope").body();

        assert!(body.error.contains("nope"));
        assert_eq!(body.code.as_deref(), Some("memcastle::jobs::invalid_id"));
        assert!(body.help.is_some());
    }
}
