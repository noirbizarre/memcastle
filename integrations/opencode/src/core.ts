// The host-agnostic half of the plugin: what MemCastle does at each OpenCode lifecycle point.
//
// OpenCode 1 and OpenCode 2 expose different plugin APIs (`server()` returning hooks, `setup(ctx)` registering them)
// but the same lifecycle points. Keeping the behaviour here, and only the wiring in `v1.ts` and `v2.ts`, means a
// follow-up issue (#33-#36) fills a stub in once and both majors get it.

import { resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { type CheckpointArgs, type Checkpoints, type ReviewHost, createCheckpoints } from "./checkpoint.ts"
import type { Turn } from "./checkpoint-core.ts"
import { MemCastleFailure } from "./failures.ts"
import { InvalidModeError } from "./modes.ts"
import { recallInstruction } from "./recall-core.ts"
import { SessionRegistry } from "./registry.ts"
import { describeSettings, resolveSettings } from "./settings.ts"
import { SKILLS_DIR, readSkill } from "./skill-text.ts"
import { PendingWakeUp, fetchWakeUp, wingFor } from "./wake-up-core.ts"

/** The shared skill whose body is re-stated in every request's system prompt (#36). */
export const SEARCH_BEFORE_ANSWER = "search-before-answer"

export type Level = "debug" | "info" | "warn" | "error"

/** Where the plugin reports to; each host supplies its own, and a log must never be the reason a hook fails. */
export type Log = (level: Level, message: string, extra?: Record<string, unknown>) => Promise<void>

export interface Core {
  readonly sessions: SessionRegistry
  /**
   * The repository's `skills/` directory, as an absolute path, for OpenCode's native skill discovery. Skills are
   * loaded from there and never copied, so `search-before-answer` and `checkpoint-instructions` stay the one text.
   */
  readonly skillsDir: string
  /**
   * An OpenCode session began in `directory`: start fetching its wake-up, so the daemon has a head start on the
   * first model request. A session with a `parentID` is a subagent's and gets no briefing of its own.
   */
  sessionCreated(sessionId: string, directory: string, parentId?: string): Promise<void>
  /** The OpenCode session ended, so its MCP connection ends with it. */
  sessionDeleted(sessionId: string): Promise<void>
  /**
   * A model request is being built: add the session's wake-up to its system prompt through `inject` (#33), and the
   * search-before-answer reminder (#36), which does not depend on the wake-up. `sessionId` is optional because
   * OpenCode 1 types it that way.
   */
  systemTransform(sessionId: string | undefined, inject: (text: string) => void): Promise<void>
  /**
   * The agent finished a run in `sessionId` (`session.idle`): count one exchange, and review the conversation when
   * the interval is reached (#34). OpenCode has no timer hook, so counting idle events is the closest equivalent.
   */
  sessionIdle(sessionId: string): Promise<void>
  /**
   * Submit an emergency checkpoint just before OpenCode summarises and loses the transcript (#34). `transcript` is the
   * conversation when the host already holds it. Waits for the review for a bounded time and never throws, so the
   * compaction always goes ahead.
   */
  compacting(sessionId: string | undefined, transcript?: readonly Turn[]): Promise<void>
  /** The `memcastle_checkpoint` tool: a manual checkpoint, with the model's own payload or the plugin's review (#34). */
  checkpoint(sessionId: string, args: CheckpointArgs): Promise<string>
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
  // How to read a session's conversation and ask a model about it. Without one the plugin cannot review (#34), and a
  // checkpoint can only be submitted with a payload the model wrote itself.
  host?: ReviewHost,
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

  const checkpoints: Checkpoints = createCheckpoints({ settings, sessions, children, host, log, report })

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

  const injectWakeUp = guarded("system.transform", async (sessionId: string | undefined, inject: (text: string) => void) => {
    if (!settings.wakeUp.enabled || sessionId === undefined || children.has(sessionId)) return
    // A resumed session began before this plugin loaded and never fired `session.created`, so it starts here.
    startWakeUp(sessionId, directory)
    const briefing = await wakeUps.get(sessionId)?.available(settings.wakeUp.mode, settings.timeoutMs)
    if (briefing) inject(briefing)
  })

  // A skill that cannot be read is reported once, not on every request: the checkout is not coming back mid-session.
  let recallReported = false
  const injectRecall = async (sessionId: string | undefined, inject: (text: string) => void): Promise<void> => {
    // A subagent works on one delegated task and the parent session already carries the reminder.
    if (settings.forceMemoryRecall.level === "off" || (sessionId !== undefined && children.has(sessionId))) return
    let text: string | null
    try {
      text = recallInstruction(settings.forceMemoryRecall, await readSkill(SEARCH_BEFORE_ANSWER))
    } catch (error) {
      if (recallReported) return
      recallReported = true
      await report(
        "system.transform",
        `could not read the ${SEARCH_BEFORE_ANSWER} skill, so requests carry no reminder to search first: ${String(error)}`,
      )
      return
    }
    // Outside the try: a host callback that throws is not an unreadable skill, and must not be reported as one.
    if (text !== null) inject(text)
  }

  return {
    sessions,
    // Without the URL's trailing slash, so the same directory a user configured by hand compares equal.
    skillsDir: resolve(fileURLToPath(SKILLS_DIR)),
    sessionCreated: guarded("session.created", async (sessionId: string, sessionDirectory: string, parentId?: string) => {
      if (parentId !== undefined) children.add(sessionId)
      else startWakeUp(sessionId, sessionDirectory)
    }),
    sessionDeleted: guarded("session.deleted", async (sessionId: string) => {
      wakeUps.delete(sessionId)
      children.delete(sessionId)
      checkpoints.forget(sessionId)
      await sessions.close(sessionId)
    }),
    // OpenCode rebuilds the system prompt for every model request (the title model's included), so unlike a message
    // in a transcript the briefing must be added again each time. What is fetched once per session is the answer.
    // The reminder is a separate step so that a disabled, slow or failing wake-up never costs the session its reminder.
    systemTransform: async (sessionId: string | undefined, inject: (text: string) => void) => {
      await injectWakeUp(sessionId, inject)
      await injectRecall(sessionId, inject)
    },
    sessionIdle: guarded("session.idle", (sessionId: string) => checkpoints.sessionIdle(sessionId)),
    // `experimental.*` (V1) can change without notice; the contract allows documenting a gap if it does.
    // Guarded because a compaction must go ahead whatever happens here.
    compacting: guarded("session.compacting", (sessionId: string | undefined, transcript?: readonly Turn[]) =>
      checkpoints.compacting(sessionId, transcript),
    ),
    // Not guarded: the tool's caller is the model, and what it needs is the failure itself, with its help.
    checkpoint: (sessionId, args) => checkpoints.checkpoint(sessionId, args),
    dispose: guarded("dispose", async () => {
      checkpoints.forgetAll()
      wakeUps.clear()
      children.clear()
      await sessions.closeAll()
    }),
  }
}
