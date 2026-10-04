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

/** What the plugin reads of an event: V2's union is far larger, and a malformed payload must be ignored. */
interface SessionEventData {
  sessionID?: unknown
  parentID?: unknown
  location?: { directory?: unknown }
}

/** The session id of a `session.deleted` event, or `undefined` for anything else. */
function deletedSessionId(event: { type: string; data?: unknown }): string | undefined {
  if (event.type !== "session.deleted") return undefined
  const sessionId = (event.data as SessionEventData | undefined)?.sessionID
  return typeof sessionId === "string" ? sessionId : undefined
}

/** The session, working directory and parent of a `session.created` event, or `undefined` for anything else. */
function createdSession(event: { type: string; data?: unknown }, fallbackDirectory: string) {
  if (event.type !== "session.created") return undefined
  const data = event.data as SessionEventData | undefined
  if (typeof data?.sessionID !== "string") return undefined
  return {
    sessionId: data.sessionID,
    // An event with no location is asked about the directory OpenCode started in, which is where the session runs.
    directory: typeof data.location?.directory === "string" ? data.location.directory : fallbackDirectory,
    parentId: typeof data.parentID === "string" ? data.parentID : undefined,
  }
}

export const setup = async (ctx: Plugin.Context): Promise<(() => Promise<void>) | undefined> => {
  const directory = ctx.location.directory
  const core = await createCore(ctx.options, consoleLog, process.env, directory)
  if (!core) return undefined

  // The stream ends when the cleanup aborts it, so a reloaded plugin never leaves a second subscriber behind.
  const controller = new AbortController()
  void (async () => {
    try {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const untyped = event as { type: string; data?: unknown }
        // session.created starts the wake-up fetch, which is what gives it a head start on the first request.
        // session.idle -> count turns for checkpoints (#34).
        const created = createdSession(untyped, directory)
        if (created) await core.sessionCreated(created.sessionId, created.directory, created.parentId)
        // The connection belongs to the OpenCode session, so it ends with it.
        const sessionId = deletedSessionId(untyped)
        if (sessionId) await core.sessionDeleted(sessionId)
      }
    } catch (error) {
      // An abort is the normal way for this loop to end; anything else must be reported, not thrown into the void.
      if (!controller.signal.aborted) await consoleLog("warn", `event stream ended: ${String(error)}`)
    }
  })()

  // `context` runs before every agent model request, which is where V1's system transform ran (#33, #36).
  await ctx.session.hook("context", (input) =>
    // The `context` hook is the agent's own request (the title model has a `title` hook), and `system` is rebuilt
    // for each one, so the briefing is added to every request, as in V1.
    core.systemTransform(input.sessionID, (text) => input.system.push({ type: "text", text })),
  )
  // V2's counterpart of V1's `experimental.session.compacting`: the transcript is about to be summarised (#34).
  await ctx.session.hook("compaction", () => core.compacting())
  await ctx.tool.hook("execute.before", () => core.toolBefore())

  return async () => {
    controller.abort()
    await core.dispose()
  }
}
