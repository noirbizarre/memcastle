import { afterAll, afterEach, beforeAll, beforeEach, expect, spyOn, test } from "bun:test"
import type { Hooks, PluginInput } from "@opencode-ai/plugin"
import type { Plugin } from "@opencode/plugin"
import { createCore, type Core, type Level } from "../src/core.ts"
import { MemCastleFailure } from "../src/failures.ts"
import plugin from "../src/index.ts"
import { TestDaemon } from "./support/daemon.ts"

const RICH = {
  diary: { content: "Finished the daemon." },
  recent_highlights: [{ content: "Prefers short answers" }],
}
const EMPTY = { diary: null, recent_highlights: [] }

function deferred<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((res) => {
    resolve = res
  })
  return { promise, resolve }
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

/** A core whose every session's daemon answers as `answer` says, so the test decides when a call resolves. */
async function coreWith(
  answer: (args: Record<string, unknown>) => Promise<unknown>,
  options: Record<string, unknown> = {},
  directory = "/work/started-here",
) {
  const logs: { level: Level; message: string }[] = []
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  const core = (await createCore(
    // These tests are about wake-up, so the search-before-answer reminder (tested in recall.test.ts) is switched off.
    { forceMemoryRecall: { level: "off" }, ...options },
    async (level, message) => void logs.push({ level, message }),
    {},
    directory,
  )) as Core
  core.sessions.session = (() => ({
    call: (tool: string, args: Record<string, unknown>) => {
      calls.push({ tool, args })
      return answer(args)
    },
    close: async () => undefined,
  })) as never
  /** One model request: what the transform adds to its system prompt. */
  const request = async (sessionId: string | undefined) => {
    const system: string[] = []
    await core.systemTransform(sessionId, (text) => system.push(text))
    return system
  }
  return { core, logs, calls, request }
}

// --- timing ---------------------------------------------------------------------------------------------------

test("in sync mode the first response waits for the wake-up call to resolve, then carries the briefing", async () => {
  const call = deferred<unknown>()
  const { core, request } = await coreWith(() => call.promise, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  const firstResponse = request("ses_1")
  expect(await settles(firstResponse)).toBe(false)

  call.resolve(RICH)
  const system = await firstResponse
  expect(system.join("\n")).toContain("Finished the daemon.")
  expect(system.join("\n")).toContain("Prefers short answers")
})

test("in async mode the first response does not wait, and a later one carries the briefing", async () => {
  const call = deferred<unknown>()
  const { core, request } = await coreWith(() => call.promise, { wakeUp: { mode: "async" } })

  await core.sessionCreated("ses_1", "/work/castle")
  const firstResponse = request("ses_1")
  expect(await settles(firstResponse)).toBe(true)
  expect(await firstResponse).toEqual([])

  call.resolve(RICH)
  await Bun.sleep(30)
  expect((await request("ses_1")).join("\n")).toContain("Finished the daemon.")
})

test("a sync first response stops waiting after the timeout instead of hanging on a stuck daemon", async () => {
  const { core, request } = await coreWith(() => new Promise(() => undefined), { timeoutMs: 50, wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  const started = Date.now()
  expect(await request("ses_1")).toEqual([])
  expect(Date.now() - started).toBeLessThan(2000)
})

test("every request of the session carries the briefing, because OpenCode rebuilds the system prompt each time", async () => {
  const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  for (let turn = 0; turn < 3; turn++) expect((await request("ses_1")).join("\n")).toContain("Finished the daemon.")
  // The answer is fetched once per session and only re-injected: the daemon is not asked again per request.
  expect(calls).toHaveLength(1)
})

// --- what is asked --------------------------------------------------------------------------------------------

test("the call names the agent identity and the wing of the session's directory", async () => {
  const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/home/me/projects/castle")
  await request("ses_1")
  expect(calls).toEqual([{ tool: "memcastle_wake_up", args: { agent_identity: "opencode", wing: "castle" } }])
})

test("each source asks about the wing it names, and none asks about no wing", async () => {
  for (const [wakeUp, args] of [
    [{ source: "user" }, { agent_identity: "opencode", wing: "preferences" }],
    [{ source: "custom", wing: "notes" }, { agent_identity: "opencode", wing: "notes" }],
    [{ source: "none" }, { agent_identity: "opencode" }],
  ] as const) {
    const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync", ...wakeUp } })
    await core.sessionCreated("ses_1", "/home/me/castle")
    await request("ses_1")
    expect(calls).toEqual([{ tool: "memcastle_wake_up", args }])
  }
})

test("a session that began before the plugin loaded still wakes up, in the directory OpenCode started in", async () => {
  const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } }, "/work/started-here")

  // No `session.created` ever fired for it: this is a resumed session.
  expect((await request("ses_resumed")).join("\n")).toContain("Finished the daemon.")
  expect(calls[0]?.args.wing).toBe("started-here")
  await core.sessionDeleted("ses_resumed")
})

