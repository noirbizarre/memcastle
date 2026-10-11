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
/// `<module>` names the part of the system the user is dealing with, never a layer of the implementation, and
/// `<kind>` says what is wrong with it: a condition (`invalid`, `malformed`,
/// `not_found`, `locked`, `pending`, `forbidden`-style words), or `failed` /
/// `<thing>_failed` when an operation itself broke. A condition on a named thing puts the thing first
/// (`id_invalid`, `manifest_invalid`), never `invalid_<thing>`. The
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
        code(memcastle::jobs::id_invalid),
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
        code(memcastle::graph::empty_label),
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
        code(memcastle::jobs::transition_invalid),
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
        code(memcastle::repair::based_on_job_invalid),
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
        code(memcastle::mode::forbidden),
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

    /// No drawer answers to the given name or id: in a room when the lookup was by name or by a path, anywhere in
    /// the palace when it was by id alone.
    #[error("drawer `{drawer}` not found{}", drawer_scope(.room.as_deref()))]
    #[diagnostic(
        code(memcastle::palace::drawer_not_found),
        help("{}", drawer_not_found_help(room.as_deref()))
    )]
    DrawerNotFound {
        /// The `wing/room` that was searched, or `None` for a lookup by id, which is not scoped to a room.
        room: Option<String>,
        /// The name or id that was looked up.
        drawer: String,
    },

    /// A `wing/room/drawer` path, or one of its names, is not usable.
    #[error("invalid path `{raw}`: {message}")]
    #[diagnostic(
        code(memcastle::palace::path_invalid),
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
    #[error("the stored cursor of the `{adapter}` source cannot be continued from: {message}")]
    #[diagnostic(
        code(memcastle::source::cursor_invalid),
        help(
            "mine it again from the beginning (`memcastle mine <name> --full`): unchanged documents are skipped, so nothing is duplicated"
        )
    )]
    SourceCursorInvalid {
        /// The adapter whose cursor was rejected.
        // Not `source`: `thiserror` reads a field of that name as the error's cause, which a name is not.
        adapter: String,
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

    /// A plugin manifest cannot be safely understood.
    #[error("invalid plugin manifest: {message}")]
    #[diagnostic(
        code(memcastle::plugin::manifest_invalid),
        help(
            "fix `plugin.toml`; module IDs, entry points and compatibility must match the included artifacts"
        )
    )]
    PluginManifestInvalid {
        /// What was wrong.
        message: String,
    },

    /// A plugin archive is incomplete or contains unexpected files.
    #[error("invalid plugin package: {message}")]
    #[diagnostic(
        code(memcastle::plugin::package_invalid),
        help("rebuild the plugin archive from its manifest and declared module artifacts")
    )]
    PluginPackageInvalid {
        /// What was wrong.
        message: String,
    },

    /// An installed plugin has live consumers and cannot be changed safely.
    #[error("plugin `{name}` cannot be changed: {reason}")]
    #[diagnostic(
        code(memcastle::plugin::blocked),
        help(
            "disable the affected source, remove its miner/trigger configuration or uninstall its integration, then retry"
        )
    )]
    PluginBlocked {
        /// Stable plugin ID.
        name: String,
        /// The specific reference that blocks this operation.
        reason: String,
    },

    /// No installed plugin has this ID.
    #[error("no installed plugin is named `{name}`")]
    #[diagnostic(
        code(memcastle::plugin::not_found),
        help(
            "`memcastle plugin list` shows installed plugins; install the plugin before changing its modules"
        )
    )]
    PluginNotFound {
        /// Stable plugin ID asked for.
        name: String,
    },

    /// A configured plugin catalogue cannot be used.
    #[error("plugin registry `{location}` is unavailable: {reason}")]
    #[diagnostic(
        code(memcastle::plugin::registry_unavailable),
        help(
            "check the plugin registry URL or path, then retry; the legacy source registry is separate"
        )
    )]
    PluginRegistryUnavailable {
        /// Catalogue location.
        location: String,
        /// Why it could not be used.
        reason: String,
    },

    /// No configured plugin catalogue supplies the requested compatible release.
    #[error("plugin `{name}` is not available: {reason}")]
    #[diagnostic(
        code(memcastle::plugin::not_in_registry),
        help(
            "check the plugin ID/version with `memcastle plugin search`, or choose another registry"
        )
    )]
    PluginNotInRegistry {
        /// Requested plugin ID.
        name: String,
        /// Missing version or lack of compatible release.
        reason: String,
    },

    /// Downloaded plugin bytes or inventory disagree with the published release.
    #[error("plugin `{name}` failed integrity verification: {reason}")]
    #[diagnostic(
        code(memcastle::plugin::integrity),
        help(
            "do not install this release; ask the publisher to correct the asset or reviewed catalogue"
        )
    )]
    PluginIntegrity {
        /// The plugin that failed verification.
        name: String,
        /// Digest, signature or identity mismatch.
        reason: String,
    },

    /// The registry release does not satisfy the plugin trust policy.
    #[error("plugin `{name}` is not trusted: {reason}")]
    #[diagnostic(
        code(memcastle::plugin::untrusted),
        help(
            "choose a signed release from a trusted publisher or adjust plugins.trust and plugins.trusted_keys"
        )
    )]
    PluginUntrusted {
        /// The package rejected by the trust policy.
        name: String,
        /// Missing or invalid signature.
        reason: String,
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

    /// No miner has this name.
    #[error("no miner is named `{name}`")]
    #[diagnostic(
        code(memcastle::miner::not_found),
        help(
            "`memcastle miner list` shows the configured miners; `memcastle miner set <name> --source <source>` adds one"
        )
    )]
    MinerNotFound {
        /// The name asked for.
        name: String,
    },

    /// A miner with this name already exists, and the request was to create it.
    #[error("a miner named `{name}` already exists")]
    #[diagnostic(
        code(memcastle::miner::exists),
        help("`memcastle miner get {name}` shows it; `memcastle miner set {name} ...` changes it")
    )]
    MinerExists {
        /// The name that is taken.
        name: String,
    },

    /// A miner's definition is not valid, so it was not saved or activated.
    #[error("miner `{name}` is not valid: {reason}")]
    #[diagnostic(
        code(memcastle::miner::invalid),
        help(
            "fix the setting named above; `docs/configuration.md` describes every `[[miners]]` key"
        )
    )]
    MinerInvalid {
        /// The miner.
        name: String,
        /// What is wrong and how to fix it.
        reason: String,
    },

    /// A change to a miner's options may select more material than before.
    #[error("changing miner `{name}` may broaden what it reads: {reasons}")]
    #[diagnostic(
        code(memcastle::miner::scope_broadened),
        help(
            "a wider set of options may send more into the palace; if that is intended, repeat with `--allow-broaden`"
        )
    )]
    MinerScopeBroadened {
        /// The miner.
        name: String,
        /// What gets wider, one line.
        reasons: String,
    },

    /// The miner is disabled, so it was not run.
    #[error("miner `{name}` is disabled")]
    #[diagnostic(
        code(memcastle::miner::disabled),
        help("`memcastle miner enable {name}` enables it")
    )]
    MinerDisabled {
        /// The miner.
        name: String,
    },

    /// The miner cannot be run as configured.
    #[error("miner `{name}` cannot be run: {reason}")]
    #[diagnostic(
        code(memcastle::miner::not_runnable),
        help(
            "`memcastle miner get {name}` shows its state; fix what it names, or mine by hand with `memcastle mine <source>`"
        )
    )]
    MinerNotRunnable {
        /// The miner.
        name: String,
        /// Why not.
        reason: String,
    },

    /// The configuration file holding the miners cannot be read or written.
    #[error("the miner configuration in `{path}` cannot be used: {reason}")]
    #[diagnostic(
        code(memcastle::miner::config_file),
        help(
            "fix the file by hand, then `memcastle miner reload`; the daemon keeps the last miners it could read"
        )
    )]
    MinerConfigFile {
        /// The configuration file.
        path: String,
        /// What went wrong.
        reason: String,
    },

    /// No trigger has this name.
    #[error("no trigger is named `{name}`")]
    #[diagnostic(
        code(memcastle::trigger::not_found),
        help(
            "`memcastle trigger list` shows the configured triggers; `memcastle trigger set <name> --miner <miner> --type <type>` adds one"
        )
    )]
    TriggerNotFound {
        /// The name asked for.
        name: String,
    },

    /// A trigger with this name already exists, and the request was to create it.
    #[error("a trigger named `{name}` already exists")]
    #[diagnostic(
        code(memcastle::trigger::exists),
        help(
            "`memcastle trigger get {name}` shows it; `memcastle trigger set {name} ...` changes it"
        )
    )]
    TriggerExists {
        /// The name that is taken.
        name: String,
    },

    /// A trigger's definition is not valid, so it was not saved or activated.
    #[error("trigger `{name}` is not valid: {reason}")]
    #[diagnostic(
        code(memcastle::trigger::invalid),
        help("fix the setting named above; `docs/triggers.md` describes every `[[triggers]]` key")
    )]
    TriggerInvalid {
        /// The trigger.
        name: String,
        /// What is wrong and how to fix it.
        reason: String,
    },

    /// A trigger cannot be switched on yet: something it needs is not set up.
    #[error("trigger `{name}` cannot be enabled yet: {reason}")]
    #[diagnostic(
        code(memcastle::trigger::not_activatable),
        help(
            "nothing is started until every prerequisite holds; `docs/triggers.md` lists what each kind needs, and `memcastle trigger get {name}` shows where this one stands"
        )
    )]
    TriggerNotActivatable {
        /// The trigger.
        name: String,
        /// What is missing, and how to set it up.
        reason: String,
    },

    /// The trigger is disabled, so it was not fired.
    #[error("trigger `{name}` is disabled")]
    #[diagnostic(
        code(memcastle::trigger::disabled),
        help(
            "`memcastle trigger enable {name}` enables it; to mine now without a trigger, `memcastle miner run <miner>`"
        )
    )]
    TriggerDisabled {
        /// The trigger.
        name: String,
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

    /// A source that ships with MemCastle was asked to be installed from a registry, updated or removed.
    #[error("`{name}` ships with MemCastle and cannot be {action}")]
    #[diagnostic(
        code(memcastle::source::bundled),
        help(
            "it is already installed: `memcastle source enable {name}` or `memcastle source disable {name}` is all it takes, and it is updated with MemCastle; to run a different build, install a package file under that name"
        )
    )]
    SourceBundled {
        /// The bundled source's name.
        name: String,
        /// What was asked, as a past participle: `removed`, `updated`, `installed from a registry`.
        action: String,
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

    /// A source needs an OAuth sign-in that has not been done, or that the provider no longer honours.
    // The command is in the message and not only in the help: a failed job records the message alone, and the job is
    // where a person finds out that a run did not happen.
    #[error(
        "source `{source_name}` is not signed in: {reason}; sign in with `memcastle source auth {source_name}`"
    )]
    #[diagnostic(
        code(memcastle::credential::required),
        help("sign in with `memcastle source auth {source_name}`, then run the miner again")
    )]
    CredentialRequired {
        /// The source.
        source_name: String,
        /// Why there is no usable credential: never signed in, signed in under other terms, or revoked.
        reason: String,
    },

    /// A stored credential could not be renewed, and is kept so that a later attempt can.
    #[error("the credential for source `{source_name}` could not be refreshed: {message}")]
    #[diagnostic(
        code(memcastle::credential::refresh_failed),
        help(
            "this is usually the provider or the network being unreachable, so run the miner again later; if it persists, sign in again with `memcastle source auth {source_name}`"
        )
    )]
    CredentialRefreshFailed {
        /// The source.
        source_name: String,
        /// What the provider or the network said.
        message: String,
    },

    /// Signing in did not complete: the user declined, the code expired, or the provider refused.
    #[error("signing in for source `{source_name}` failed: {message}")]
    #[diagnostic(
        code(memcastle::credential::flow_failed),
        help(
            "run `memcastle source auth {source_name}` again and finish signing in before the code expires"
        )
    )]
    CredentialFlowFailed {
        /// The source.
        source_name: String,
        /// What went wrong.
        message: String,
    },

    /// The source does not sign in with OAuth, so there is nothing to authenticate.
    #[error("source `{source_name}` does not use OAuth")]
    #[diagnostic(
        code(memcastle::credential::oauth_unsupported),
        help(
            "only a source whose manifest declares `[permissions.oauth]` signs in this way; `memcastle source show {source_name}` lists what it asks for, and a static token is a miner's `--credential-env` or `--credential-file`"
        )
    )]
    CredentialOauthUnsupported {
        /// The source.
        source_name: String,
    },

    /// The place credentials are kept could not be read or written.
    #[error("the credential store failed: {message}")]
    #[diagnostic(
        code(memcastle::credential::store_failed),
        help(
            "no platform keyring was usable and the fallback file could not be used either; check the permissions of the credentials directory shown, or set `credentials.backend = \"file\"` and a writable `credentials.dir`"
        )
    )]
    CredentialStoreFailed {
        /// What went wrong.
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
            "`memcastle source search` lists what the configured registries offer; a local package installs with `memcastle source install <file>`"
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

    /// An integration manifest is malformed or breaks a rule.
    #[error("invalid integration manifest: {message}")]
    #[diagnostic(
        code(memcastle::integration::manifest_invalid),
        help(
            "fix `memcastle-integration.toml`; docs/integrations.md lists every key, and a development checkout needs the file next to the integration's sources"
        )
    )]
    IntegrationManifestInvalid {
        /// What is wrong.
        message: String,
    },

    /// No shipped integration has this name.
    #[error("no integration named `{name}` is shipped in {root}")]
    #[diagnostic(
        code(memcastle::integration::not_found),
        help(
            "`memcastle integration list` shows what this installation ships; point `--assets-dir` at a checkout or a package layout to look somewhere else"
        )
    )]
    IntegrationNotFound {
        /// The name asked for.
        name: String,
        /// Where it was looked for.
        root: String,
    },

    /// There is no assets directory to read integrations from.
    #[error("no assets directory carries integrations: {message}")]
    #[diagnostic(
        code(memcastle::integration::assets_missing),
        help(
            "install MemCastle from a package that ships integrations, or run from a checkout with `--assets-dir <checkout>` after `mise run integrations:build`"
        )
    )]
    IntegrationAssetsMissing {
        /// What was missing.
        message: String,
    },

    /// This MemCastle or this agent cannot take the integration.
    #[error("integration `{name}` cannot be installed: {reason}")]
    #[diagnostic(
        code(memcastle::integration::incompatible),
        help(
            "upgrade MemCastle or the agent, or install the integration version shipped with the one you have; nothing was changed"
        )
    )]
    IntegrationIncompatible {
        /// The integration.
        name: String,
        /// Which requirement fails.
        reason: String,
    },

    /// The agent the integration is for cannot be found or run.
    #[error("the agent for integration `{name}` was not found: {message}")]
    #[diagnostic(
        code(memcastle::integration::agent_not_found),
        help(
            "install the agent and make sure its command is on PATH, then retry; nothing was changed"
        )
    )]
    IntegrationAgentNotFound {
        /// The integration.
        name: String,
        /// What was tried.
        message: String,
    },

    /// A file the integration would write belongs to the user.
    #[error("integration `{name}` would overwrite {path}, which MemCastle did not write")]
    #[diagnostic(
        code(memcastle::integration::conflict),
        help(
            "move or delete that file if you want the integration there, then retry; nothing was changed"
        )
    )]
    IntegrationConflict {
        /// The integration.
        name: String,
        /// The file in the way.
        path: String,
    },

    /// The agent refused, or failed, to register the integration.
    #[error("registering integration `{name}` with its agent failed: {message}")]
    #[diagnostic(
        code(memcastle::integration::registration_failed),
        help(
            "the files MemCastle copied were removed again; run the agent's own command by hand to see why it refuses"
        )
    )]
    IntegrationRegistrationFailed {
        /// The integration.
        name: String,
        /// What the agent said.
        message: String,
    },

    /// The installed integration does not look right after installation.
    #[error("integration `{name}` did not validate after installation: {message}")]
    #[diagnostic(
        code(memcastle::integration::validation_failed),
        help(
            "`memcastle integration remove <name>` undoes the installation; then check that the package is complete (`memcastle integration list`) and retry"
        )
    )]
    IntegrationValidationFailed {
        /// The integration.
        name: String,
        /// What failed.
        message: String,
    },

    /// The integration is not installed.
    #[error("integration `{name}` is not installed")]
    #[diagnostic(
        code(memcastle::integration::not_installed),
        help(
            "`memcastle integration install {name}` installs it; `memcastle integration list` shows the state of each one"
        )
    )]
    IntegrationNotInstalled {
        /// The integration.
        name: String,
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

    /// Parse a drawer id a caller supplied, raising [`Error::InvalidInput`] naming `field`, the argument that held it
    /// (`id` in a REST path, `drawer_id` in an MCP tool). Shared by REST and MCP so the same mistake reads the same.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `raw` is not a drawer id.
    pub fn parse_drawer_id(field: &str, raw: &str) -> Result<crate::domain::DrawerId> {
        raw.parse()
            .map_err(|_| Self::invalid_input(field, format!("`{raw}` is not a drawer id (a UUID)")))
    }

    /// Parse an entity id a caller supplied, raising [`Error::InvalidInput`] naming `field`.
    /// The entity counterpart of [`Error::parse_drawer_id`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `raw` is not an entity id.
    pub fn parse_entity_id(field: &str, raw: &str) -> Result<crate::domain::EntityId> {
        raw.parse().map_err(|_| {
            Self::invalid_input(field, format!("`{raw}` is not an entity id (a UUID)"))
        })
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

/// Where a missing drawer was looked for, as the tail of its message: a room, or nothing for an id, which names no room
/// and would otherwise be reported as missing from a room the caller never mentioned.
fn drawer_scope(room: Option<&str>) -> String {
    room.map_or_else(String::new, |room| format!(" in `{room}`"))
}

/// What to do about a missing drawer: list a room's drawers when the lookup was in one, and otherwise check the id,
/// because listing a room cannot help someone who asked by id alone.
fn drawer_not_found_help(room: Option<&str>) -> &'static str {
    if room.is_some() {
        "list the room's drawers with `memcastle drawer list --room <wing>/<room>`"
    } else {
        "check the drawer id: it is printed by `memcastle search` and by `memcastle drawer list`"
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
                room: Some("work/x".to_string()),
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
                adapter: "directory".to_string(),
                message: "not an object".to_string(),
            },
            Error::SourceManifestInvalid {
                message: "no name".to_string(),
            },
            Error::SourcePackageInvalid {
                message: "no component".to_string(),
            },
            Error::PluginManifestInvalid {
                message: "no plugin ID".to_string(),
            },
            Error::PluginPackageInvalid {
                message: "missing artifact".to_string(),
            },
            Error::PluginBlocked {
                name: "acme".to_string(),
                reason: "source enabled".to_string(),
            },
            Error::PluginNotFound {
                name: "acme".to_string(),
            },
            Error::PluginRegistryUnavailable {
                location: "https://example.invalid/plugins.json".into(),
                reason: "unreachable".into(),
            },
            Error::PluginNotInRegistry {
                name: "acme".into(),
                reason: "not listed".into(),
            },
            Error::PluginIntegrity {
                name: "acme".into(),
                reason: "digest mismatch".into(),
            },
            Error::PluginUntrusted {
                name: "acme".into(),
                reason: "missing signature".into(),
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
            Error::MinerNotFound {
                name: "signal".to_string(),
            },
            Error::MinerExists {
                name: "signal".to_string(),
            },
            Error::MinerInvalid {
                name: "signal".to_string(),
                reason: "`source` is empty".to_string(),
            },
            Error::MinerScopeBroadened {
                name: "signal".to_string(),
                reasons: "`groups` is no longer filtered".to_string(),
            },
            Error::MinerDisabled {
                name: "signal".to_string(),
            },
            Error::MinerNotRunnable {
                name: "signal".to_string(),
                reason: "no locator".to_string(),
            },
            Error::MinerConfigFile {
                path: "/etc/memcastle.toml".to_string(),
                reason: "not valid TOML".to_string(),
            },
            Error::TriggerNotFound {
                name: "daily".to_string(),
            },
            Error::TriggerExists {
                name: "daily".to_string(),
            },
            Error::TriggerInvalid {
                name: "daily".to_string(),
                reason: "`every` is missing".to_string(),
            },
            Error::TriggerNotActivatable {
                name: "daily".to_string(),
                reason: "the webhook listener is off".to_string(),
            },
            Error::TriggerDisabled {
                name: "daily".to_string(),
            },
            Error::SourceConsentRequired {
                name: "slack".to_string(),
                permissions: "network".to_string(),
                digest: "abc".to_string(),
            },
            Error::SourceBuiltin {
                name: "directory".to_string(),
            },
            Error::SourceBundled {
                name: "pi".to_string(),
                action: "removed".to_string(),
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
            Error::CredentialRequired {
                source_name: "slack".to_string(),
                reason: "never signed in".to_string(),
            },
            Error::CredentialRefreshFailed {
                source_name: "slack".to_string(),
                message: "timeout".to_string(),
            },
            Error::CredentialFlowFailed {
                source_name: "slack".to_string(),
                message: "denied".to_string(),
            },
            Error::CredentialOauthUnsupported {
                source_name: "slack".to_string(),
            },
            Error::CredentialStoreFailed {
                message: "read-only".to_string(),
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
            Error::IntegrationManifestInvalid {
                message: "no id".to_string(),
            },
            Error::IntegrationNotFound {
                name: "pi".to_string(),
                root: "/usr/share/memcastle".to_string(),
            },
            Error::IntegrationAssetsMissing {
                message: "no directory".to_string(),
            },
            Error::IntegrationIncompatible {
                name: "pi".to_string(),
                reason: "needs MemCastle 9".to_string(),
            },
            Error::IntegrationAgentNotFound {
                name: "pi".to_string(),
                message: "no `pi` on PATH".to_string(),
            },
            Error::IntegrationConflict {
                name: "opencode".to_string(),
                path: "/x/memcastle.ts".to_string(),
            },
            Error::IntegrationRegistrationFailed {
                name: "pi".to_string(),
                message: "exit 1".to_string(),
            },
            Error::IntegrationValidationFailed {
                name: "pi".to_string(),
                message: "entry missing".to_string(),
            },
            Error::IntegrationNotInstalled {
                name: "pi".to_string(),
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
            | Error::PluginManifestInvalid { .. }
            | Error::PluginPackageInvalid { .. }
            | Error::PluginBlocked { .. }
            | Error::PluginNotFound { .. }
            | Error::PluginRegistryUnavailable { .. }
            | Error::PluginNotInRegistry { .. }
            | Error::PluginIntegrity { .. }
            | Error::PluginUntrusted { .. }
            | Error::SourceIncompatible { .. }
            | Error::SourceNotFound { .. }
            | Error::SourceNotEnabled { .. }
            | Error::MinerNotFound { .. }
            | Error::MinerExists { .. }
            | Error::MinerInvalid { .. }
            | Error::MinerScopeBroadened { .. }
            | Error::MinerDisabled { .. }
            | Error::MinerNotRunnable { .. }
            | Error::MinerConfigFile { .. }
            | Error::TriggerNotFound { .. }
            | Error::TriggerExists { .. }
            | Error::TriggerInvalid { .. }
            | Error::TriggerNotActivatable { .. }
            | Error::TriggerDisabled { .. }
            | Error::SourceConsentRequired { .. }
            | Error::SourceBuiltin { .. }
            | Error::SourceBundled { .. }
            | Error::SourceFailed { .. }
            | Error::SourceTimeout { .. }
            | Error::SourcePermissionDenied { .. }
            | Error::CredentialRequired { .. }
            | Error::CredentialRefreshFailed { .. }
            | Error::CredentialFlowFailed { .. }
            | Error::CredentialOauthUnsupported { .. }
            | Error::CredentialStoreFailed { .. }
            | Error::SourceBuildFailed { .. }
            | Error::SourceRegistryUnavailable { .. }
            | Error::SourceNotInRegistry { .. }
            | Error::SourceIntegrity { .. }
            | Error::SourceUntrusted { .. }
            | Error::SourceSigning { .. }
            | Error::IntegrationManifestInvalid { .. }
            | Error::IntegrationNotFound { .. }
            | Error::IntegrationAssetsMissing { .. }
            | Error::IntegrationIncompatible { .. }
            | Error::IntegrationAgentNotFound { .. }
            | Error::IntegrationConflict { .. }
            | Error::IntegrationRegistrationFailed { .. }
            | Error::IntegrationValidationFailed { .. }
            | Error::IntegrationNotInstalled { .. } => {}
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
        // `<module>` is what the user is dealing with, never a layer of the implementation: a code that named
        // `app` or `domain` would say where MemCastle noticed the problem, which no user can act on.
        assert!(
            !["app", "domain", "mining"].contains(&segments[1]),
            "`{code}` names an internal layer; use the part of the system the user is dealing with"
        );
        // One word order for a condition, `<noun>_<condition>` (`id_invalid`, `manifest_invalid`), so that a grep
        // for `_invalid` finds them all and a new code does not pick a third spelling.
        assert!(
            !segments[2].starts_with("invalid_"),
            "`{code}` puts the condition first; write `<noun>_invalid`"
        );
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
    fn a_drawer_missing_by_id_names_no_room_and_a_drawer_missing_in_a_room_names_it() {
        let by_id = Error::DrawerNotFound {
            room: None,
            drawer: "abc".to_string(),
        }
        .body();
        let in_room = Error::DrawerNotFound {
            room: Some("work/x".to_string()),
            drawer: "abc".to_string(),
        }
        .body();

        // The same public code, so nothing that matches on it breaks.
        assert_eq!(by_id.code, in_room.code);
        assert_eq!(by_id.error, "drawer `abc` not found");
        assert_eq!(in_room.error, "drawer `abc` not found in `work/x`");
        // Listing a room cannot help someone who asked by id alone.
        assert!(!by_id.help.unwrap().contains("<wing>/<room>"));
        assert!(in_room.help.unwrap().contains("--room <wing>/<room>"));
    }

    #[test]
    fn an_error_body_carries_the_message_the_code_and_the_help() {
        let body = Error::invalid_job_id("nope").body();

        assert!(body.error.contains("nope"));
        assert_eq!(body.code.as_deref(), Some("memcastle::jobs::id_invalid"));
        assert!(body.help.is_some());
    }
}
