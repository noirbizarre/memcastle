import { afterAll, beforeAll, expect, test } from "bun:test"
import type { ExtensionAPI, ExtensionCommandContext, ExtensionContext } from "@earendil-works/pi-coding-agent"
import memcastle from "../src/extension.ts"
import { MemCastleFailure } from "../src/failures.ts"
import type { McpManager } from "../src/mcp-manager.ts"
import { registerWakeUp, WAKE_UP_MESSAGE_TYPE } from "../src/wake-up.ts"
import { registerWakeUpCommand } from "../src/wake-up-cli.ts"
import { DEFAULT_WAKE_UP, type WakeUpSettings } from "../src/wake-up-core.ts"
import { TestDaemon } from "./support/daemon.ts"

type Handler = (event: unknown, ctx: ExtensionContext) => unknown
type Command = { handler: (args: string, ctx: ExtensionCommandContext) => Promise<void> }

/** The slice of Pi these capabilities touch: `on`, `registerCommand`, and `ui.notify`. */
function fakePi(cwd = "/work/memcastle") {
  const handlers = new Map<string, Handler[]>()
  const commands = new Map<string, Command>()
  const notes: { message: string; level: string }[] = []
  const pi = {
    on(event: string, handler: Handler) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler])
      return () => undefined
    },
    registerCommand(name: string, command: Command) {
      commands.set(name, command)
    },
  } as unknown as ExtensionAPI
  const ctx = {
    cwd,
    ui: { notify: (message: string, level: string) => notes.push({ message, level }) },
  } as unknown as ExtensionContext
  /** Fire an event at every handler in registration order, and return what the last one answered. */
  const fire = async (event: string, payload: object = {}) => {
    let answer: unknown
    for (const handler of handlers.get(event) ?? []) answer = await handler({ type: event, ...payload }, ctx)
    return answer as { message?: { customType: string; content: string; display: boolean } } | undefined
  }
  return { pi, ctx, notes, commands, fire }
}

/** A deferred value the test settles by hand. */
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

const RICH = {
  diary: { content: "Finished the daemon." },
  recent_highlights: [{ content: "Prefers short answers" }],
}

/** A manager whose daemon answers as `answer` says, so each test decides when and how the call resolves. */
function fakeManager(
  answer: (tool: string, args: Record<string, unknown>) => Promise<unknown>,
  options: { wakeUp?: Partial<WakeUpSettings>; ready?: Promise<boolean>; timeoutMs?: number } = {},
) {
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  const reports: unknown[] = []
  const manager = {
    settings: {
      agentIdentity: "pi",
      timeoutMs: options.timeoutMs ?? 5000,
      wakeUp: { ...DEFAULT_WAKE_UP, ...options.wakeUp },
    },
    ready: options.ready ?? Promise.resolve(true),
    session: {
      call: (tool: string, args: Record<string, unknown>) => {
        calls.push({ tool, args })
        return answer(tool, args)
      },
    },
    report: (error: unknown) => reports.push(error),
  } as unknown as McpManager
  return { manager, calls, reports }
}

/** Register wake-up against a fake Pi, with `manager` as the session's connection (or none). */
function setup(manager: McpManager | null, cwd?: string) {
  const host = fakePi(cwd)
  registerWakeUp(host.pi, () => manager)
  registerWakeUpCommand(host.pi, () => manager)
  return host
}

async function settles(promise: Promise<unknown>): Promise<boolean> {
  let settled = false
  void promise.then(
    () => (settled = true),
    () => (settled = true),
  )
  await Bun.sleep(30)
  return settled
}

// --- timing ---------------------------------------------------------------------------------------------------

