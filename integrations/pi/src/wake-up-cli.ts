// A command to run the wake-up on demand and show what a session start would inject.
//
// It shows the briefing to the user and nothing else: it is not added to the conversation, so looking never changes
// what the agent knows. It works whatever `wakeUp.enabled` says, because asking for it is the opt-in.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "./mcp-manager.ts"
import { fetchWakeUp, wingFor } from "./wake-up-core.ts"

export const WAKE_UP_COMMAND = "memcastle-wake-up"

export function registerWakeUpCommand(pi: ExtensionAPI, manager: () => McpManager | null): void {
  pi.registerCommand(WAKE_UP_COMMAND, {
    description: "Show what MemCastle would inject at the start of this session",
    handler: async (_args, ctx) => {
      const current = manager()
      const session = current?.session
      if (!current || !session) {
        // An `off` session has no manager, and answering with a briefing there would be MemCastle context from a
        // session that was meant to have none. Saying so is not context, it is the reason for the silence.
        ctx.ui.notify("MemCastle is not active in this session.", "info")
        return
      }
      const { settings } = current
      const wing = wingFor(settings.wakeUp, ctx.cwd)
      try {
        const briefing = await fetchWakeUp(session, settings.agentIdentity, wing)
        // Naming the wing is what lets the user see why nothing came back: asking about the wrong one is the usual cause.
        ctx.ui.notify(
          briefing ?? `MemCastle has nothing to wake up with yet (${wing === undefined ? "all wings" : `wing "${wing}"`}).`,
          "info",
        )
      } catch (error) {
        current.report(error, (message, level) => ctx.ui.notify(message, level))
      }
    },
  })
}
