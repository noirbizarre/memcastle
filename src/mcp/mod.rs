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

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use tokio_util::sync::CancellationToken;

use crate::app::{AppServices, WakeUpBudget};
use crate::domain::CheckpointPayload;

/// The MCP tool surface. Cheap to clone (holds only `AppServices`, itself
/// cheap to clone, and the macro-generated router).
///
/// `tool_router` looks unread to a naive dead-code scan — `#[tool_handler]`
/// wires it into `call_tool`/`list_tools` through macro-generated code, the
/// same pattern (and the same warning) as the SDK's own examples.
#[derive(Clone)]
pub struct McpTools {
    app: AppServices,
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
    10
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
    20
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MineArgs {
    /// The absolute path to a directory to mine.
    path: String,
    /// The wing to file mined drawers under. Defaults to the directory name.
    wing: Option<String>,
}

#[tool_router]
impl McpTools {
    /// Wrap `app` as an MCP tool surface.
    #[must_use]
    pub fn new(app: AppServices) -> Self {
        Self {
            app,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Report daemon health: version, uptime, palace name, drawer and job counts"
    )]
    async fn memcastle_status(&self) -> Result<CallToolResult, McpError> {
        match self.app.status().await {
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
    ) -> Result<CallToolResult, McpError> {
        match self
            .app
            .search(
                &args.query,
                args.limit,
                args.wing.as_deref(),
                args.room.as_deref(),
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
    ) -> Result<CallToolResult, McpError> {
        match self
            .app
            .recall(&args.query, args.wing.as_deref(), args.limit)
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
    ) -> Result<CallToolResult, McpError> {
        let default_budget = WakeUpBudget::default();
        let budget = WakeUpBudget {
            max_items: args.max_items.unwrap_or(default_budget.max_items),
            max_bytes: args.max_bytes.unwrap_or(default_budget.max_bytes),
        };
        match self
            .app
            .wake_up(&args.agent_identity, args.wing.as_deref(), budget)
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
    ) -> Result<CallToolResult, McpError> {
        let requested_by = "mcp".to_string();
        match self
            .app
            .submit_mine(args.path.into(), args.wing, requested_by)
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
    ) -> Result<CallToolResult, McpError> {
        let payload: CheckpointPayload = match serde_json::from_value(args.payload) {
            Ok(payload) => payload,
            Err(error) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    error.to_string(),
                )]));
            }
        };
        let requested_by = "mcp".to_string();
        let result = if args.emergency {
            self.app.emergency_checkpoint(payload, requested_by).await
        } else {
            self.app.checkpoint(payload, requested_by).await
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
        description = "Write a diary entry for an agent identity, filed in a wing's fixed diary room"
    )]
    async fn memcastle_diary_write(
        &self,
        Parameters(args): Parameters<DiaryWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .app
            .diary_write(&args.agent_identity, &args.wing, args.content)
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
    ) -> Result<CallToolResult, McpError> {
        match self
            .app
            .diary_read(&args.agent_identity, &args.wing, args.limit)
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
    async fn memcastle_jobs_list(&self) -> Result<CallToolResult, McpError> {
        match self.app.list_jobs(None).await {
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
                 memcastle_checkpoint, memcastle_diary_write, memcastle_diary_read, \
                 memcastle_jobs_list."
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
