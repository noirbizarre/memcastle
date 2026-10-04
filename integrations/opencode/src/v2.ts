// The OpenCode 2 adapter: `setup(ctx)` registers hooks on the domains that own them. Wiring only; behaviour lives in
// `core.ts`.
//
// There is no `client.app.log` in V2's plugin context, so the plugin reports through the console, which OpenCode
// captures in its own log.

import type { Plugin } from "@opencode/plugin"
import { createCore, type Level } from "./core.ts"

const consoleLog = async (level: Level, message: string, extra?: Record<string, unknown>) => {
  // `extra` is already redacted by `describeSettings`; an empty object adds nothing but noise.
  const line = `[memcastle] ${message}`
  const args = extra && Object.keys(extra).length > 0 ? [line, extra] : [line]
  console[level](...args)
}

/** The one field of a `session.deleted` event the plugin reads; the event union is far larger than that. */
function deletedSessionId(event: { type: string; data?: unknown }): string | undefined {
  if (event.type !== "session.deleted") return undefined
  const sessionId = (event.data as { sessionID?: unknown } | undefined)?.sessionID
  return typeof sessionId === "string" ? sessionId : undefined
}

export const setup = async (ctx: Plugin.Context): Promise<(() => Promise<void>) | undefined> => {
  const core = await createCore(ctx.options, consoleLog)
  if (!core) return undefined

  // The stream ends when the cleanup aborts it, so a reloaded plugin never leaves a second subscriber behind.
  const controller = new AbortController()
  void (async () => {
    try {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        // The connection belongs to the OpenCode session, so it ends with it.
        // session.created -> start the wake-up fetch (#33). session.idle -> count turns for checkpoints (#34).
        const sessionId = deletedSessionId(event as { type: string; data?: unknown })
        if (sessionId) await core.sessionDeleted(sessionId)
      }
    } catch (error) {
      // An abort is the normal way for this loop to end; anything else must be reported, not thrown into the void.
      if (!controller.signal.aborted) await consoleLog("warn", `event stream ended: ${String(error)}`)
    }
  })()

  // `context` runs before every agent model request, which is where V1's system transform ran (#33, #36).
  await ctx.session.hook("context", () => core.systemTransform())
  // V2's counterpart of V1's `experimental.session.compacting`: the transcript is about to be summarised (#34).
  await ctx.session.hook("compaction", () => core.compacting())
  await ctx.tool.hook("execute.before", () => core.toolBefore())

  return async () => {
    controller.abort()
    await core.dispose()
  }
}
