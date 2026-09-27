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
    #[diagnostic(code(memcastle::error::io))]
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

    /// A SurrealDB operation failed.
    #[error("storage backend error")]
    #[diagnostic(code(memcastle::store::backend))]
    Store {
        /// The underlying driver error.
        #[source]
        source: surrealdb::Error,
    },

    /// A row read back from the store didn't shape the way we expected.
    #[error("storage returned malformed data: {message}")]
    #[diagnostic(code(memcastle::store::malformed))]
    StoreMalformed {
        /// What was malformed.
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
    #[diagnostic(code(memcastle::jobs::not_found))]
    JobNotFound {
        /// The id that was looked up.
        id: String,
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

    /// No daemon is reachable for this palace.
    #[error("no running memcastle daemon found for this palace")]
    #[diagnostic(
        code(memcastle::client::not_running),
        help("start one with `memcastle serve`")
    )]
    DaemonNotRunning,

    /// The HTTP server failed to bind or serve.
    #[error("server error: {message}")]
    #[diagnostic(code(memcastle::server::failure))]
    Server {
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

    /// Build an [`Error::StoreMalformed`] from a message.
    pub fn store_malformed(message: impl Into<String>) -> Self {
        Self::StoreMalformed {
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
