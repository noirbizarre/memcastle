// The MemCastle extension for Pi.
//
// This file only decides *when* to talk to MemCastle; every memory operation is a call to the daemon over MCP.
// The connection, mode and failure handling are real and tested, and so are wake-up, search-before-answer and
// checkpointing and the audit command; a capability module that is still empty names the issue that fills it in.
//
// Pi may load an extension without starting a session, so the factory only registers handlers. Nothing is opened
// until `session_start`, and everything is closed, idempotently, in `session_shutdown`.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import { registerAuditCommand } from "./audit-command.ts"
import { CheckpointSessions, registerCheckpointAgent } from "./checkpoint-agent.ts"
import { registerCheckpointTool } from "./checkpoint-tool.ts"
import { registerDailyMine } from "./daily-mine.ts"
import { MemCastleFailure } from "./failures.ts"
import { InvalidModeError } from "./modes.ts"
import { McpManager } from "./mcp-manager.ts"
import { ProjectScopes } from "./project-core.ts"
import { registerSearchBeforeAnswer } from "./search-before-answer.ts"
import { resolveSettings } from "./settings.ts"
import { registerSkills } from "./skills.ts"
import { registerWakeUp } from "./wake-up.ts"
import { registerWakeUpCommand } from "./wake-up-cli.ts"

export default function memcastle(pi: ExtensionAPI): void {
  let manager: McpManager | null = null

  pi.on("session_start", async (_event, ctx) => {
    let settings
    try {
      settings = resolveSettings(undefined)
    } catch (error) {
      // Fail closed: a mode the user mistyped must not become `full`, so the extension does nothing at all.
      const detail = error instanceof InvalidModeError ? error.message : String(error)
      ctx.ui.notify(`MemCastle is disabled for this session: ${detail}`, "error")
      return
    }
    // An `off` session behaves as if MemCastle did not exist: no connection, no context, no skill (#27).
    if (settings.mode === "off") return

    // Resolved here, from Pi's own directory, and only now: an `off` session returned above without reading a file.
    // A broken project file or variable is reported once and the session carries on without a project scope.
    const project = new ProjectScopes(process.env, (error) =>
      ctx.ui.notify(`MemCastle: ${error instanceof MemCastleFailure ? error.toUserMessage() : String(error)}`, "warning"),
    ).for(ctx.cwd)
    manager = new McpManager(settings, process.env, project)
    // Not awaited: a slow or absent daemon must not hold up the first prompt. The failure is reported once, inside.
    void manager.start((message, level) => ctx.ui.notify(message, level))
  })

  pi.on("session_shutdown", async () => {
    const closing = manager
    manager = null
    await closing?.stop()
  })

  // Each capability reads the manager when it fires, so none of them holds a connection of its own.
  const current = () => manager
  registerWakeUp(pi, current)
  registerWakeUpCommand(pi, current)
  registerSearchBeforeAnswer(pi, current)
  // Offers the bundled skills to Pi's own skill listing; it reads the mode itself, so an `off` session gets none.
  registerSkills(pi)
  // The interval review and the manual command share one counter and one review at a time per Pi session.
  const checkpoints = new CheckpointSessions()
  registerCheckpointAgent(pi, current, checkpoints)
  registerCheckpointTool(pi, current, checkpoints)
  // The manual audit and its confirmed repair (#28).
  registerAuditCommand(pi, current)
  registerDailyMine(pi, current)
}
