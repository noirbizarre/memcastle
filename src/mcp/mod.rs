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

use crate::app::AppServices;
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
                 memcastle_search, memcastle_mine, memcastle_checkpoint, memcastle_jobs_list."
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
