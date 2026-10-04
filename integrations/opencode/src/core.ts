// The host-agnostic half of the plugin: what MemCastle does at each OpenCode lifecycle point.
//
// OpenCode 1 and OpenCode 2 expose different plugin APIs (`server()` returning hooks, `setup(ctx)` registering them)
// but the same lifecycle points. Keeping the behaviour here, and only the wiring in `v1.ts` and `v2.ts`, means a
// follow-up issue (#33-#36) fills a stub in once and both majors get it.

import { MemCastleFailure } from "./failures.ts"
import { InvalidModeError } from "./modes.ts"
import { SessionRegistry } from "./registry.ts"
import { describeSettings, resolveSettings } from "./settings.ts"

export type Level = "debug" | "info" | "warn" | "error"

/** Where the plugin reports to; each host supplies its own, and a log must never be the reason a hook fails. */
export type Log = (level: Level, message: string, extra?: Record<string, unknown>) => Promise<void>

export interface Core {
  readonly sessions: SessionRegistry
  /** The OpenCode session ended, so its MCP connection ends with it. */
  sessionDeleted(sessionId: string): Promise<void>
  /** Inject the cached wake-up context and the search-before-answer reminder (#33, #36). */
  systemTransform(): Promise<void>
  /** Submit an emergency checkpoint just before OpenCode summarises and loses the transcript (#34). */
  compacting(): Promise<void>
  /** Refuse to load a MemCastle skill into an `off` session (#35). */
  toolBefore(): Promise<void>
  /** Close every connection, which also tells the daemon to forget each session. */
  dispose(): Promise<void>
}

/**
 * Resolve the settings and build the lifecycle handlers, or return `undefined` when the plugin must do nothing.
 *
 * Every handler is guarded: a MemCastle failure is reported through `log` and never breaks the user's OpenCode session.
 */
export async function createCore(
  options: Record<string, unknown> | undefined,
  log: Log,
  env: Readonly<Record<string, string | undefined>> = process.env,
): Promise<Core | undefined> {
  let settings
  try {
    settings = resolveSettings(options, env)
  } catch (error) {
    // Fail closed: a mode the user mistyped must not become `full`, so the plugin does nothing at all.
    const detail = error instanceof InvalidModeError ? error.message : String(error)
    await log("error", `MemCastle is disabled for this OpenCode process: ${detail}`)
    return undefined
  }

  if (settings.mode === "off") {
    // An `off` session behaves as if MemCastle did not exist: no connection, no context, no skill (#35).
    await log("info", "MemCastle is off for this OpenCode process; the plugin is inactive.")
    return undefined
  }

  const sessions = new SessionRegistry(settings, env)
  await log("info", "MemCastle plugin ready.", describeSettings(settings))

  const guarded =
    <Args extends unknown[]>(name: string, body: (...args: Args) => Promise<void>) =>
    async (...args: Args): Promise<void> => {
      try {
        await body(...args)
      } catch (error) {
        const message = error instanceof MemCastleFailure ? error.toUserMessage() : String(error)
        // Logging is itself best effort: a failing host logger must not turn a reported failure into a thrown one.
        await log("warn", `${name}: ${message}`).catch(() => undefined)
      }
    }

  return {
    sessions,
    sessionDeleted: guarded("session.deleted", (sessionId: string) => sessions.close(sessionId)),
    // This runs once per model request, including the title model's, so the answer is computed once per session.
    systemTransform: guarded("system.transform", async () => undefined),
    // `experimental.*` (V1) can change without notice; the contract allows documenting a gap if it does.
    compacting: guarded("session.compacting", async () => undefined),
    toolBefore: guarded("tool.execute.before", async () => undefined),
    dispose: guarded("dispose", () => sessions.closeAll()),
  }
}
