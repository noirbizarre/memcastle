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
        help("job ids are UUIDs, as printed by `memcastle job list`")
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

    /// The user declined a confirmation prompt, so nothing was changed.
    /// An error rather than a quiet success: a script that chains commands
    /// must stop, and the exit code must say the action did not happen.
    #[error("{action} was not confirmed, nothing was changed")]
    #[diagnostic(
        code(memcastle::cli::aborted),
        help("run the command again and answer `y`, or pass `--yes` to skip the confirmation")
    )]
    Aborted {
        /// What would have been done, e.g. "applying repairs".
        action: String,
    },

    /// A confirmation prompt could not be shown or read (the terminal went
    /// away, or the user pressed Ctrl-C).
    #[error("could not ask for confirmation: {message}")]
    #[diagnostic(
        code(memcastle::cli::prompt_failed),
        help(
            "pass `--yes` to skip the confirmation, or run the command from an interactive terminal"
        )
    )]
    PromptFailed {
        /// What the terminal reported.
        message: String,
    },

    /// The project's `.config/memcastle.toml`, or the `MEMCASTLE_WING` /
    /// `MEMCASTLE_ROOM` variables, cannot be used to scope a command.
    #[error("the project scope is not usable: {message}")]
    #[diagnostic(
        code(memcastle::project::invalid),
        help(
            "fix the file or variable it names (see docs/project-config.md), or pass `--wing` and `--room` explicitly"
        )
    )]
    ProjectInvalid {
        /// The file or variable at fault and what is wrong with it.
        message: String,
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

    /// A graph read named an entity that does not exist.
    #[error("entity {id} not found")]
    #[diagnostic(
        code(memcastle::graph::entity_not_found),
        help("list entities with `GET /api/entities` to find the id of the one you mean")
    )]
    EntityNotFound {
        /// The id that was looked up.
        id: String,
    },

    /// A job transition was rejected by the state machine.
    #[error("job {id} cannot go from {from} to {event}")]
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
        help("list the jobs this daemon knows about with `memcastle job list`")
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
        help("start one with `memcastle daemon start` (or `memcastle serve` in the foreground)")
    )]
    DaemonNotRunning,

    /// `daemon start` found a daemon already serving this palace. Starting a
    /// second one would only die on the palace's file lock, so it is refused
    /// up front with the command that does mean "replace it".
    #[error("a memcastle daemon is already running for this palace on {addr}")]
    #[diagnostic(
        code(memcastle::client::already_running),
        help("use `memcastle daemon restart` to replace it, or `memcastle status` to inspect it")
    )]
    DaemonAlreadyRunning {
        /// Where the running daemon listens.
        addr: String,
    },

    /// A job is recorded as `Running` but nothing in this daemon is running
    /// it. Distinct from [`Error::Server`], whose help is about the bind
    /// address and would send the user to the wrong place.
    #[error("job {id} is marked running but has no worker")]
    #[diagnostic(
        code(memcastle::jobs::orphaned),
        help(
            "restart the daemon (`memcastle daemon restart`): startup recovery re-queues jobs left running"
        )
    )]
    JobOrphaned {
        /// The orphaned job.
        id: String,
    },

    /// A job kept changing status faster than a control request could be
    /// applied. Its own variant, not [`Error::Server`], whose help would send
    /// the user to the daemon's foreground log when the fix is to retry.
    #[error("job {id} kept changing state while the request was being applied")]
    #[diagnostic(
        code(memcastle::jobs::contended),
        help("nothing was changed; run the command again")
    )]
    JobContended {
        /// The job that would not hold still.
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

    /// The operating system could not provide the randomness a token needs.
    /// MemCastle never falls back to a weaker source, so no token was made.
    #[error("the operating system could not provide randomness: {message}")]
    #[diagnostic(
        code(memcastle::auth::entropy_unavailable),
        help(
            "no token was generated; check that the system's random source (`getrandom`) is available to this process, then try again"
        )
    )]
    EntropyUnavailable {
        /// What the operating system reported.
        message: String,
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

    /// The operator-chosen assets directory does not exist or is not a
    /// directory.
    ///
    /// An explicit choice is never silently replaced by the installed or
    /// embedded assets: a typo'd `--assets-dir` would otherwise serve the
    /// wrong files with nothing to say so.
    #[error("assets directory {path} does not exist or is not a directory")]
    #[diagnostic(
        code(memcastle::assets::not_found),
        help(
            "point `--assets-dir`, MEMCASTLE_ASSETS_DIR or `assets.dir` at an existing directory, \
             or remove the setting to use the installed or embedded assets"
        )
    )]
    AssetsNotFound {
        /// The configured directory.
        path: String,
    },

    /// A memory operation was rejected by the calling session/request's
    /// [`crate::domain::MemoryMode`]: `ReadOnly` rejects writes, `Disabled`
    /// rejects everything (see that type's doc comment for the full
    /// allow/deny matrix). Returned before the store is ever touched.
    #[error("`{operation}` is not permitted in {mode} mode")]
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

    /// The daemon requires a bearer token and the request carried none, a
    /// malformed one, or the wrong one.
    ///
    /// Its own variant (and HTTP 401, not the 403 of [`Error::ModeForbidden`])
    /// so a client can tell "who are you?" apart from "you may not do that"
    /// and from "the daemon is not there". `reason` is a fixed word
    /// (`missing`, `malformed`, `invalid`) and never echoes the presented
    /// token: this error is logged.
    #[error("authentication required: the bearer token is {reason}")]
    #[diagnostic(
        code(memcastle::auth::unauthorized),
        help(
            "send `Authorization: Bearer <token>`; the CLI reads the token from MEMCASTLE_AUTH_TOKEN \
             or `auth.token`. A token can be (re)generated with `memcastle auth generate` \
             by someone who already holds a valid one"
        )
    )]
    Unauthorized {
        /// Why the credential was refused: `missing`, `malformed` or `invalid`.
        reason: &'static str,
    },

    /// Authentication is enabled but the daemon has nothing to check a token
    /// against, so it refuses to start rather than serve everything open or
    /// lock every client out with no way back in.
    #[error("authentication is enabled but no token is configured or stored")]
    #[diagnostic(
        code(memcastle::auth::not_configured),
        help(
            "set MEMCASTLE_AUTH_TOKEN (or `auth.token`), or start once with `auth.enabled = false`, \
             run `memcastle auth generate`, store the token, then enable authentication"
        )
    )]
    AuthNotConfigured,

    /// The database admin endpoint was asked to listen somewhere it would not
    /// be safe: beyond loopback without the explicit opt-in, or beyond loopback
    /// with authentication disabled.
    ///
    /// `reason` is a complete sentence naming which of the two it was.
    #[error("refusing to expose the database admin endpoint: {reason}")]
    #[diagnostic(
        code(memcastle::db::unsafe_bind),
        help(
            "the default is 127.0.0.1, which needs nothing; to listen elsewhere pass `--allow-remote` \
             (or set `db.allow_remote`) and enable authentication (`auth.enabled` with a token)"
        )
    )]
    DbEndpointUnsafe {
        /// Why the request was refused.
        reason: String,
    },

    /// The database admin endpoint is already listening, and the start request
    /// asked for a different bind, port or origin than it has.
    #[error(
        "the database admin endpoint is already listening on {addr}, which differs from what was requested"
    )]
    #[diagnostic(
        code(memcastle::db::already_running),
        help(
            "stop it first with `memcastle db stop` to change its settings, \
             or run `memcastle db start` without them to use it as it is"
        )
    )]
    DbEndpointRunning {
        /// Where it is listening.
        addr: String,
    },

    /// The database admin endpoint could not listen on the requested address.
    #[error("cannot start the database admin endpoint on {addr}: {source}")]
    #[diagnostic(
        code(memcastle::db::bind_failed),
        help(
            "pick a free port with `--port`, MEMCASTLE_DB_PORT or `db.port` (it must differ from the \
             daemon's own port), or use port 0 to let the OS choose"
        )
    )]
    DbEndpointBind {
        /// The address that could not be bound.
        addr: std::net::SocketAddr,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },

    /// The palace is on a remote SurrealDB server, so the daemon has no
    /// embedded database to expose.
    #[error("the palace uses a {backend} database, which the admin endpoint does not expose")]
    #[diagnostic(
        code(memcastle::db::unavailable),
        help(
            "the admin endpoint exists because an embedded SurrealKV database has no server of its own; \
             point SurrealDB Studio at the remote SurrealDB server directly"
        )
    )]
    DbEndpointUnavailable {
        /// The configured backend kind.
        backend: String,
    },

    /// No wing answers to the given name or id.
    #[error("wing `{wing}` not found")]
    #[diagnostic(
        code(memcastle::palace::wing_not_found),
        help(
            "list the wings with `memcastle wing list`, or create it with `memcastle wing create`"
        )
    )]
    WingNotFound {
        /// The name or id that was looked up.
        wing: String,
    },

    /// The wing exists but has no room answering to the given name or id.
    #[error("room `{room}` not found in wing `{wing}`")]
    #[diagnostic(
        code(memcastle::palace::room_not_found),
        help(
            "list the wing's rooms with `memcastle room list --wing <wing>`, \
             or create it with `memcastle room create <wing>/<room>`"
        )
    )]
    RoomNotFound {
        /// The wing that was searched.
        wing: String,
        /// The name or id that was looked up.
        room: String,
    },

    /// The room exists but holds no drawer answering to the given name or id.
    #[error("drawer `{drawer}` not found in `{room}`")]
    #[diagnostic(
        code(memcastle::palace::drawer_not_found),
        help("list the room's drawers with `memcastle drawer list --room <wing>/<room>`")
    )]
    DrawerNotFound {
        /// The `wing/room` that was searched.
        room: String,
        /// The name or id that was looked up.
        drawer: String,
    },

    /// A `wing/room/drawer` path, or one of its names, is not usable.
    #[error("invalid path `{raw}`: {message}")]
    #[diagnostic(
        code(memcastle::palace::invalid_path),
        help(
            "paths read `<wing>`, `<wing>/<room>` or `<wing>/<room>/<drawer>`; \
             wing and room names cannot contain `/`"
        )
    )]
    InvalidPalacePath {
        /// What was given.
        raw: String,
        /// What was wrong with it.
        message: String,
    },

    /// A drawer with that name already exists in the room, holding other
    /// content. Drawer content is immutable, so the name cannot be reused.
    #[error("a drawer named `{name}` already exists in `{room}` with different content")]
    #[diagnostic(
        code(memcastle::palace::drawer_name_taken),
        help(
            "pick another name, or delete the existing drawer first with `memcastle drawer delete`"
        )
    )]
    DrawerNameTaken {
        /// The `wing/room` the name is taken in.
        room: String,
        /// The name.
        name: String,
    },

    /// A wing or room cannot be deleted while a job that writes to the palace
    /// is pending: it could silently re-create what was just removed.
    #[error("cannot delete {target} while {count} palace-writing job(s) are active")]
    #[diagnostic(
        code(memcastle::palace::busy),
        help(
            "wait for the jobs to finish, or cancel them: see `memcastle job list` and `memcastle job cancel`"
        )
    )]
    PalaceBusy {
        /// What was to be deleted, e.g. "wing `work`".
        target: String,
        /// How many mining, checkpoint or repair jobs are queued, running or paused.
        count: usize,
    },

    /// A drawer was asked to be superseded but its validity already ended.
    #[error("drawer `{drawer}` was already superseded")]
    #[diagnostic(
        code(memcastle::palace::drawer_superseded),
        help(
            "supersede the drawer that replaced it instead, or search with `include_historical` to find the current one"
        )
    )]
    DrawerSuperseded {
        /// The drawer id.
        drawer: String,
    },

    /// A vector's length is not the one the palace's vector index accepts.
    #[error("an embedding has {actual} dimension(s) but the palace stores {expected}")]
    #[diagnostic(
        code(memcastle::embed::dimension_mismatch),
        help(
            "use a model that produces the stored dimension, or ask the provider for it (the `dimensions` option of OpenAI-compatible APIs); see docs/configuration.md"
        )
    )]
    EmbeddingDimension {
        /// The dimension the vector index declares.
        expected: usize,
        /// The length of the vector that was offered.
        actual: usize,
    },

    /// Something asked for embeddings while no provider is configured.
    #[error("no embedding provider is configured")]
    #[diagnostic(
        code(memcastle::embed::not_configured),
        help(
            "set `provider = \"command\"` or `\"http\"` in the `[embeddings]` section of the config file (see docs/configuration.md), or send vectors yourself with `PUT /api/drawers/{{id}}/embedding`"
        )
    )]
    EmbeddingsNotConfigured,

    /// The embedding provider was configured but did not return usable vectors.
    #[error("the embedding provider failed: {message}")]
    #[diagnostic(
        code(memcastle::embed::failed),
        help(
            "check the `[embeddings]` section of the config and that the provider is reachable; semantic search falls back to lexical until it works"
        )
    )]
    EmbeddingFailed {
        /// What the provider reported.
        message: String,
    },

    /// Something asked for entity extraction while no provider is configured.
    #[error("no extraction provider is configured")]
    #[diagnostic(
        code(memcastle::extract::not_configured),
        help(
            "set `provider = \"heuristic\"`, `\"command\"` or `\"http\"` in the `[extraction]` section of the config file (see docs/configuration.md); extraction is off by default"
        )
    )]
    ExtractionNotConfigured,

    /// The extraction provider was configured but did not return a usable answer.
    #[error("the extraction provider failed: {message}")]
    #[diagnostic(
        code(memcastle::extract::failed),
        help(
            "check the `[extraction]` section of the config and that the provider is reachable; the job can be retried and drawers already read are not read again"
        )
    )]
    ExtractionFailed {
        /// What the provider reported.
        message: String,
    },

    /// A search asked for vector ranking, but no vector could be produced.
    #[error("{ranking} ranking needs a query embedding, and none is available")]
    #[diagnostic(
        code(memcastle::search::semantic_unavailable),
        help(
            "configure an `[embeddings]` provider, send a `query_embedding` with the request, or search with `ranking: lexical` (the default `auto` falls back on its own)"
        )
    )]
    SemanticUnavailable {
        /// The requested ranking (`semantic` or `hybrid`).
        ranking: String,
    },

    /// A source's stored cursor is not one its adapter can continue from.
    #[error("the stored cursor of the `{provider}` source cannot be continued from: {message}")]
    #[diagnostic(
        code(memcastle::mining::cursor_invalid),
        help(
            "mine it again from the beginning (`memcastle mine --source <provider> --full`): unchanged documents are skipped, so nothing is duplicated"
        )
    )]
    SourceCursorInvalid {
        /// The adapter whose cursor was rejected.
        provider: String,
        /// Why it was rejected.
        message: String,
    },

    /// A source manifest (`memcastle-source.toml`) does not parse or breaks a rule.
    #[error("invalid source manifest: {message}")]
    #[diagnostic(
        code(memcastle::source::manifest_invalid),
        help(
            "fix `memcastle-source.toml`; docs/writing-sources.md lists every field and what it accepts"
        )
    )]
    SourceManifestInvalid {
        /// What was wrong.
        message: String,
    },

    /// A source package is not one MemCastle can read.
    #[error("invalid source package: {message}")]
    #[diagnostic(
        code(memcastle::source::package_invalid),
        help(
            "build a fresh package with `memcastle source package`: it holds `memcastle-source.toml` and `source.wasm`, and nothing else is read"
        )
    )]
    SourcePackageInvalid {
        /// What was wrong.
        message: String,
    },

    /// A source cannot run on this MemCastle: its contract or version requirement is not met, or its
    /// component does not fit the contract.
    #[error("source `{name}` is incompatible with this MemCastle: {reason}")]
    #[diagnostic(
        code(memcastle::source::incompatible),
        help(
            "rebuild the source against this MemCastle's contract (`memcastle source init` scaffolds the current one), or install a MemCastle version it supports"
        )
    )]
    SourceIncompatible {
        /// The source.
        name: String,
        /// Why it cannot run.
        reason: String,
    },

    /// No installed source has this name.
    #[error("no installed source is named `{name}`")]
    #[diagnostic(
        code(memcastle::source::not_found),
        help(
            "`memcastle source list` shows what is installed; `memcastle source install <package>` adds a source"
        )
    )]
    SourceNotFound {
        /// The name asked for.
        name: String,
    },

    /// The source exists but is not usable right now.
    #[error("source `{name}` is {state}")]
    #[diagnostic(
        code(memcastle::source::not_enabled),
        help(
            "`memcastle source list` says why; `memcastle source enable <name>` enables a disabled source"
        )
    )]
    SourceNotEnabled {
        /// The source.
        name: String,
        /// Its state, with the reason when it is unavailable.
        state: String,
    },

    /// Installing a source needs the user's explicit agreement to the permissions it asks for.
    #[error("source `{name}` asks for permissions that were not agreed to: {permissions}")]
    #[diagnostic(
        code(memcastle::source::consent_required),
        help(
            "review the permissions, then install again with `--yes` (or answer the prompt); an unattended install passes `--consent {digest}`"
        )
    )]
    SourceConsentRequired {
        /// The source.
        name: String,
        /// The permissions, one line.
        permissions: String,
        /// The digest agreeing to exactly these permissions.
        digest: String,
    },

    /// A built-in source cannot be disabled, replaced or removed.
    #[error("`{name}` is built into MemCastle")]
    #[diagnostic(
        code(memcastle::source::builtin),
        help(
            "built-in sources are always available; install a package under another name to change behaviour"
        )
    )]
    SourceBuiltin {
        /// The built-in's name.
        name: String,
    },

    /// A source ran and failed: it trapped, ran out of memory, or reported an error.
    #[error("source `{name}` failed: {message}")]
    #[diagnostic(
        code(memcastle::source::failed),
        help(
            "the message is the source's own; a source that needs a file, a program or the network must declare it under `[permissions]` and be installed with that agreed to"
        )
    )]
    SourceFailed {
        /// The source.
        name: String,
        /// What it said.
        message: String,
    },

    /// A source took longer than its time limit for one call.
    #[error("source `{name}` did not answer within {secs}s")]
    #[diagnostic(
        code(memcastle::source::timeout),
        help(
            "raise `mining.source_timeout_secs` (or the source's own `[limits] timeout_secs`, up to that ceiling)"
        )
    )]
    SourceTimeout {
        /// The source.
        name: String,
        /// The limit that was hit, in seconds.
        secs: u64,
    },

    /// A source asked for something its permissions do not grant.
    #[error("source `{name}` was denied: {message}")]
    #[diagnostic(
        code(memcastle::source::permission_denied),
        help(
            "add what it needs to `[permissions]` in `memcastle-source.toml`, rebuild, and install it again so the new permissions are agreed to"
        )
    )]
    SourcePermissionDenied {
        /// The source.
        name: String,
        /// What was refused.
        message: String,
    },

    /// Building a source did not produce a component.
    #[error("building the source failed: {message}")]
    #[diagnostic(
        code(memcastle::source::build_failed),
        help(
            "run the build command shown in `memcastle-source.toml` under `[build]` yourself to see its full output"
        )
    )]
    SourceBuildFailed {
        /// What went wrong.
        message: String,
    },

    /// A source registry's index could not be read: unreachable, not an index, or a format this MemCastle does not
    /// know.
    #[error("the source registry `{location}` cannot be used: {message}")]
    #[diagnostic(
        code(memcastle::source::registry_unavailable),
        help(
            "check the location under `mining.registries` (or `--registry`): it is an `https://` URL, a `file://` URL or a path to a `memcastle-index.json`"
        )
    )]
    SourceRegistryUnavailable {
        /// The index location as configured.
        location: String,
        /// What went wrong.
        message: String,
    },

    /// No registry offers the source, or none of its versions can be installed here.
    #[error("cannot install `{name}`: {reason}")]
    #[diagnostic(
        code(memcastle::source::not_in_registry),
        help(
            "`memcastle source search` lists what the bundled and configured registries offer; a local package installs with `memcastle source install <file>`"
        )
    )]
    SourceNotInRegistry {
        /// The source asked for.
        name: String,
        /// Why nothing qualifies.
        reason: String,
    },

    /// A downloaded package is not the one the registry published.
    #[error("the package for `{name}` failed its integrity check: {message}")]
    #[diagnostic(
        code(memcastle::source::integrity),
        help(
            "nothing was installed; retry in case the download was cut short, and if it persists the registry or the connection is serving something other than what it published"
        )
    )]
    SourceIntegrity {
        /// The source.
        name: String,
        /// What did not match.
        message: String,
    },

    /// A package's signature does not satisfy the trust policy.
    #[error("the package for `{name}` is not trusted: {message}")]
    #[diagnostic(
        code(memcastle::source::untrusted),
        help(
            "add the publisher's public key to `mining.trusted_keys` if you trust them, or set `mining.trust = \"optional\"` to allow unsigned packages"
        )
    )]
    SourceUntrusted {
        /// The source.
        name: String,
        /// Why the policy refuses it.
        message: String,
    },

    /// A signing key or signature could not be made or read.
    #[error("signing failed: {message}")]
    #[diagnostic(
        code(memcastle::source::signing_failed),
        help(
            "`memcastle source keygen <file>` writes a new signing key; a key file holds one base64-encoded 32-byte seed"
        )
    )]
    SourceSigning {
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

    /// Build an [`Error::InvalidPalacePath`].
    pub fn invalid_palace_path(raw: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidPalacePath {
            raw: raw.into(),
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

    /// Build an [`Error::Aborted`].
    pub fn aborted(action: impl Into<String>) -> Self {
        Self::Aborted {
            action: action.into(),
        }
    }

    /// Build an [`Error::PromptFailed`].
    pub fn prompt_failed(message: impl Into<String>) -> Self {
        Self::PromptFailed {
            message: message.into(),
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

    /// Build an [`Error::JobContended`].
    pub fn job_contended(id: impl Into<String>) -> Self {
        Self::JobContended { id: id.into() }
    }

    /// Build an [`Error::EntropyUnavailable`].
    pub fn entropy_unavailable(message: impl ToString) -> Self {
        Self::EntropyUnavailable {
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

    /// Build an [`Error::AssetsNotFound`].
    pub fn assets_not_found(path: impl Into<String>) -> Self {
        Self::AssetsNotFound { path: path.into() }
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
            from: err.from.to_string(),
            event: err.event.to_string(),
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
            Error::not_implemented("memcastle maintenance"),
            Error::aborted("cancelling a job"),
            Error::prompt_failed("not a terminal"),
            Error::ProjectInvalid {
                message: "/p/.config/memcastle.toml is not valid".to_string(),
            },
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
            Error::EntityNotFound {
                id: "x".to_string(),
            },
            Error::InvalidJobTransition {
                id: "x".to_string(),
                from: "queued".to_string(),
                event: "pause".to_string(),
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
            Error::DaemonAlreadyRunning {
                addr: "127.0.0.1:8420".to_string(),
            },
            Error::JobOrphaned {
                id: "x".to_string(),
            },
            Error::LeaseLost {
                id: "x".to_string(),
            },
            Error::job_contended("x"),
            Error::entropy_unavailable("no source"),
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
            Error::assets_not_found("/nowhere"),
            Error::Unauthorized { reason: "missing" },
            Error::AuthNotConfigured,
            Error::DbEndpointUnsafe {
                reason: "not loopback".to_string(),
            },
            Error::DbEndpointRunning {
                addr: "127.0.0.1:8000".to_string(),
            },
            Error::DbEndpointBind {
                addr: std::net::SocketAddr::from(([127, 0, 0, 1], 8000)),
                source: std::io::Error::from(std::io::ErrorKind::AddrInUse),
            },
            Error::DbEndpointUnavailable {
                backend: "remote".to_string(),
            },
            Error::WingNotFound {
                wing: "work".to_string(),
            },
            Error::RoomNotFound {
                wing: "work".to_string(),
                room: "x".to_string(),
            },
            Error::DrawerNotFound {
                room: "work/x".to_string(),
                drawer: "y".to_string(),
            },
            Error::InvalidPalacePath {
                raw: "a//b".to_string(),
                message: "empty segment".to_string(),
            },
            Error::DrawerNameTaken {
                room: "work/x".to_string(),
                name: "y".to_string(),
            },
            Error::PalaceBusy {
                target: "wing `work`".to_string(),
                count: 1,
            },
            Error::DrawerSuperseded {
                drawer: "y".to_string(),
            },
            Error::EmbeddingDimension {
                expected: 768,
                actual: 2,
            },
            Error::EmbeddingsNotConfigured,
            Error::EmbeddingFailed {
                message: "timeout".to_string(),
            },
            Error::ExtractionNotConfigured,
            Error::ExtractionFailed {
                message: "timeout".to_string(),
            },
            Error::SemanticUnavailable {
                ranking: "hybrid".to_string(),
            },
            Error::SourceCursorInvalid {
                provider: "directory".to_string(),
                message: "not an object".to_string(),
            },
            Error::SourceManifestInvalid {
                message: "no name".to_string(),
            },
            Error::SourcePackageInvalid {
                message: "no component".to_string(),
            },
            Error::SourceIncompatible {
                name: "slack".to_string(),
                reason: "contract 9.0".to_string(),
            },
            Error::SourceNotFound {
                name: "slack".to_string(),
            },
            Error::SourceNotEnabled {
                name: "slack".to_string(),
                state: "disabled".to_string(),
            },
            Error::SourceConsentRequired {
                name: "slack".to_string(),
                permissions: "network".to_string(),
                digest: "abc".to_string(),
            },
            Error::SourceBuiltin {
                name: "directory".to_string(),
            },
            Error::SourceFailed {
                name: "slack".to_string(),
                message: "trap".to_string(),
            },
            Error::SourceTimeout {
                name: "slack".to_string(),
                secs: 30,
            },
            Error::SourcePermissionDenied {
                name: "slack".to_string(),
                message: "git".to_string(),
            },
            Error::SourceBuildFailed {
                message: "no target".to_string(),
            },
            Error::SourceRegistryUnavailable {
                location: "https://example.test/index.json".to_string(),
                message: "unreachable".to_string(),
            },
            Error::SourceNotInRegistry {
                name: "slack".to_string(),
                reason: "no registry offers it".to_string(),
            },
            Error::SourceIntegrity {
                name: "slack".to_string(),
                message: "digest mismatch".to_string(),
            },
            Error::SourceUntrusted {
                name: "slack".to_string(),
                message: "unsigned".to_string(),
            },
            Error::SourceSigning {
                message: "bad key".to_string(),
            },
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
            | Error::Aborted { .. }
            | Error::PromptFailed { .. }
            | Error::ProjectInvalid { .. }
            | Error::MigrationsPending { .. }
            | Error::Store { .. }
            | Error::StoreMalformed { .. }
            | Error::SchemaSync { .. }
            | Error::EmptyLabel { .. }
            | Error::Serialization { .. }
            | Error::RelationshipNotFound { .. }
            | Error::EntityNotFound { .. }
            | Error::InvalidJobTransition { .. }
            | Error::JobNotFound { .. }
            | Error::InvalidBasedOnJob { .. }
            | Error::Client { .. }
            | Error::Remote { .. }
            | Error::DaemonNotRunning
            | Error::DaemonAlreadyRunning { .. }
            | Error::JobOrphaned { .. }
            | Error::LeaseLost { .. }
            | Error::JobContended { .. }
            | Error::EntropyUnavailable { .. }
            | Error::Server { .. }
            | Error::ServerBind { .. }
            | Error::ModeForbidden { .. }
            | Error::MigrationLocked { .. }
            | Error::MigrationFailed { .. }
            | Error::AssetsNotFound { .. }
            | Error::Unauthorized { .. }
            | Error::AuthNotConfigured
            | Error::DbEndpointUnsafe { .. }
            | Error::DbEndpointRunning { .. }
            | Error::DbEndpointBind { .. }
            | Error::DbEndpointUnavailable { .. }
            | Error::WingNotFound { .. }
            | Error::RoomNotFound { .. }
            | Error::DrawerNotFound { .. }
            | Error::InvalidPalacePath { .. }
            | Error::DrawerNameTaken { .. }
            | Error::PalaceBusy { .. }
            | Error::DrawerSuperseded { .. }
            | Error::EmbeddingDimension { .. }
            | Error::EmbeddingsNotConfigured
            | Error::EmbeddingFailed { .. }
            | Error::ExtractionNotConfigured
            | Error::ExtractionFailed { .. }
            | Error::SemanticUnavailable { .. }
            | Error::SourceCursorInvalid { .. }
            | Error::SourceManifestInvalid { .. }
            | Error::SourcePackageInvalid { .. }
            | Error::SourceIncompatible { .. }
            | Error::SourceNotFound { .. }
            | Error::SourceNotEnabled { .. }
            | Error::SourceConsentRequired { .. }
            | Error::SourceBuiltin { .. }
            | Error::SourceFailed { .. }
            | Error::SourceTimeout { .. }
            | Error::SourcePermissionDenied { .. }
            | Error::SourceBuildFailed { .. }
            | Error::SourceRegistryUnavailable { .. }
            | Error::SourceNotInRegistry { .. }
            | Error::SourceIntegrity { .. }
            | Error::SourceUntrusted { .. }
            | Error::SourceSigning { .. } => {}
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
