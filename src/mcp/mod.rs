//! MCP integration: the daemon's tool surface, exposed over HTTP.
//!
//! Deliberately HTTP-only for this bootstrap — `rmcp`'s streamable-HTTP
//! server transport, mounted on the same axum router as the REST API, is
//! natively multi-client, which is the actual architectural goal ("N agents
//! share one daemon"). A stdio bridge for MCP clients that only support
//! spawning a local subprocess is real future work (see the architecture
//! doc's non-goals list), not something this module needs to pre-guess the
//! shape of: tool logic below never touches a transport type, so adding one
//! later is additive.
//!
//! Every tool calls `AppServices` only — never `store` or `jobs` directly,
//! same rule as `api` (see that module's doc comment).

use std::sync::{Arc, Mutex};

use axum::http;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use tokio_util::sync::CancellationToken;

use crate::app::{AppServices, WakeUpBudget};
use crate::domain::default_dry_run; // one default for REST and MCP, so repair's dry-run-first cannot drift
use crate::domain::{CheckpointPayload, Job, MemoryMode, MiningSource};
use crate::error::Error;

/// The MCP tool surface. Cheap to clone (holds only `AppServices`, itself
/// cheap to clone, one small shared slot, and the macro-generated router).
///
/// `tool_router` looks unread to a naive dead-code scan — `#[tool_handler]`
/// wires it into `call_tool`/`list_tools` through macro-generated code, the
/// same pattern (and the same warning) as the SDK's own examples.
///
/// `mode` remembers the [`MemoryMode`] one MCP session chose — the MCP
/// surface has no per-request header the way HTTP does (see
/// `api::ModeHeader`), so mode is instead negotiated once, via
/// `memcastle_set_mode`, and applied to every later tool call *on the same
/// session*, identified by the `mcp-session-id` header
/// `StreamableHttpService` assigns.
///
/// rmcp builds one `McpTools` per session (the service factory in
/// [`service`] runs once per `initialize`) and drops it when the session
/// ends, so this is a single slot tagged with the session it belongs to, not
/// a map keyed by every session ever seen: nothing accumulates for the life
/// of the daemon, and nothing needs pruning when a session closes. The tag
/// is what keeps a call that arrives *without* a session (stateless
/// transport, or any request that never went through `initialize`) from
/// reading, or overwriting, the session's choice: such a call has no
/// identity to remember a mode under, so it runs as `Full` and
/// `memcastle_set_mode` refuses it. A session that never calls
/// `memcastle_set_mode` is `Full`, exactly like a missing
/// `X-MemCastle-Mode` header over HTTP.
#[derive(Clone)]
pub struct McpTools {
    app: AppServices,
    mode: Arc<Mutex<Option<SessionMode>>>,
    tool_router: ToolRouter<Self>,
}

/// The mode a session chose, and which session that was.
#[derive(Debug, Clone)]
struct SessionMode {
    session_id: String,
    mode: MemoryMode,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// The search query, in your own words.
    query: String,
    /// Maximum number of results to return.
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to drawers filed (transitively, via their room)
    /// under this wing name.
    wing: Option<String>,
    /// Restrict results to drawers filed directly under this room name.
    room: Option<String>,
    /// Ranking: `auto` (default: hybrid when the daemon can embed the query,
    /// else lexical), `lexical`, `semantic` or `hybrid`.
    ranking: Option<String>,
    /// Only drawers carrying every one of these tags.
    #[serde(default)]
    tags: Vec<String>,
    /// Only drawers from this kind of source: `file`, `manual`, `transcript`, `note` or `other`.
    source_kind: Option<String>,
    /// An RFC 3339 instant (`2026-01-31T12:00:00Z`) or a date (`2026-01-31`):
    /// search the memory that was valid then instead of now.
    as_of: Option<String>,
    /// Start of an interval (inclusive, same forms as `as_of`): search the memory
    /// that was valid at some moment between `from` and `until`. Needs `until`.
    from: Option<String>,
    /// End of an interval (exclusive). Needs `from`.
    until: Option<String>,
    /// Also return memory that has since been superseded.
    #[serde(default)]
    include_historical: bool,
    /// Also surface drawers related to the hits through the knowledge graph.
    #[serde(default)]
    expand: bool,
}

fn default_search_limit() -> u32 {
    crate::app::DEFAULT_SEARCH_LIMIT
}

/// `memcastle_recall`'s arguments: [`SearchArgs`] without `room`, spelled out
/// rather than shared so the tool's schema does not advertise an option recall
/// ignores.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RecallArgs {
    /// The recall query, in your own words.
    query: String,
    /// Maximum number of results to return.
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to drawers filed (transitively, via their room)
    /// under this wing name.
    wing: Option<String>,
    /// Ranking: `auto` (default), `lexical`, `semantic` or `hybrid`.
    ranking: Option<String>,
    /// Only drawers carrying every one of these tags.
    #[serde(default)]
    tags: Vec<String>,
    /// Only drawers from this kind of source: `file`, `manual`, `transcript`, `note` or `other`.
    source_kind: Option<String>,
    /// An RFC 3339 instant or a date: recall the memory that was valid then.
    as_of: Option<String>,
    /// Start of an interval (inclusive): recall what was valid at some moment
    /// between `from` and `until`. Needs `until`.
    from: Option<String>,
    /// End of an interval (exclusive). Needs `from`.
    until: Option<String>,
    /// Also return memory that has since been superseded.
    #[serde(default)]
    include_historical: bool,
    /// Also surface drawers related to the hits through the knowledge graph.
    #[serde(default)]
    expand: bool,
}

impl RecallArgs {
    fn into_query(self) -> Result<crate::search::SearchQuery, crate::Error> {
        SearchArgs {
            query: self.query,
            limit: self.limit,
            wing: self.wing,
            room: None,
            ranking: self.ranking,
            tags: self.tags,
            source_kind: self.source_kind,
            as_of: self.as_of,
            from: self.from,
            until: self.until,
            include_historical: self.include_historical,
            expand: self.expand,
        }
        .into_query(false)
    }
}

