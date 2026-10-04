// The host-agnostic half of the plugin: what MemCastle does at each OpenCode lifecycle point.
//
// OpenCode 1 and OpenCode 2 expose different plugin APIs (`server()` returning hooks, `setup(ctx)` registering them)
// but the same lifecycle points. Keeping the behaviour here, and only the wiring in `v1.ts` and `v2.ts`, means a
// follow-up issue (#33-#36) fills a stub in once and both majors get it.

import { MemCastleFailure } from "./failures.ts"
import { InvalidModeError } from "./modes.ts"
import { SessionRegistry } from "./registry.ts"
import { describeSettings, resolveSettings } from "./settings.ts"
import { PendingWakeUp, fetchWakeUp, wingFor } from "./wake-up-core.ts"

export type Level = "debug" | "info" | "warn" | "error"

/** Where the plugin reports to; each host supplies its own, and a log must never be the reason a hook fails. */
export type Log = (level: Level, message: string, extra?: Record<string, unknown>) => Promise<void>

export interface Core {
  readonly sessions: SessionRegistry
  /**
   * An OpenCode session began in `directory`: start fetching its wake-up, so the daemon has a head start on the
   * first model request. A session with a `parentID` is a subagent's and gets no briefing of its own.
   */
  sessionCreated(sessionId: string, directory: string, parentId?: string): Promise<void>
  /** The OpenCode session ended, so its MCP connection ends with it. */
  sessionDeleted(sessionId: string): Promise<void>
  /**
   * A model request is being built: add the session's wake-up to its system prompt through `inject` (#33), and the
   * search-before-answer reminder (#36). `sessionId` is optional because OpenCode 1 types it that way.
   */
  systemTransform(sessionId: string | undefined, inject: (text: string) => void): Promise<void>
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
  // Where OpenCode was started: the directory of a session that began before the plugin loaded, which never
  // fired a `session.created` to say where it is.
  directory: string = process.cwd(),
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

  const report = async (name: string, error: unknown) => {
    const message = error instanceof MemCastleFailure ? error.toUserMessage() : String(error)
    // Logging is itself best effort: a failing host logger must not turn a reported failure into a thrown one.
    await log("warn", `${name}: ${message}`).catch(() => undefined)
  }

  const guarded =
    <Args extends unknown[]>(name: string, body: (...args: Args) => Promise<void>) =>
    async (...args: Args): Promise<void> => {
      try {
        await body(...args)
      } catch (error) {
        await report(name, error)
      }
    }

  // One wake-up request per OpenCode session, started once and read by every model request of that session.
  const wakeUps = new Map<string, PendingWakeUp>()
  // Subagent sessions: each would otherwise open its own connection and repeat the briefing the parent already has.
  const children = new Set<string>()

  const startWakeUp = (sessionId: string, sessionDirectory: string) => {
    if (!settings.wakeUp.enabled || wakeUps.has(sessionId)) return
    const wing = wingFor(settings.wakeUp, sessionDirectory)
    wakeUps.set(
      sessionId,
      new PendingWakeUp(
        async () => {
          // A session deleted before this runs must not have its connection recreated by a request nobody will read.
          if (!wakeUps.has(sessionId)) return null
          return fetchWakeUp(sessions.session(sessionId), settings.agentIdentity, wing)
        },
        // A failed wake-up is reported once here, and the session carries on without it: a down daemon is not an
        // empty palace, so the user is told, but it must never stop OpenCode from answering.
        (error) => report("wake-up", error),
      ),
    )
  }

  return {
    sessions,
    sessionCreated: guarded("session.created", async (sessionId: string, sessionDirectory: string, parentId?: string) => {
      if (parentId !== undefined) children.add(sessionId)
      else startWakeUp(sessionId, sessionDirectory)
    }),
    sessionDeleted: guarded("session.deleted", async (sessionId: string) => {
      wakeUps.delete(sessionId)
      children.delete(sessionId)
      await sessions.close(sessionId)
    }),
    // OpenCode rebuilds the system prompt for every model request (the title model's included), so unlike a message
    // in a transcript the briefing must be added again each time. What is fetched once per session is the answer.
    systemTransform: guarded("system.transform", async (sessionId: string | undefined, inject: (text: string) => void) => {
      if (!settings.wakeUp.enabled || sessionId === undefined || children.has(sessionId)) return
      // A resumed session began before this plugin loaded and never fired `session.created`, so it starts here.
      startWakeUp(sessionId, directory)
      const briefing = await wakeUps.get(sessionId)?.available(settings.wakeUp.mode, settings.timeoutMs)
      if (briefing) inject(briefing)
    }),
    // `experimental.*` (V1) can change without notice; the contract allows documenting a gap if it does.
    compacting: guarded("session.compacting", async () => undefined),
    toolBefore: guarded("tool.execute.before", async () => undefined),
    dispose: guarded("dispose", async () => {
      wakeUps.clear()
      children.clear()
      await sessions.closeAll()
    }),
  }
}
