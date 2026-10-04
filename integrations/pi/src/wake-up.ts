// Wake-up on session start: fetch memcastle_wake_up and inject it as established context.
//
// The request starts at `session_start`, so the daemon has a head start on the user's first prompt, and it is handed
// to the agent from `before_agent_start`, the one hook that runs after the prompt and before the model is called:
//   - `sync` waits there (bounded by the call timeout), so the first response is written with the briefing;
//   - `async` injects only if the answer is already in, and otherwise a later prompt picks it up.
// The briefing goes in as a custom message, which Pi keeps in the session history, so it is injected exactly once
// and stays in context for the rest of the session without being sent again.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "./mcp-manager.ts"
import { PendingWakeUp, fetchWakeUp, wingFor } from "./wake-up-core.ts"

/** Names the injected message, so Pi's UI and other extensions can tell it apart from the user's own. */
export const WAKE_UP_MESSAGE_TYPE = "memcastle-wake-up"

/**
 * Why Pi started a session in which the briefing is new. A resumed, reloaded or forked session already carries the
 * message an earlier start injected in its history, and injecting it again would only repeat it.
 */
const FRESH_SESSIONS: ReadonlySet<string> = new Set(["startup", "new"])

export function registerWakeUp(pi: ExtensionAPI, manager: () => McpManager | null): void {
  // The request for the current Pi session, and whether its briefing has gone into the conversation yet.
  let pending: PendingWakeUp | null = null
  let delivered = false

  // Registered after the extension's own `session_start` handler, so `manager()` is already the new session's.
  pi.on("session_start", async (event, ctx) => {
    pending = null
    delivered = false
    const current = manager()
    if (!current?.settings.wakeUp.enabled || !FRESH_SESSIONS.has(event.reason)) return

    const { settings } = current
    const wing = wingFor(settings.wakeUp, ctx.cwd)
    pending = new PendingWakeUp(
      async () => {
        // The manager's own start reports a daemon that cannot be reached, so this stays quiet about it.
        if (!(await current.ready)) return null
        const session = current.session
        return session ? fetchWakeUp(session, settings.agentIdentity, wing) : null
      },
      (error) => current.report(error, (message, level) => ctx.ui.notify(message, level)),
    )
  })

  pi.on("before_agent_start", async () => {
    const request = pending
    const current = manager()
    // No manager is an `off` session or a finished one: nothing MemCastle-derived may reach the conversation then.
    if (!request || delivered || !current) return

    const briefing = await request.available(current.settings.wakeUp.mode, current.settings.timeoutMs)
    // The session may have ended or restarted while this waited, and a briefing for another session must not leak in.
    if (briefing === null || delivered || pending !== request || manager() !== current) return
    delivered = true
    return { message: { customType: WAKE_UP_MESSAGE_TYPE, content: briefing, display: false } }
  })

  pi.on("session_shutdown", async () => {
    pending = null
    delivered = false
  })
}