impl SearchArgs {
    /// The validated query, with `room` dropped when `recall` is the caller.
    fn into_query(self, with_room: bool) -> Result<crate::search::SearchQuery, crate::Error> {
        crate::search::SearchOptions {
            limit: Some(self.limit),
            wing: self.wing,
            room: if with_room { self.room } else { None },
            ranking: self.ranking,
            tags: self.tags,
            source_kind: self.source_kind,
            as_of: self.as_of,
            from: self.from,
            until: self.until,
            include_historical: self.include_historical,
            expand: self.expand,
        }
        .into_query(self.query)
    }
}

/// `memcastle_history`'s arguments.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct HistoryArgs {
    /// The id (a UUID) of any version of the knowledge, as a search hit's `id`
    /// carries it: current or superseded, the whole chain comes back.
    drawer_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WakeUpArgs {
    /// The identity to build session-start context for.
    agent_identity: String,
    /// Restrict the diary lookup and recent highlights to this wing.
    /// `None` skips the diary lookup entirely (see
    /// `AppServices::wake_up`'s doc comment) but still returns unscoped
    /// recent highlights.
    wing: Option<String>,
    /// Maximum number of recent-highlight drawers to include. Falls back
    /// to `WakeUpBudget::default()` when omitted.
    max_items: Option<usize>,
    /// Maximum total content bytes across recent highlights. Falls back
    /// to `WakeUpBudget::default()` when omitted.
    max_bytes: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CheckpointArgs {
    /// The checkpoint payload, matching `domain::CheckpointPayload`'s JSON
    /// shape exactly: `{"items": [{"destination":
    /// "preference"|"project"|"diary"|"general", "wing": string|null,
    /// "content": string, "tags": [string], "source": {"kind":
    /// "file"|"manual", "uri": string|null, "agent": string|null}, "fact":
    /// null|{"op": "add"|"supersede"|"invalidate", ...}}]}`. Passed as a
    /// JSON object, not a JSON-encoded string. Held as a raw value and
    /// validated by `CheckpointPayload`'s own deserialization, with a
    /// hand-written advertised schema ([`checkpoint_payload_schema`]) rather
    /// than `schemars::JsonSchema` derives threaded through every
    /// knowledge-graph domain type for one argument.
    #[schemars(schema_with = "checkpoint_payload_schema")]
    payload: serde_json::Value,
    /// Escalate to `Priority::Critical`, preempting all other queued work —
    /// reserved for save-before-crash situations, not routine checkpoints.
    #[serde(default)]
    emergency: bool,
}

/// The JSON Schema advertised for [`CheckpointArgs::payload`].
///
/// A bare `serde_json::Value` renders as the schema `true`, which tells a
/// model nothing about the argument's shape; models then guess, and some
/// JSON-encode the payload into a string. Spelling the object out steers
/// them to send an object. Keep it in step with
/// `domain::CheckpointPayload` (tests check the enum values, the required
/// fields and the set of property names against the real type).
fn checkpoint_payload_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "object",
        "description": "The checkpoint payload, as a JSON object (not a JSON-encoded string).",
        "properties": {
            "items": {
                "type": "array",
                "description": "The memories to persist, in order.",
                "items": {
                    "type": "object",
                    "properties": {
                        "destination": {
                            "type": "string",
                            "enum": ["preference", "project", "diary", "general"],
                            "description": "Which bucket the content belongs to."
                        },
                        "wing": {
                            "type": ["string", "null"],
                            "description": "Optional wing override; null uses the destination's default wing."
                        },
                        "name": {
                            "type": ["string", "null"],
                            "description": "Optional name, unique within the room, so the drawer can be addressed as wing/room/name."
                        },
                        "content": {
                            "type": "string",
                            "description": "The text to store, verbatim."
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Free-form labels; may be empty."
                        },
                        "source": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string", "enum": ["file", "manual"] },
                                "uri": { "type": ["string", "null"] },
                                "agent": { "type": ["string", "null"] }
                            },
                            "required": ["kind"],
                            "description": "Where the memory came from."
                        },
                        "fact": {
                            "type": ["object", "null"],
                            "description": "Optional knowledge-graph change, with the ids of entities and relationships that already exist. \"add\" needs subject, predicate, object and confidence; \"supersede\" needs relationship_id, from, to, predicate and confidence; \"invalidate\" needs relationship_id.",
                            "properties": {
                                "op": { "type": "string", "enum": ["add", "supersede", "invalidate"] },
                                "subject": { "type": "string", "description": "Entity id (add)." },
                                "object": { "type": "string", "description": "Entity id (add)." },
                                "relationship_id": { "type": "string", "description": "The edge to close (supersede, invalidate)." },
                                "from": { "type": "string", "description": "Entity id of the replacement's subject (supersede)." },
                                "to": { "type": "string", "description": "Entity id of the replacement's object (supersede)." },
                                "predicate": { "type": "string", "description": "The relationship's label (add, supersede)." },
                                "confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Confidence in [0, 1] (add, supersede)." }
                            },
                            "required": ["op"]
                        }
                    },
                    "required": ["destination", "content", "tags", "source"]
                }
            }
        },
        "required": ["items"]
    })
}

