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

use std::sync::Arc;

use axum::http;
use dashmap::DashMap;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use tokio_util::sync::CancellationToken;

use crate::app::{AppServices, WakeUpBudget};
use crate::domain::{CheckpointPayload, MemoryMode};

/// The MCP tool surface. Cheap to clone (holds only `AppServices`, itself
/// cheap to clone, an `Arc<DashMap<..>>`, and the macro-generated router).
///
/// `tool_router` looks unread to a naive dead-code scan — `#[tool_handler]`
/// wires it into `call_tool`/`list_tools` through macro-generated code, the
/// same pattern (and the same warning) as the SDK's own examples.
///
/// `modes` caches each MCP session's [`MemoryMode`], keyed by the
/// `mcp-session-id` HTTP header `StreamableHttpService`/`LocalSessionManager`
/// assigns per session — the MCP surface has no per-request header the way
/// HTTP does (see `api::ModeHeader`), so mode is instead negotiated once,
/// via `memcastle_set_mode`, and looked up by every subsequent tool call on
/// the same session. A session that never calls `memcastle_set_mode`
/// defaults to `Full` (`DashMap::get` returning `None`), exactly like a
/// missing `X-MemCastle-Mode` header does over HTTP. Same `Arc<DashMap<..>>`
/// pattern `jobs::Scheduler` already uses for `controls`.
#[derive(Clone)]
pub struct McpTools {
    app: AppServices,
    modes: Arc<DashMap<String, MemoryMode>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// The search query.
    query: String,
    /// Maximum number of results to return.
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to drawers filed (transitively, via their room)
    /// under this wing name.
    wing: Option<String>,
    /// Restrict results to drawers filed directly under this room name.
    room: Option<String>,
}