// --- when nothing is injected ---------------------------------------------------------------------------------

test("an empty palace injects nothing and is not reported as a failure", async () => {
  const { core, request, logs } = await coreWith(async () => EMPTY, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  expect(await request("ses_1")).toEqual([])
  expect(logs.filter((entry) => entry.level !== "info")).toEqual([])
})

test("a daemon that is down does not block the session, is reported once, and injects nothing", async () => {
  const { core, request, logs } = await coreWith(async () => {
    throw new MemCastleFailure("daemon_unavailable", "MemCastle cannot be reached.", null, "Start it.")
  }, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  expect(await request("ses_1")).toEqual([])
  expect(await request("ses_1")).toEqual([])
  const warnings = logs.filter((entry) => entry.level === "warn")
  expect(warnings).toHaveLength(1)
  expect(warnings[0]?.message).toContain("cannot be reached")
  expect(warnings[0]?.message).toContain("Start it.")
})

test("disabled wake-up makes no call and injects nothing", async () => {
  const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { enabled: false, mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  expect(await request("ses_1")).toEqual([])
  expect(calls).toEqual([])
})

test("a request with no session id is left alone, because there is no session to brief", async () => {
  const { request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } })
  expect(await request(undefined)).toEqual([])
  expect(calls).toEqual([])
})

test("a subagent's session gets no briefing and opens no connection of its own", async () => {
  const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_child", "/work/castle", "ses_parent")
  expect(await request("ses_child")).toEqual([])
  expect(calls).toEqual([])
})

test("a session deleted the moment it is created never has its connection reopened by its own wake-up", async () => {
  const { core, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } })

  // Not awaited in between: the request is scheduled by the first call and would run after the second.
  const created = core.sessionCreated("ses_1", "/work/castle")
  const deleted = core.sessionDeleted("ses_1")
  await Promise.all([created, deleted])
  await Bun.sleep(30)
  expect(calls).toEqual([])
})

test("a deleted session is forgotten, so a request for it afterwards starts a fresh wake-up", async () => {
  const { core, request, calls } = await coreWith(async () => RICH, { wakeUp: { mode: "sync" } })

  await core.sessionCreated("ses_1", "/work/castle")
  await request("ses_1")
  await core.sessionDeleted("ses_1")
  await request("ses_1")
  expect(calls).toHaveLength(2)
})

test("an off session has no core at all, so nothing is ever fetched or injected", async () => {
  expect(await createCore({ mode: "off" }, async () => undefined, {}, "/work/castle")).toBeUndefined()
})

// --- the two hosts' wiring ------------------------------------------------------------------------------------

const saved = { ...process.env }
beforeEach(() => {
  // OpenCode 2 reports through the console; keep the test output readable.
  spyOn(console, "info").mockImplementation(() => undefined)
})
afterEach(() => {
  ;(console.info as unknown as { mockRestore?: () => void }).mockRestore?.()
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
})

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

/** Make the plugin find the real daemon the way a user's environment would. */
function pointAtDaemon(env: Record<string, string> = {}) {
  Object.assign(process.env, {
    MEMCASTLE_PALACE_PATH: daemon.palacePath,
    HOME: daemon.clientEnv.HOME,
    XDG_STATE_HOME: daemon.clientEnv.XDG_STATE_HOME,
    // These tests are about wake-up, so the search-before-answer reminder (tested in recall.test.ts) is switched off.
    MEMCASTLE_FORCE_MEMORY_RECALL: "off",
    ...env,
  })
}

async function writeDiary(wing: string, content: string) {
  const writer = daemon.session("full")
  await writer.call("memcastle_diary_write", { agent_identity: "opencode", wing, content })
  await writer.close()
}

function v1Input(directory: string): PluginInput {
  const client = { app: { log: async () => ({}) } }
  return { client, directory } as unknown as PluginInput
}

test("OpenCode 1: session.created starts the fetch and the system transform adds a real daemon's diary", async () => {
  await writeDiary("v1-project", "Decided to ship wake-up on Friday.")
  pointAtDaemon({ MEMCASTLE_WAKE_UP_MODE: "sync" })
  const hooks: Hooks = await plugin.server(v1Input("/work/v1-project"))

  await hooks.event?.({
    event: { type: "session.created", properties: { info: { id: "ses_1", directory: "/work/v1-project" } } } as never,
  })
  const output = { system: [] as string[] }
  await hooks["experimental.chat.system.transform"]?.({ sessionID: "ses_1", model: {} as never }, output)

  expect(output.system.join("\n")).toContain("Decided to ship wake-up on Friday.")
  await hooks.dispose?.()
})