/// Accept a checkpoint payload sent as a JSON-encoded string.
///
/// Some clients and models serialise the object into a string before
/// sending it; the intent is unambiguous, and rejecting it sent an agent
/// to the CLI instead of storing the memory. Anything else passes through
/// untouched for `CheckpointPayload`'s own deserialization to judge.
fn unwrap_stringified_payload(raw: serde_json::Value) -> Result<serde_json::Value, Error> {
    match raw {
        serde_json::Value::String(text) => serde_json::from_str(&text).map_err(|source| {
            Error::invalid_input(
                "payload",
                format!("expected a JSON object, got a string that is not valid JSON: {source}"),
            )
        }),
        other => Ok(other),
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct DiaryWriteArgs {
    /// The identity to scope this diary entry to — keep this consistent
    /// across writes/reads (see `AppServices::diary_write`'s doc comment):
    /// MemCastle stores/retrieves by this string faithfully, but never
    /// normalizes or verifies it itself.
    agent_identity: String,
    /// The wing to file this entry under, in its fixed `"diary"` room.
    wing: String,
    /// The diary entry's content.
    content: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct DiaryReadArgs {
    /// The identity whose diary entries to read back.
    agent_identity: String,
    /// The wing to read this identity's entries from.
    wing: String,
    /// Maximum number of entries to return, newest first.
    #[serde(default = "default_diary_limit")]
    limit: u32,
}

fn default_diary_limit() -> u32 {
    crate::app::DEFAULT_DIARY_LIMIT
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MineArgs {
    /// The absolute path to a directory to mine. Give this, or `source`.
    path: Option<String>,
    /// A source adapter to mine instead of a directory, e.g. `pi` (Pi coding-agent session history, once installed).
    /// `GET /api/sources` lists them.
    source: Option<String>,
    /// Where within `source` to read, when it needs more than its default (for `pi`, a directory of Pi sessions).
    locator: Option<String>,
    /// Read the source again from the beginning instead of continuing from where the last run stopped. Unchanged
    /// documents are still skipped, so nothing is duplicated.
    #[serde(default)]
    full: bool,
    /// The wing to file mined drawers under. Defaults to the directory name, or to the source's own default.
    wing: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AuditArgs {
    /// Restrict the report's embedding-count fields to one wing by name.
    /// Orphan-drawer and dangling-provenance findings are always
    /// palace-wide regardless of this (see `memcastle::audit`'s module doc).
    scope: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RepairArgs {
    /// Only report what would be removed (the default). Set `false` to
    /// actually remove orphan drawers — that is a write, so it is rejected
    /// in `read_only` and `disabled` mode.
    #[serde(default = "default_dry_run")]
    dry_run: bool,
    /// Narrow the repair to what this completed audit job also found (a job
    /// id from `memcastle_audit`). A live scan always decides what is
    /// removed; this can only narrow it.
    based_on_job: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct JobIdArgs {
    /// The job's id, as returned when it was submitted.
    id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct JobsListArgs {
    /// Only list jobs in this status: queued, running, paused, completed,
    /// failed or cancelled.
    status: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetModeArgs {
    /// This session's memory mode from now on: `"full"` (default — reads
    /// and writes both proceed), `"read_only"` (reads proceed, writes
    /// rejected), or `"disabled"` (neither reads nor writes proceed — see
    /// `domain::MemoryMode`'s doc comment for the full allow/deny matrix).
    /// A plain string rather than a typed `MemoryMode` field — deriving
    /// `schemars::JsonSchema` on that domain enum would mean `domain`
    /// depending on `schemars` for one MCP-only argument, the same
    /// trade-off `CheckpointArgs::payload` already makes for
    /// `CheckpointPayload`.
    mode: String,
}

/// What every job and drawer written through this surface records as the
/// channel it came through (`Job::requested_by`, `provenance.requested_by`).
const CHANNEL: &str = crate::domain::channel::MCP;

/// The one place a tool's outcome becomes an MCP result, so every tool
/// reports success and failure the same way.
///
/// A success is the value as pretty JSON — and a value that cannot be
/// serialized is a failure ([`Error::Serialization`]), not an empty string
/// the caller would take for "no results". A failure is the same
/// [`crate::error::ErrorBody`] the REST API serves, with the diagnostic code
/// and help, so an integration can act on it instead of parsing prose.
///
/// Each call is traced at `debug`, and each failure at `warn`, with the tool
/// and the code: the MCP transport otherwise leaves no record of what was
/// asked or refused.
fn tool_result<T: serde::Serialize>(
    tool: &'static str,
    result: crate::Result<T>,
) -> Result<CallToolResult, McpError> {
    let outcome = result.and_then(|value| {
        serde_json::to_string_pretty(&value)
            .map_err(|source| Error::serialization(format!("the `{tool}` response"), source))
    });
    Ok(match outcome {
        Ok(text) => {
            tracing::debug!(tool, "mcp tool call succeeded");
            CallToolResult::success(vec![ContentBlock::text(text)])
        }
        Err(error) => {
            let body = error.body();
            tracing::warn!(
                tool,
                error = %error,
                code = body.code.as_deref().unwrap_or("-"),
                "mcp tool call failed"
            );
            // `ErrorBody` is three optional strings; serializing it cannot
            // fail, but if it somehow did the plain message still gets out.
            let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| error.to_string());
            CallToolResult::error(vec![ContentBlock::text(text)])
        }
    })
}

#[tool_router]
impl McpTools {
    /// Wrap `app` as an MCP tool surface.
    #[must_use]
    pub fn new(app: AppServices) -> Self {
        Self {
            app,
            mode: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    /// Read the `mcp-session-id` header rmcp's streamable-HTTP transport
    /// sets on every request after the initialize handshake — the only way
    /// to identify "which session is this" from inside a tool handler (see
    /// `McpTools::mode`'s doc comment).
    ///
    /// `None` when the header is absent or empty: a request with no session
    /// has no identity, and must not be given one by falling back to `""`,
    /// which every such request would share.
    fn session_id(parts: &http::request::Parts) -> Option<String> {
        parts
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }

    /// This request's effective `MemoryMode`: whatever `memcastle_set_mode`
    /// last recorded for *its* session, or `Full` — including for a request
    /// with no session, which is never cached and never sees another's mode.
    fn mode_for(&self, parts: &http::request::Parts) -> MemoryMode {
        let Some(session_id) = Self::session_id(parts) else {
            return MemoryMode::Full;
        };
        // A poisoned lock only means another tool call panicked mid-update;
        // the slot is a plain value, so reading it is still sound.
        let slot = self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match slot.as_ref() {
            Some(chosen) if chosen.session_id == session_id => chosen.mode,
            _ => MemoryMode::Full,
        }
    }

    /// Record `mode` for the session `parts` belongs to. Refused without one:
    /// there is nothing to remember it under, and accepting it would be a lie
    /// (the next call would still run as `Full`).
    fn set_mode(
        &self,
        mode: &str,
        parts: &http::request::Parts,
    ) -> crate::Result<serde_json::Value> {
        let mode: MemoryMode = mode
            .parse()
            .map_err(|message: String| Error::invalid_input("mode", message))?;
        let Some(session_id) = Self::session_id(parts) else {
            return Err(Error::invalid_input(
                "mcp-session-id",
                "memcastle_set_mode needs an MCP session, and this request has none, so the \
                 mode cannot be remembered for later calls; use a client that keeps an MCP \
                 session open, or send `X-MemCastle-Mode` over REST",
            ));
        };
        *self
            .mode
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(SessionMode { session_id, mode });
        Ok(serde_json::json!({ "mode": mode }))
    }

    #[tool(
        description = "Set this MCP session's memory mode (full/read_only/disabled); call once \
                        at session start — every other tool call on this session uses whatever \
                        was set here, defaulting to full until this is called"
    )]
    async fn memcastle_set_mode(
        &self,
        Parameters(args): Parameters<SetModeArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        tool_result("memcastle_set_mode", self.set_mode(&args.mode, &parts))
    }

    #[tool(
        description = "Report daemon health: version, uptime, pid, listen address, palace name and path, datastore health and migration state, drawer and job counts"
    )]
    async fn memcastle_status(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        tool_result("memcastle_status", self.app.status(mode).await)
    }

    #[tool(
        description = "Search palace drawer content. By default the ranking is hybrid (word \
                        match plus meaning) when the daemon can embed the query, and word match \
                        alone otherwise; `ranking` forces lexical, semantic or hybrid. Word matching \
                        is stemmed with no synonyms: drawers containing every query word are \
                        returned, and only if there are none, drawers containing any of them. \
                        Narrow with wing, room, tags and source_kind. Time is validity time, \
                        when the knowledge was true, not when it was stored: by default only \
                        what is true now; `as_of` (an instant or a date) what was true then; \
                        `from` with `until` what was true at some moment of that interval \
                        (`until` exclusive); `include_historical` every version ever. To see \
                        how one piece of knowledge evolved, pass a hit's id to memcastle_history. \
                        `expand` adds drawers related through the knowledge graph. Each hit is \
                        the stored drawer verbatim plus a `score` that is comparable only within \
                        one response."
    )]
    async fn memcastle_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let hits = match args.into_query(true) {
            Ok(query) => self.app.search(query, mode).await,
            Err(error) => Err(error),
        };
        tool_result("memcastle_search", hits)
    }

    #[tool(
        description = "Retrieve palace content matching a query, returned verbatim — the \
                        recall-oriented counterpart to memcastle_search (see \
                        AppServices::recall's doc comment for why both exist). Takes the same \
                        options as memcastle_search except `room`: ranking mode, tags, \
                        source_kind, as_of, from, until, include_historical and expand."
    )]
    async fn memcastle_recall(
        &self,
        Parameters(args): Parameters<RecallArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let hits = match args.into_query() {
            Ok(query) => self.app.recall(query, mode).await,
            Err(error) => Err(error),
        };
        tool_result("memcastle_recall", hits)
    }

    #[tool(
        description = "Show how one piece of knowledge evolved: every version of a drawer's \
                        supersession chain, oldest first, each verbatim with its id, validity \
                        period (`valid_from`, `valid_to`, absent while still true), provenance \
                        and content. Takes the id of any version, such as a hit from \
                        memcastle_search with `include_historical`. Read-only."
    )]
    async fn memcastle_history(
        &self,
        Parameters(args): Parameters<HistoryArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let history = match args.drawer_id.parse() {
            Ok(id) => self.app.drawer_history(id, mode).await,
            Err(_) => Err(crate::Error::invalid_input(
                "drawer_id",
                format!("`{}` is not a drawer id (a UUID)", args.drawer_id),
            )),
        };
        tool_result("memcastle_history", history)
    }

    #[tool(
        description = "Build an agent identity's session-start context: its most recent diary \
                        entry (when a wing is given) plus recent checkpoint-originated \
                        highlights, bounded by a deterministic item/byte budget"
    )]
    async fn memcastle_wake_up(
        &self,
        Parameters(args): Parameters<WakeUpArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let budget = WakeUpBudget::from_options(args.max_items, args.max_bytes);
        let context = self
            .app
            .wake_up(&args.agent_identity, args.wing.as_deref(), budget, mode)
            .await;
        tool_result("memcastle_wake_up", context)
    }

    #[tool(
        description = "Submit a mining job for a directory (`path`) or a source adapter (`source`); \
                        returns the job id immediately. Mining is incremental: a source remembers where \
                        the last run stopped and unchanged documents are not filed again"
    )]
    async fn memcastle_mine(
        &self,
        Parameters(args): Parameters<MineArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let source = match (args.path, args.source) {
            (Some(path), None) => Ok(MiningSource::Directory { path: path.into() }),
            (None, Some(provider)) => Ok(MiningSource::Provider {
                provider,
                locator: args.locator,
            }),
            _ => Err(crate::Error::invalid_input(
                "path",
                "give exactly one of `path` (a directory) or `source` (a source adapter)",
            )),
        };
        let job = match source {
            Ok(source) => {
                self.app
                    .submit_mine(source, args.wing, args.full, CHANNEL, mode)
                    .await
            }
            Err(error) => Err(error),
        };
        tool_result("memcastle_mine", job)
    }

    #[tool(
        description = "Persist a pre-classified checkpoint payload (see the `payload` \
                        argument's own description for its exact JSON shape); set \
                        emergency=true to preempt all other queued work"
    )]
    async fn memcastle_checkpoint(
        &self,
        Parameters(args): Parameters<CheckpointArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let parsed = unwrap_stringified_payload(args.payload).and_then(|value| {
            serde_json::from_value::<CheckpointPayload>(value)
                .map_err(|source| Error::invalid_input("payload", source.to_string()))
        });
        let payload = match parsed {
            Ok(payload) => payload,
            Err(error) => return tool_result::<Job>("memcastle_checkpoint", Err(error)),
        };
        let job = self
            .app
            .submit_checkpoint_with_urgency(payload, args.emergency, CHANNEL, mode)
            .await;
        tool_result("memcastle_checkpoint", job)
    }

    #[tool(
        description = "Submit a read-only palace consistency audit; returns the job id \
                        immediately — poll memcastle_job_get for the \
                        report, which lands in the job's `result` field once completed"
    )]
    async fn memcastle_audit(
        &self,
        Parameters(args): Parameters<AuditArgs>,
    ) -> Result<CallToolResult, McpError> {
        tool_result(
            "memcastle_audit",
            self.app.submit_audit(args.scope, CHANNEL).await,
        )
    }

    #[tool(
        description = "Write a diary entry for an agent identity, filed in a wing's fixed diary room"
    )]
    async fn memcastle_diary_write(
        &self,
        Parameters(args): Parameters<DiaryWriteArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let drawer = self
            .app
            .diary_write(
                &args.agent_identity,
                &args.wing,
                args.content,
                CHANNEL,
                mode,
            )
            .await;
        tool_result("memcastle_diary_write", drawer)
    }

    #[tool(
        description = "Read back an agent identity's most recent diary entries in a wing, newest first"
    )]
    async fn memcastle_diary_read(
        &self,
        Parameters(args): Parameters<DiaryReadArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let entries = self
            .app
            .diary_read(&args.agent_identity, &args.wing, args.limit, mode)
            .await;
        tool_result("memcastle_diary_read", entries)
    }

    #[tool(
        description = "Submit a repair job that removes orphan drawers (drawers whose room no \
                        longer exists). Dry-run by default: it only reports what it would \
                        remove, in the job's `result`. Pass dry_run=false to apply it, which is a \
                        write. based_on_job narrows it to what a completed memcastle_audit found"
    )]
    async fn memcastle_repair(
        &self,
        Parameters(args): Parameters<RepairArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let job = async {
            let based_on_job = args
                .based_on_job
                .as_deref()
                .map(Error::parse_job_id)
                .transpose()?;
            self.app
                .submit_repair(args.dry_run, based_on_job, CHANNEL, mode)
                .await
        }
        .await;
        tool_result("memcastle_repair", job)
    }

    #[tool(
        description = "List jobs known to the daemon, newest first, optionally only those in one \
                        status (queued, running, paused, completed, failed or cancelled)"
    )]
    async fn memcastle_job_list(
        &self,
        Parameters(args): Parameters<JobsListArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let jobs = async {
            let status = args
                .status
                .as_deref()
                .map(Error::parse_job_status)
                .transpose()?;
            self.app.list_jobs(status, mode).await
        }
        .await;
        tool_result("memcastle_job_list", jobs)
    }

    #[tool(
        description = "Show one job: its status, progress, attempt counts and, once finished, \
                        its result (an audit's or repair's report) or error"
    )]
    async fn memcastle_job_get(
        &self,
        Parameters(args): Parameters<JobIdArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        let job = async { self.app.get_job(Error::parse_job_id(&args.id)?, mode).await }.await;
        tool_result("memcastle_job_get", job)
    }

    #[tool(
        description = "Ask a running job to pause at its next checkpoint. It is a request: the \
                        job stops at its next check, not instantly. Resume it with memcastle_job_resume"
    )]
    async fn memcastle_job_pause(
        &self,
        Parameters(args): Parameters<JobIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = async { self.app.pause_job(Error::parse_job_id(&args.id)?).await }.await;
        tool_result("memcastle_job_pause", result)
    }

    #[tool(description = "Resume a paused job; it continues from where it stopped")]
    async fn memcastle_job_resume(
        &self,
        Parameters(args): Parameters<JobIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = async { self.app.resume_job(Error::parse_job_id(&args.id)?).await }.await;
        tool_result("memcastle_job_resume", result)
    }

    #[tool(
        description = "Cancel a queued, paused or running job. For a running one it is a \
                        request: the job stops at its next check. A cancelled job is not re-run"
    )]
    async fn memcastle_job_cancel(
        &self,
        Parameters(args): Parameters<JobIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = async { self.app.cancel_job(Error::parse_job_id(&args.id)?).await }.await;
        tool_result("memcastle_job_cancel", result)
    }

    #[tool(
        description = "Retry a failed job: it goes back to the queue and resumes from its checkpoint"
    )]
    async fn memcastle_job_retry(
        &self,
        Parameters(args): Parameters<JobIdArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = async { self.app.retry_job(Error::parse_job_id(&args.id)?).await }.await;
        tool_result("memcastle_job_retry", result)
    }
}