fn default_search_limit() -> u32 {
    crate::app::DEFAULT_SEARCH_LIMIT
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RecallArgs {
    /// The recall query.
    query: String,
    /// Maximum number of results to return.
    #[serde(default = "default_search_limit")]
    limit: u32,
    /// Restrict results to drawers filed (transitively, via their room)
    /// under this wing name.
    wing: Option<String>,
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
    /// null|{"op": "add"|"supersede"|"invalidate", ...}}]}`. Kept as a raw
    /// object here rather than a fully-typed schema — the shape is already
    /// enforced by `CheckpointPayload`'s own deserialization, and this
    /// avoids threading `schemars::JsonSchema` through every
    /// knowledge-graph domain type for one argument.
    payload: serde_json::Value,
    /// Escalate to `Priority::Critical`, preempting all other queued work —
    /// reserved for save-before-crash situations, not routine checkpoints.
    #[serde(default)]
    emergency: bool,
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
    /// The absolute path to a directory to mine.
    path: String,
    /// The wing to file mined drawers under. Defaults to the directory name.
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

#[tool_router]
impl McpTools {
    /// Wrap `app` as an MCP tool surface.
    #[must_use]
    pub fn new(app: AppServices) -> Self {
        Self {
            app,
            modes: Arc::new(DashMap::new()),
            tool_router: Self::tool_router(),
        }
    }

    /// Read the `mcp-session-id` header rmcp's streamable-HTTP transport
    /// sets on every request after the initialize handshake — the only way
    /// to identify "which session is this" from inside a tool handler (see
    /// `McpTools::modes`'s doc comment).
    fn session_id(parts: &http::request::Parts) -> String {
        parts
            .headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    /// This request's effective `MemoryMode`: whatever `memcastle_set_mode`
    /// last cached for its session, or `Full` if it never called that tool.
    fn mode_for(&self, parts: &http::request::Parts) -> MemoryMode {
        self.modes
            .get(&Self::session_id(parts))
            .map(|mode| *mode)
            .unwrap_or_default()
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
        let mode: MemoryMode =
            match serde_json::from_value(serde_json::Value::String(args.mode.clone())) {
                Ok(mode) => mode,
                Err(_) => {
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "unknown memory mode `{}` (expected full, read_only, or disabled)",
                        args.mode
                    ))]));
                }
            };
        self.modes.insert(Self::session_id(&parts), mode);
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "memory mode set to {mode:?}"
        ))]))
    }

    #[tool(
        description = "Report daemon health: version, uptime, palace name, drawer and job counts"
    )]
    async fn memcastle_status(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        match self.app.status(mode).await {
            Ok(status) => {
                let text = serde_json::to_string_pretty(&status).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    #[tool(description = "Lexically search palace drawer content")]
    async fn memcastle_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        match self
            .app
            .search(
                &args.query,
                args.wing.as_deref(),
                args.room.as_deref(),
                args.limit,
                mode,
            )
            .await
        {
            Ok(hits) => {
                let text = serde_json::to_string_pretty(&hits).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    #[tool(
        description = "Retrieve palace content matching a query, returned verbatim — the \
                        recall-oriented counterpart to memcastle_search (see \
                        AppServices::recall's doc comment for why both exist)"
    )]
    async fn memcastle_recall(
        &self,
        Parameters(args): Parameters<RecallArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        match self
            .app
            .recall(&args.query, args.wing.as_deref(), args.limit, mode)
            .await
        {
            Ok(hits) => {
                let text = serde_json::to_string_pretty(&hits).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
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
        match self
            .app
            .wake_up(&args.agent_identity, args.wing.as_deref(), budget, mode)
            .await
        {
            Ok(context) => {
                let text = serde_json::to_string_pretty(&context).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    #[tool(description = "Submit a mining job for a directory; returns the job id immediately")]
    async fn memcastle_mine(
        &self,
        Parameters(args): Parameters<MineArgs>,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let requested_by = "mcp";
        let mode = self.mode_for(&parts);
        match self
            .app
            .submit_mine(args.path.into(), args.wing, requested_by, mode)
            .await
        {
            Ok(job) => {
                let text = serde_json::to_string_pretty(&job).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
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
        let payload: CheckpointPayload = match serde_json::from_value(args.payload) {
            Ok(payload) => payload,
            Err(error) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    error.to_string(),
                )]));
            }
        };
        let requested_by = "mcp";
        let result = if args.emergency {
            self.app
                .emergency_checkpoint(payload, requested_by, mode)
                .await
        } else {
            self.app.checkpoint(payload, requested_by, mode).await
        };
        match result {
            Ok(job) => {
                let text = serde_json::to_string_pretty(&job).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    #[tool(
        description = "Submit a read-only palace consistency audit; returns the job id \
                        immediately — poll memcastle_jobs_list or GET /api/jobs/{id} for the \
                        report, which lands in the job's `result` field once completed"
    )]
    async fn memcastle_audit(
        &self,
        Parameters(args): Parameters<AuditArgs>,
    ) -> Result<CallToolResult, McpError> {
        let requested_by = "mcp";
        match self.app.submit_audit(args.scope, requested_by).await {
            Ok(job) => {
                let text = serde_json::to_string_pretty(&job).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
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
        match self
            .app
            .diary_write(&args.agent_identity, &args.wing, args.content, mode)
            .await
        {
            Ok(drawer) => {
                let text = serde_json::to_string_pretty(&drawer).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
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
        match self
            .app
            .diary_read(&args.agent_identity, &args.wing, args.limit, mode)
            .await
        {
            Ok(entries) => {
                let text = serde_json::to_string_pretty(&entries).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }

    #[tool(description = "List jobs known to the daemon")]
    async fn memcastle_jobs_list(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, McpError> {
        let mode = self.mode_for(&parts);
        match self.app.list_jobs(None, mode).await {
            Ok(jobs) => {
                let text = serde_json::to_string_pretty(&jobs).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
            }
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(
                error.to_string(),
            )])),
        }
    }
}

#[tool_handler]
impl ServerHandler for McpTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "MemCastle: a shared memory palace daemon. Tools: memcastle_status, \
                 memcastle_search, memcastle_recall, memcastle_wake_up, memcastle_mine, \
                 memcastle_checkpoint, memcastle_audit, memcastle_diary_write, \
                 memcastle_diary_read, memcastle_jobs_list, memcastle_set_mode. Call \
                 memcastle_set_mode once at session start to switch this session to read_only \
                 or disabled memory mode (defaults to full)."
                    .to_string(),
            )
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