test("in sync mode the first response waits for the wake-up call to resolve, then carries the briefing", async () => {
  const call = deferred<unknown>()
  const { manager } = fakeManager(() => call.promise, { wakeUp: { mode: "sync" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  const firstResponse = fire("before_agent_start", { prompt: "hello" })
  expect(await settles(firstResponse)).toBe(false)

  call.resolve(RICH)
  const injected = await firstResponse
  expect(injected?.message).toMatchObject({ customType: WAKE_UP_MESSAGE_TYPE, display: false })
  expect(injected?.message?.content).toContain("Finished the daemon.")
  expect(injected?.message?.content).toContain("Prefers short answers")
})

test("in async mode the first response does not wait, and a later one carries the briefing", async () => {
  const call = deferred<unknown>()
  const { manager } = fakeManager(() => call.promise, { wakeUp: { mode: "async" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  const firstResponse = fire("before_agent_start", { prompt: "hello" })
  expect(await settles(firstResponse)).toBe(true)
  expect(await firstResponse).toBeUndefined()

  call.resolve(RICH)
  await Bun.sleep(30)
  const later = await fire("before_agent_start", { prompt: "again" })
  expect(later?.message?.content).toContain("Finished the daemon.")
})

test("an async briefing that is already in by the first response is on that response", async () => {
  const { manager } = fakeManager(async () => RICH, { wakeUp: { mode: "async" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  await Bun.sleep(30)
  expect((await fire("before_agent_start", {}))?.message?.content).toContain("Finished the daemon.")
})

test("a sync first response stops waiting after the timeout instead of hanging on a stuck daemon", async () => {
  const { manager } = fakeManager(() => new Promise(() => undefined), { wakeUp: { mode: "sync" }, timeoutMs: 50 })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  const started = Date.now()
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(Date.now() - started).toBeLessThan(2000)
})

test("the briefing is injected once, because Pi keeps the message in the conversation", async () => {
  const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { mode: "sync" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  expect((await fire("before_agent_start", {}))?.message).toBeDefined()
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(calls).toHaveLength(1)
})

// --- what is asked --------------------------------------------------------------------------------------------

test("the call names the agent identity and the wing of the working directory", async () => {
  const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { mode: "sync" } })
  const { fire } = setup(manager, "/home/me/projects/castle")

  await fire("session_start", { reason: "startup" })
  await fire("before_agent_start", {})
  expect(calls).toEqual([{ tool: "memcastle_wake_up", args: { agent_identity: "pi", wing: "castle" } }])
})

test("each source asks about the wing it names, and none asks about no wing", async () => {
  for (const [wakeUp, args] of [
    [{ source: "user" }, { agent_identity: "pi", wing: "preferences" }],
    [{ source: "custom", wing: "notes" }, { agent_identity: "pi", wing: "notes" }],
    [{ source: "none" }, { agent_identity: "pi" }],
  ] as const) {
    const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { mode: "sync", ...wakeUp } })
    const { fire } = setup(manager, "/home/me/castle")
    await fire("session_start", { reason: "startup" })
    await fire("before_agent_start", {})
    expect(calls).toEqual([{ tool: "memcastle_wake_up", args }])
  }
})

// --- when nothing is injected ---------------------------------------------------------------------------------

test("an empty palace injects nothing and is not reported as a failure", async () => {
  const { manager, reports } = fakeManager(async () => ({ diary: null, recent_highlights: [] }), { wakeUp: { mode: "sync" } })
  const { fire, notes } = setup(manager)

  await fire("session_start", { reason: "startup" })
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(reports).toEqual([])
  expect(notes).toEqual([])
})

test("a daemon that is down does not block the session, is reported once, and injects nothing", async () => {
  const failure = new MemCastleFailure("daemon_unavailable", "MemCastle cannot be reached.", null, "Start it.")
  const { manager, reports } = fakeManager(async () => {
    throw failure
  }, { wakeUp: { mode: "sync" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(reports).toEqual([failure])
})

test("a connection the manager already failed to open is not called again, so the user is told once", async () => {
  const { manager, calls, reports } = fakeManager(async () => RICH, { wakeUp: { mode: "sync" }, ready: Promise.resolve(false) })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(calls).toEqual([])
  expect(reports).toEqual([])
})

test("disabled wake-up makes no call and injects nothing", async () => {
  const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { enabled: false, mode: "sync" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  expect(await fire("before_agent_start", {})).toBeUndefined()
  expect(calls).toEqual([])
})

test("a session without a connection, as an off session is, injects nothing and calls nothing", async () => {
  const { fire } = setup(null)
  await fire("session_start", { reason: "startup" })
  expect(await fire("before_agent_start", {})).toBeUndefined()
})

test("a briefing that arrives after the session ended is not injected into whatever comes next", async () => {
  const call = deferred<unknown>()
  const { manager } = fakeManager(() => call.promise, { wakeUp: { mode: "sync" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  const firstResponse = fire("before_agent_start", {})
  await fire("session_shutdown", { reason: "quit" })
  call.resolve(RICH)
  expect(await firstResponse).toBeUndefined()
})

test("a resumed, reloaded or forked session does not fetch again, as its history already holds the briefing", async () => {
  for (const reason of ["resume", "reload", "fork"]) {
    const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { mode: "sync" } })
    const { fire } = setup(manager)
    await fire("session_start", { reason })
    expect(await fire("before_agent_start", {})).toBeUndefined()
    expect(calls).toEqual([])
  }
})

test("a new session starts a new briefing", async () => {
  const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { mode: "sync" } })
  const { fire } = setup(manager)

  await fire("session_start", { reason: "startup" })
  expect((await fire("before_agent_start", {}))?.message).toBeDefined()
  await fire("session_shutdown", { reason: "new" })
  await fire("session_start", { reason: "new" })
  expect((await fire("before_agent_start", {}))?.message).toBeDefined()
  expect(calls).toHaveLength(2)
})

// --- the command ----------------------------------------------------------------------------------------------

test("the wake-up command shows what a session start would inject, without adding it to the conversation", async () => {
  const { manager } = fakeManager(async () => RICH)
  const { commands, ctx, notes } = setup(manager)

  await commands.get("memcastle-wake-up")?.handler("", ctx as ExtensionCommandContext)
  expect(notes).toHaveLength(1)
  expect(notes[0]?.message).toContain("Finished the daemon.")
  expect(notes[0]?.level).toBe("info")
})

test("the wake-up command works even when wake-up on session start is disabled", async () => {
  const { manager, calls } = fakeManager(async () => RICH, { wakeUp: { enabled: false } })
  const { commands, ctx, notes } = setup(manager)

  await commands.get("memcastle-wake-up")?.handler("", ctx as ExtensionCommandContext)
  expect(calls).toHaveLength(1)
  expect(notes[0]?.message).toContain("Finished the daemon.")
})

test("the wake-up command names the wing it asked about when there is nothing to show", async () => {
  const { manager } = fakeManager(async () => ({ diary: null, recent_highlights: [] }))
  const { commands, ctx, notes } = setup(manager, "/work/castle")

  await commands.get("memcastle-wake-up")?.handler("", ctx as ExtensionCommandContext)
  expect(notes[0]?.message).toContain('wing "castle"')
})

test("the wake-up command reports a failure instead of hiding it as an empty palace", async () => {
  const failure = new MemCastleFailure("daemon_unavailable", "MemCastle cannot be reached.")
  const { manager, reports } = fakeManager(async () => {
    throw failure
  })
  const { commands, ctx } = setup(manager)

  await commands.get("memcastle-wake-up")?.handler("", ctx as ExtensionCommandContext)
  expect(reports).toEqual([failure])
})

test("the wake-up command in a session without a connection says so and calls nothing", async () => {
  const { commands, ctx, notes } = setup(null)
  await commands.get("memcastle-wake-up")?.handler("", ctx as ExtensionCommandContext)
  expect(notes).toEqual([{ message: "MemCastle is not active in this session.", level: "info" }])
})

// --- against a real daemon ------------------------------------------------------------------------------------

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

/** Run the whole extension, as Pi would, against the real daemon, with `env` as the user's configuration. */
async function realSession(env: Record<string, string>, cwd: string) {
  const saved = { ...process.env }
  // The extension reads the environment at session start, exactly as a user's shell configures it.
  Object.assign(process.env, {
    MEMCASTLE_PALACE_PATH: daemon.palacePath,
    HOME: daemon.clientEnv.HOME,
    XDG_STATE_HOME: daemon.clientEnv.XDG_STATE_HOME,
    ...env,
  })
  const host = fakePi(cwd)
  memcastle(host.pi)
  const restore = () => {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
    Object.assign(process.env, saved)
  }
  return { ...host, restore }
}

test("a real daemon's diary for the project wing is in the first response of a synchronous session", async () => {
  const writer = daemon.session("full")
  await writer.call("memcastle_diary_write", {
    agent_identity: "pi",
    wing: "real-project",
    content: "Decided to ship wake-up on Friday.",
  })
  await writer.close()

  const { fire, restore } = await realSession({ MEMCASTLE_WAKE_UP_MODE: "sync" }, "/work/real-project")
  try {
    await fire("session_start", { reason: "startup" })
    const injected = await fire("before_agent_start", { prompt: "hello" })
    expect(injected?.message?.content).toContain("Decided to ship wake-up on Friday.")
  } finally {
    await fire("session_shutdown", { reason: "quit" })
    restore()
  }
})

test("a real daemon with nothing remembered for the project injects nothing", async () => {
  const { fire, notes, restore } = await realSession({ MEMCASTLE_WAKE_UP_MODE: "sync" }, "/work/unknown-project")
  try {
    await fire("session_start", { reason: "startup" })
    expect(await fire("before_agent_start", { prompt: "hello" })).toBeUndefined()
    expect(notes).toEqual([])
  } finally {
    await fire("session_shutdown", { reason: "quit" })
    restore()
  }
})

test("a read-only session still wakes up, and an off session injects nothing even with a diary waiting", async () => {
  const readOnly = await realSession({ MEMCASTLE_WAKE_UP_MODE: "sync", MEMCASTLE_MODE: "read-only" }, "/work/real-project")
  try {
    await readOnly.fire("session_start", { reason: "startup" })
    const injected = await readOnly.fire("before_agent_start", {})
    expect(injected?.message?.content).toContain("Decided to ship wake-up on Friday.")
  } finally {
    await readOnly.fire("session_shutdown", { reason: "quit" })
    readOnly.restore()
  }

  const off = await realSession({ MEMCASTLE_WAKE_UP_MODE: "sync", MEMCASTLE_MODE: "off" }, "/work/real-project")
  try {
    await off.fire("session_start", { reason: "startup" })
    expect(await off.fire("before_agent_start", {})).toBeUndefined()
  } finally {
    await off.fire("session_shutdown", { reason: "quit" })
    off.restore()
  }
})