impl McpTools {
    /// The instruction text sent to every client at `initialize`.
    ///
    /// The tool list is read from the router, not typed out: this string is
    /// where an integration first learns what exists, and a hand-kept copy
    /// silently omitted `repair` and every job-control tool until a test
    /// compared the two.
    fn instructions(&self) -> String {
        let tools: Vec<String> = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        format!(
            "MemCastle: a shared memory palace daemon. Tools: {}. Call memcastle_set_mode once \
             at session start to switch this session to read_only or disabled memory mode \
             (defaults to full). Submitting work (mine, checkpoint, audit, repair) returns a job \
             immediately; follow it with memcastle_job_get and control it with memcastle_job_pause, \
             memcastle_job_resume, memcastle_job_cancel and memcastle_job_retry.",
            tools.join(", ")
        )
    }
}

#[tool_handler]
impl ServerHandler for McpTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(self.instructions())
    }
}

/// Build the `/mcp` axum service, cancelled by `shutdown`.
#[must_use]
pub fn service(
    app: AppServices,
    shutdown: &CancellationToken,
) -> StreamableHttpService<McpTools, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(McpTools::new(app.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(shutdown.child_token()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{JobId, JobStatus};

    /// The `Parts` of a request carrying `session` as its `mcp-session-id`,
    /// or none at all.
    fn parts(session: Option<&str>) -> http::request::Parts {
        let mut request = http::Request::builder();
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        request.body(()).expect("request").into_parts().0
    }

    async fn tools() -> McpTools {
        McpTools::new(AppServices::for_tests().await)
    }

    async fn set_mode(tools: &McpTools, session: Option<&str>, mode: &str) -> CallToolResult {
        tools
            .memcastle_set_mode(
                Parameters(SetModeArgs {
                    mode: mode.to_string(),
                }),
                Extension(parts(session)),
            )
            .await
            .expect("tool call")
    }

    #[tokio::test]
    async fn a_session_that_never_set_a_mode_runs_as_full() {
        let tools = tools().await;
        assert_eq!(tools.mode_for(&parts(Some("s1"))), MemoryMode::Full);
    }

    #[tokio::test]
    async fn a_session_keeps_the_mode_it_chose() {
        let tools = tools().await;

        let result = set_mode(&tools, Some("s1"), "read_only").await;

        assert_ne!(result.is_error, Some(true));
        assert_eq!(tools.mode_for(&parts(Some("s1"))), MemoryMode::ReadOnly);
    }

    #[tokio::test]
    async fn a_call_from_another_session_does_not_inherit_the_mode() {
        let tools = tools().await;
        set_mode(&tools, Some("s1"), "disabled").await;

        assert_eq!(tools.mode_for(&parts(Some("s2"))), MemoryMode::Full);
    }

    #[tokio::test]
    async fn setting_a_mode_without_a_session_is_refused_and_remembers_nothing() {
        let tools = tools().await;

        let result = set_mode(&tools, None, "disabled").await;

        assert_eq!(
            result.is_error,
            Some(true),
            "the caller must be told it failed"
        );
        assert_eq!(tools.mode_for(&parts(None)), MemoryMode::Full);
    }

    #[tokio::test]
    async fn two_headerless_calls_cannot_influence_each_others_mode() {
        let tools = tools().await;
        // One headerless client tries to lock everyone out...
        set_mode(&tools, None, "disabled").await;

        // ...and a second one is unaffected, as is a real session.
        assert_eq!(tools.mode_for(&parts(None)), MemoryMode::Full);
        assert_eq!(tools.mode_for(&parts(Some("s1"))), MemoryMode::Full);
    }

    #[tokio::test]
    async fn a_headerless_call_never_sees_a_sessions_mode() {
        let tools = tools().await;
        set_mode(&tools, Some("s1"), "read_only").await;

        assert_eq!(
            tools.mode_for(&parts(None)),
            MemoryMode::Full,
            "a request with no session must not read another session's choice"
        );
    }

    #[tokio::test]
    async fn an_empty_session_header_is_treated_as_no_session() {
        let tools = tools().await;

        let result = set_mode(&tools, Some(""), "disabled").await;

        assert_eq!(result.is_error, Some(true));
    }

    fn text_of(result: &CallToolResult) -> String {
        serde_json::to_value(&result.content)
            .unwrap()
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_failed_tool_call_reports_the_diagnostic_code_and_help_as_json() {
        let error = Error::ModeForbidden {
            operation: "checkpoint".to_string(),
            mode: MemoryMode::ReadOnly,
        };

        let result = tool_result::<()>("memcastle_checkpoint", Err(error)).unwrap();

        assert_eq!(result.is_error, Some(true));
        let body: serde_json::Value = serde_json::from_str(&text_of(&result)).expect("json body");
        assert_eq!(body["code"], "memcastle::app::mode_forbidden");
        assert!(
            body["help"].is_string(),
            "the body must say what to do: {body}"
        );
        assert!(body["error"].as_str().unwrap().contains("checkpoint"));
    }

    #[test]
    fn a_value_that_cannot_be_serialized_is_an_error_not_an_empty_success() {
        struct Broken;
        impl serde::Serialize for Broken {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("nope"))
            }
        }

        let result = tool_result("memcastle_status", Ok(Broken)).unwrap();

        assert_eq!(
            result.is_error,
            Some(true),
            "an empty string would read as 'no results'"
        );
        let body: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["code"], "memcastle::serialization::failed");
    }

    #[test]
    fn a_successful_tool_call_is_the_value_as_pretty_json() {
        let result = tool_result("memcastle_status", Ok(vec![1, 2])).unwrap();

        assert_ne!(result.is_error, Some(true));
        assert_eq!(
            serde_json::from_str::<Vec<i32>>(&text_of(&result)).unwrap(),
            [1, 2]
        );
    }

    #[tokio::test]
    async fn set_mode_reports_the_name_it_accepts_not_the_debug_form() {
        let tools = tools().await;

        let result = set_mode(&tools, Some("s1"), "read_only").await;

        let body: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["mode"], "read_only");
    }

    #[tokio::test]
    async fn an_unknown_mode_is_an_input_error_that_lists_the_valid_ones() {
        let tools = tools().await;

        let result = set_mode(&tools, Some("s1"), "readonly").await;

        assert_eq!(result.is_error, Some(true));
        let body: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["code"], "memcastle::input::invalid");
        assert!(body["error"].as_str().unwrap().contains("read_only"));
    }

    fn one_item_payload() -> serde_json::Value {
        serde_json::json!({ "items": [{
            "destination": "preference",
            "wing": null,
            "content": "The user's main programming languages are Rust and Python.",
            "tags": ["user"],
            "source": { "kind": "manual", "uri": null, "agent": null },
            "fact": null,
        }] })
    }

    async fn checkpoint(tools: &McpTools, payload: serde_json::Value) -> CallToolResult {
        tools
            .memcastle_checkpoint(
                Parameters(CheckpointArgs {
                    payload,
                    emergency: false,
                }),
                Extension(parts(Some("s"))),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_checkpoint_payload_sent_as_a_json_string_is_accepted_like_the_object() {
        let tools = tools().await;
        let stringified = serde_json::Value::String(one_item_payload().to_string());

        let result = checkpoint(&tools, stringified).await;

        assert_eq!(code_of(&result), None, "{}", text_of(&result));
        let job: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(job["kind"]["type"], "checkpoint");
        assert_eq!(
            job["kind"]["payload"]["items"][0]["destination"],
            "preference"
        );
    }

    #[tokio::test]
    async fn a_checkpoint_payload_string_that_is_not_json_is_an_input_error_on_payload() {
        let tools = tools().await;

        let result = checkpoint(&tools, serde_json::json!("not json at all")).await;

        assert_eq!(
            code_of(&result).as_deref(),
            Some("memcastle::input::invalid")
        );
        assert!(text_of(&result).contains("payload"), "{}", text_of(&result));
    }

    #[tokio::test]
    async fn the_checkpoint_tool_advertises_an_object_schema_for_its_payload() {
        let tools = tools().await;
        let tool = tools
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "memcastle_checkpoint")
            .expect("checkpoint tool registered");

        let schema = serde_json::to_value(&*tool.input_schema).unwrap();

        // `true` (the schema of a bare `Value`) is what let a model send a string.
        let payload = &schema["properties"]["payload"];
        assert_eq!(payload["type"], "object", "{schema}");
        assert_eq!(
            payload["required"],
            serde_json::json!(["items"]),
            "{schema}"
        );
        let item = &payload["properties"]["items"]["items"];
        assert_eq!(
            item["required"],
            serde_json::json!(["destination", "content", "tags", "source"]),
            "required fields must match `CheckpointItem`: {schema}"
        );
    }

    #[test]
    fn the_advertised_checkpoint_enum_values_are_exactly_the_ones_the_domain_accepts() {
        let schema = serde_json::to_value(checkpoint_payload_schema(
            &mut schemars::SchemaGenerator::default(),
        ))
        .unwrap();
        let item = &schema["properties"]["items"]["items"]["properties"];

        // Each advertised value must deserialize, so the schema cannot promise what the domain refuses.
        for destination in item["destination"]["enum"].as_array().unwrap() {
            let payload = serde_json::json!({ "items": [{
                "destination": destination,
                "content": "c",
                "tags": [],
                "source": { "kind": "manual" },
            }] });
            serde_json::from_value::<CheckpointPayload>(payload)
                .unwrap_or_else(|e| panic!("{destination} is advertised but refused: {e}"));
        }
        for kind in item["source"]["properties"]["kind"]["enum"]
            .as_array()
            .unwrap()
        {
            let payload = serde_json::json!({ "items": [{
                "destination": "general",
                "content": "c",
                "tags": [],
                "source": { "kind": kind },
            }] });
            serde_json::from_value::<CheckpointPayload>(payload)
                .unwrap_or_else(|e| panic!("{kind} is advertised but refused: {e}"));
        }
        // And the doc example (one item, every field present) is accepted.
        serde_json::from_value::<CheckpointPayload>(one_item_payload()).expect("example payload");
    }

    #[test]
    fn the_advertised_checkpoint_properties_cover_every_field_the_domain_serialises() {
        use crate::domain::{
            CheckpointDestination, CheckpointItem, EntityId, FactMutation, RelationshipId, Source,
            SourceKind,
        };
        use std::collections::BTreeSet;

        let schema = serde_json::to_value(checkpoint_payload_schema(
            &mut schemars::SchemaGenerator::default(),
        ))
        .unwrap();
        let advertised = |value: &serde_json::Value| -> BTreeSet<String> {
            value["properties"]
                .as_object()
                .expect("an object schema")
                .keys()
                .cloned()
                .collect()
        };
        let keys_of = |value: serde_json::Value| -> BTreeSet<String> {
            value.as_object().unwrap().keys().cloned().collect()
        };
        let item_schema = &schema["properties"]["items"]["items"];

        // Every field set on the real type must be advertised, or a model never learns it exists
        // (`name` once was not).
        let item = CheckpointItem {
            destination: CheckpointDestination::General,
            wing: Some("w".into()),
            name: Some("n".into()),
            content: "c".into(),
            tags: vec![],
            source: Source::new(SourceKind::Manual, Some("u".into()), Some("a".into())),
            fact: None,
        };
        let mut item_keys = keys_of(serde_json::to_value(&item).unwrap());
        item_keys.insert("fact".into());
        assert_eq!(advertised(item_schema), item_keys);
        assert_eq!(
            advertised(&item_schema["properties"]["source"]),
            keys_of(serde_json::to_value(&item.source).unwrap()),
        );

        // The fact is a tagged union: the advertised properties are the union of every variant's keys.
        let (entity, relationship) = (EntityId::new(), RelationshipId::new());
        let mut fact_keys = BTreeSet::new();
        for fact in [
            FactMutation::Add {
                subject: entity,
                predicate: "p".into(),
                object: entity,
                confidence: 1.0,
            },
            FactMutation::Supersede {
                relationship_id: relationship,
                from: entity,
                to: entity,
                predicate: "p".into(),
                confidence: 1.0,
            },
            FactMutation::Invalidate {
                relationship_id: relationship,
            },
        ] {
            fact_keys.extend(keys_of(serde_json::to_value(&fact).unwrap()));
        }
        assert_eq!(advertised(&item_schema["properties"]["fact"]), fact_keys);
    }

    /// The diagnostic code of a failed tool call, or `None` if it succeeded.
    fn code_of(result: &CallToolResult) -> Option<String> {
        if result.is_error != Some(true) {
            return None;
        }
        let body: serde_json::Value = serde_json::from_str(&text_of(result)).ok()?;
        body["code"].as_str().map(str::to_string)
    }

    /// The code an error carries, read from the error itself: this module may
    /// not spell out `jobs` diagnostic codes (the `store-isolation` hook greps
    /// for that path), and asking the type is the sturdier check anyway.
    fn code_of_error(error: &Error) -> Option<String> {
        error.body().code
    }

    const MODE_FORBIDDEN: &str = "memcastle::app::mode_forbidden";

    /// A tools surface whose one session `s` has chosen `mode`.
    async fn tools_in_mode(mode: &str) -> McpTools {
        let tools = tools().await;
        set_mode(&tools, Some("s"), mode).await;
        tools
    }

    async fn queued_job(tools: &McpTools) -> String {
        tools
            .app
            .submit_demo(1, "test")
            .await
            .expect("submit")
            .id
            .to_string()
    }

    fn id_args(id: &str) -> Parameters<JobIdArgs> {
        Parameters(JobIdArgs { id: id.to_string() })
    }

    #[tokio::test]
    async fn the_instructions_name_exactly_the_registered_tools() {
        let tools = tools().await;
        let registered: std::collections::BTreeSet<String> = tools
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();

        let mentioned: std::collections::BTreeSet<String> = tools
            .instructions()
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|word| word.starts_with("memcastle_"))
            .map(str::to_string)
            .collect();

        assert_eq!(
            mentioned, registered,
            "an unlisted tool is invisible to an integration; a listed one that does not exist is a lie"
        );
        for expected in [
            "memcastle_repair",
            "memcastle_job_get",
            "memcastle_job_pause",
            "memcastle_job_resume",
            "memcastle_job_cancel",
            "memcastle_job_retry",
        ] {
            assert!(
                registered.contains(expected),
                "{expected} is not registered"
            );
        }
    }

    #[tokio::test]
    async fn a_repair_dry_run_is_allowed_in_every_mode_but_applying_one_is_a_write() {
        for mode in ["full", "read_only", "disabled"] {
            let tools = tools_in_mode(mode).await;
            let dry = tools
                .memcastle_repair(
                    Parameters(RepairArgs {
                        dry_run: true,
                        based_on_job: None,
                    }),
                    Extension(parts(Some("s"))),
                )
                .await
                .unwrap();
            assert_eq!(
                code_of(&dry),
                None,
                "a dry run only reports, in {mode} mode"
            );

            let applied = tools
                .memcastle_repair(
                    Parameters(RepairArgs {
                        dry_run: false,
                        based_on_job: None,
                    }),
                    Extension(parts(Some("s"))),
                )
                .await
                .unwrap();
            let expected = (mode != "full").then(|| MODE_FORBIDDEN.to_string());
            assert_eq!(code_of(&applied), expected, "applied repair in {mode} mode");
        }
    }

    #[tokio::test]
    async fn a_repair_based_on_a_malformed_or_unknown_job_is_an_input_error_not_a_crash() {
        let tools = tools().await;
        for based_on_job in ["not-a-job-id", &JobId::new().to_string()] {
            let result = tools
                .memcastle_repair(
                    Parameters(RepairArgs {
                        dry_run: true,
                        based_on_job: Some(based_on_job.to_string()),
                    }),
                    Extension(parts(Some("s"))),
                )
                .await
                .unwrap();
            let code = code_of(&result).expect("must fail");
            assert!(
                Some(&code) == code_of_error(&Error::invalid_job_id("")).as_ref()
                    || code == "memcastle::repair::invalid_based_on_job",
                "{code}"
            );
        }
    }

    #[tokio::test]
    async fn job_reads_are_allowed_in_full_and_read_only_mode_and_rejected_when_disabled() {
        for (mode, allowed) in [("full", true), ("read_only", true), ("disabled", false)] {
            let tools = tools_in_mode(mode).await;
            let id = queued_job(&tools).await;

            let get = tools
                .memcastle_job_get(id_args(&id), Extension(parts(Some("s"))))
                .await
                .unwrap();
            let list = tools
                .memcastle_job_list(
                    Parameters(JobsListArgs { status: None }),
                    Extension(parts(Some("s"))),
                )
                .await
                .unwrap();

            let expected = (!allowed).then(|| MODE_FORBIDDEN.to_string());
            assert_eq!(code_of(&get), expected, "job_get in {mode} mode");
            assert_eq!(code_of(&list), expected, "job_list in {mode} mode");
        }
    }

    #[tokio::test]
    async fn jobs_list_filters_by_status_and_rejects_an_unknown_one() {
        let tools = tools().await;
        queued_job(&tools).await;

        let queued = tools
            .memcastle_job_list(
                Parameters(JobsListArgs {
                    status: Some("queued".to_string()),
                }),
                Extension(parts(Some("s"))),
            )
            .await
            .unwrap();
        let running = tools
            .memcastle_job_list(
                Parameters(JobsListArgs {
                    status: Some("running".to_string()),
                }),
                Extension(parts(Some("s"))),
            )
            .await
            .unwrap();
        let bogus = tools
            .memcastle_job_list(
                Parameters(JobsListArgs {
                    status: Some("done".to_string()),
                }),
                Extension(parts(Some("s"))),
            )
            .await
            .unwrap();

        assert_eq!(
            serde_json::from_str::<Vec<Job>>(&text_of(&queued))
                .unwrap()
                .len(),
            1
        );
        assert!(
            serde_json::from_str::<Vec<Job>>(&text_of(&running))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            code_of(&bogus).as_deref(),
            Some("memcastle::input::invalid")
        );
    }

    #[tokio::test]
    async fn job_control_is_administrative_so_no_mode_rejects_it() {
        for mode in ["full", "read_only", "disabled"] {
            let tools = tools_in_mode(mode).await;
            let id = queued_job(&tools).await;

            let results = [
                (
                    "pause",
                    tools.memcastle_job_pause(id_args(&id)).await.unwrap(),
                ),
                (
                    "resume",
                    tools.memcastle_job_resume(id_args(&id)).await.unwrap(),
                ),
                (
                    "retry",
                    tools.memcastle_job_retry(id_args(&id)).await.unwrap(),
                ),
                (
                    "cancel",
                    tools.memcastle_job_cancel(id_args(&id)).await.unwrap(),
                ),
            ];
            for (name, result) in results {
                assert_ne!(
                    code_of(&result).as_deref(),
                    Some(MODE_FORBIDDEN),
                    "job {name} is administrative and must work in {mode} mode"
                );
            }
        }
    }

    #[tokio::test]
    async fn cancelling_a_queued_job_over_mcp_reports_the_daemons_answer() {
        let tools = tools().await;
        let id = queued_job(&tools).await;

        let result = tools.memcastle_job_cancel(id_args(&id)).await.unwrap();

        assert_eq!(code_of(&result), None);
        let body: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
        assert_eq!(body["status"], "cancel_requested");
        let job = tools
            .memcastle_job_get(id_args(&id), Extension(parts(None)))
            .await
            .unwrap();
        let job: Job = serde_json::from_str(&text_of(&job)).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn a_malformed_or_unknown_job_id_is_a_typed_error() {
        let tools = tools().await;

        let malformed = tools
            .memcastle_job_get(id_args("nope"), Extension(parts(None)))
            .await
            .unwrap();
        let unknown = tools
            .memcastle_job_get(id_args(&JobId::new().to_string()), Extension(parts(None)))
            .await
            .unwrap();

        assert_eq!(
            code_of(&malformed),
            code_of_error(&Error::invalid_job_id(""))
        );
        assert_eq!(
            code_of(&unknown),
            code_of_error(&Error::JobNotFound { id: String::new() })
        );
    }
}