test("OpenCode 1: a session with nothing remembered for its project gets an untouched system prompt", async () => {
  pointAtDaemon({ MEMCASTLE_WAKE_UP_MODE: "sync" })
  const hooks = await plugin.server(v1Input("/work/never-used"))

  await hooks.event?.({
    event: { type: "session.created", properties: { info: { id: "ses_1", directory: "/work/never-used" } } } as never,
  })
  const output = { system: ["base prompt"] }
  await hooks["experimental.chat.system.transform"]?.({ sessionID: "ses_1", model: {} as never }, output)

  expect(output.system).toEqual(["base prompt"])
  await hooks.dispose?.()
})

test("OpenCode 1: a read-only session still wakes up, and an off session registers no hook to inject with", async () => {
  await writeDiary("v1-project", "Decided to ship wake-up on Friday.")
  pointAtDaemon({ MEMCASTLE_WAKE_UP_MODE: "sync", MEMCASTLE_MODE: "read-only" })
  const readOnly = await plugin.server(v1Input("/work/v1-project"))
  await readOnly.event?.({
    event: { type: "session.created", properties: { info: { id: "ses_1", directory: "/work/v1-project" } } } as never,
  })
  const output = { system: [] as string[] }
  await readOnly["experimental.chat.system.transform"]?.({ sessionID: "ses_1", model: {} as never }, output)
  expect(output.system.join("\n")).toContain("Decided to ship wake-up on Friday.")
  await readOnly.dispose?.()

  pointAtDaemon({ MEMCASTLE_MODE: "off" })
  expect(Object.keys(await plugin.server(v1Input("/work/v1-project")))).toEqual([])
})

/** A V2 context that records its hooks and lets the test push events into the subscription. */
function v2Context() {
  const callbacks = new Map<string, (input: unknown) => Promise<void> | void>()
  const queue: unknown[] = []
  let wake: (() => void) | undefined
  const ctx = {
    options: {},
    location: { directory: "/work/started-here" },
    event: {
      subscribe: async function* (opts?: { signal?: AbortSignal }) {
        while (!opts?.signal?.aborted) {
          if (queue.length === 0) {
            await new Promise<void>((resolve) => {
              wake = resolve
              opts?.signal?.addEventListener("abort", () => resolve())
            })
          }
          while (queue.length > 0) yield queue.shift()
        }
      },
    },
    session: { hook: async (name: string, callback: (input: unknown) => void) => (callbacks.set(name, callback), { dispose: async () => undefined }) },
    tool: { hook: async () => ({ dispose: async () => undefined }), transform: async () => ({ dispose: async () => undefined }) },
    command: { transform: async () => ({ dispose: async () => undefined }) },
    skill: { transform: async () => ({ dispose: async () => undefined }) },
  } as unknown as Plugin.Context
  return {
    ctx,
    push: (event: unknown) => (queue.push(event), wake?.()),
    context: async (input: unknown) => void (await callbacks.get("context")?.(input)),
  }
}

test("OpenCode 2: session.created starts the fetch and the context hook adds a real daemon's diary", async () => {
  await writeDiary("v2-project", "Switched the schema to SurrealKit.")
  pointAtDaemon({ MEMCASTLE_WAKE_UP_MODE: "sync" })
  const fake = v2Context()
  const cleanup = await plugin.setup(fake.ctx)

  fake.push({
    type: "session.created",
    data: { sessionID: "ses_1", projectID: "p", location: { directory: "/work/v2-project" } },
  })
  await Bun.sleep(50)
  const input = { sessionID: "ses_1", system: [] as { type: "text"; text: string }[] }
  await fake.context(input)

  expect(input.system).toHaveLength(1)
  expect(input.system[0]?.type).toBe("text")
  expect(input.system[0]?.text).toContain("Switched the schema to SurrealKit.")
  await cleanup?.()
})

test("OpenCode 2: a subagent's session created event gets no briefing", async () => {
  await writeDiary("v2-project", "Switched the schema to SurrealKit.")
  pointAtDaemon({ MEMCASTLE_WAKE_UP_MODE: "sync" })
  const fake = v2Context()
  const cleanup = await plugin.setup(fake.ctx)

  fake.push({
    type: "session.created",
    data: { sessionID: "ses_child", parentID: "ses_1", location: { directory: "/work/v2-project" } },
  })
  await Bun.sleep(50)
  const input = { sessionID: "ses_child", system: [] as unknown[] }
  await fake.context(input)

  expect(input.system).toEqual([])
  await cleanup?.()
})

test("OpenCode 2: a created event with no usable session id is ignored", async () => {
  pointAtDaemon({ MEMCASTLE_WAKE_UP_MODE: "sync" })
  const fake = v2Context()
  const cleanup = await plugin.setup(fake.ctx)
  fake.push({ type: "session.created" })
  fake.push({ type: "session.created", data: { sessionID: 7 } })
  await Bun.sleep(30)
  await cleanup?.()
})
