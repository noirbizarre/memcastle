// The MemCastle plugin for OpenCode.
//
// This file only decides *when* to talk to MemCastle; every memory operation is a call to the daemon over MCP.
// It is a foundation: the connection, mode and failure handling are real and tested, and each lifecycle hook below is
// wired but empty, with the issue that fills it in. See docs/research.md for why each hook was chosen.
//
// Only the default export is a plugin. OpenCode treats every exported function of a plugin module as a plugin, so
// helpers live in their own modules and are never re-exported from here.

import type { Hooks, Plugin, PluginModule } from "@opencode-ai/plugin"
import { MemCastleFailure } from "./failures.ts"
import { InvalidModeError } from "./modes.ts"
import { SessionRegistry } from "./registry.ts"
import { describeSettings, resolveSettings } from "./settings.ts"

const SERVICE = "memcastle"

const server: Plugin = async ({ client }, options) => {
  // Logging goes through OpenCode so it lands in its log, and it must never be the reason a hook fails.
  const log = async (level: "debug" | "info" | "warn" | "error", message: string, extra?: Record<string, unknown>) => {
    await client.app.log({ body: { service: SERVICE, level, message, extra } }).catch(() => undefined)
  }

  let settings
  try {
    settings = resolveSettings(options)
  } catch (error) {
    // Fail closed: a mode the user mistyped must not become `full`, so the plugin does nothing at all.
    const detail = error instanceof InvalidModeError ? error.message : String(error)
    await log("error", `MemCastle is disabled for this OpenCode process: ${detail}`)
    return {}
  }

  if (settings.mode === "off") {
    // An `off` session behaves as if MemCastle did not exist: no connection, no context, no skill (#35).
    await log("info", "MemCastle is off for this OpenCode process; the plugin is inactive.")
    return {}
  }

  const sessions = new SessionRegistry(settings)
  await log("info", "MemCastle plugin ready.", describeSettings(settings))

  /** Run a hook body so that a MemCastle failure is reported and never breaks the user's OpenCode session. */
  const guarded =
    <Args extends unknown[]>(name: string, body: (...args: Args) => Promise<void>) =>
    async (...args: Args): Promise<void> => {
      try {
        await body(...args)
      } catch (error) {
        const message = error instanceof MemCastleFailure ? error.toUserMessage() : String(error)
        await log("warn", `${name}: ${message}`)
      }
    }

  const hooks: Hooks = {
    event: guarded("event", async ({ event }) => {
      // The connection belongs to the OpenCode session, so it ends with it.
      if (event.type === "session.deleted") await sessions.close(event.properties.info.id)
      // session.created -> start the wake-up fetch (#33). session.idle -> count turns for checkpoints (#34).
    }),

    // Inject the cached wake-up context and the search-before-answer reminder (#33, #36).
    // This hook runs once per model request, including the title model's, so the answer is computed once per session.
    "experimental.chat.system.transform": guarded("system.transform", async () => undefined),

    // Submit an emergency checkpoint just before OpenCode summarises and loses the transcript (#34).
    // `experimental.*` can change without notice; the contract allows documenting a gap if it does.
    "experimental.session.compacting": guarded("session.compacting", async () => undefined),

    // Refuse to load a MemCastle skill into an `off` session (#35).
    "tool.execute.before": guarded("tool.execute.before", async () => undefined),

    // Close every connection, which also tells the daemon to forget each session.
    dispose: async () => {
      await sessions.closeAll()
    },
  }
  return hooks
}

const plugin: PluginModule = { id: "memcastle", server }

export default plugin
