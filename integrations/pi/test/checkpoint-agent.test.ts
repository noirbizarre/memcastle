import { afterAll, beforeAll, expect, test } from "bun:test"
import type { ExtensionAPI, ExtensionCommandContext, ExtensionContext } from "@earendil-works/pi-coding-agent"
import { CheckpointSessions, registerCheckpointAgent } from "../src/checkpoint-agent.ts"
import { CHECKPOINT_COMMAND, registerCheckpointTool } from "../src/checkpoint-tool.ts"
import { DEFAULT_CHECKPOINT, type CheckpointSettings } from "../src/checkpoint-core.ts"
import memcastle from "../src/extension.ts"
import { MemCastleFailure } from "../src/failures.ts"
import type { McpManager } from "../src/mcp-manager.ts"
import { TestDaemon } from "./support/daemon.ts"

type Handler = (event: unknown, ctx: ExtensionContext) => unknown
type Command = { handler: (args: string, ctx: ExtensionCommandContext) => Promise<void> }

const REPLY = JSON.stringify({ items: [{ destination: "project", content: "We chose SurrealDB.", tags: ["db"] }] })

/** A message entry as Pi's session manager returns it. */
const said = (role: string, content: unknown) => ({ type: "message", message: { role, content } })

interface HostOptions {
  /** What the model answers, or a function that decides (and may wait). */
  answer?: string | ((signal: AbortSignal | undefined) => Promise<string>)
  branch?: unknown[]
  model?: unknown
  known?: Record<string, unknown>
  stopReason?: string
}

/** The slice of Pi these capabilities touch, with a model that answers as the test says. */
function fakePi(options: HostOptions = {}) {
  const handlers = new Map<string, Handler[]>()
  const commands = new Map<string, Command>()
  const notes: { message: string; level: string }[] = []
  const statuses: (string | undefined)[] = []
  const asked: { systemPrompt?: string; prompt: string; model: unknown; signal: AbortSignal | undefined }[] = []
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
    cwd: "/work/memcastle",
    model: options.model ?? { id: "session-model" },
    ui: {
      notify: (message: string, level: string) => notes.push({ message, level }),
      setStatus: (_key: string, text: string | undefined) => statuses.push(text),
    },
    sessionManager: {
      getBranch: () =>
        options.branch ?? [
          said("user", "which database?"),
          said("assistant", [{ type: "text", text: "SurrealDB." }]),
          { type: "custom_message", message: { role: "custom", content: "injected" } },
          said("toolResult", [{ type: "text", text: "secret tool output" }]),
        ],
    },
    modelRegistry: {
      find: (provider: string, id: string) => options.known?.[`${provider}/${id}`],
      complete: async (model: unknown, context: { systemPrompt?: string; messages: { content: { text: string }[] }[] }, opts: { signal?: AbortSignal }) => {
        asked.push({ systemPrompt: context.systemPrompt, prompt: context.messages[0]?.content[0]?.text ?? "", model, signal: opts.signal })
        const answer = options.answer ?? REPLY
        const text = typeof answer === "string" ? answer : await answer(opts.signal)
        return { stopReason: options.stopReason ?? "stop", errorMessage: "quota exceeded", content: [{ type: "text", text }] }
      },
    },
  } as unknown as ExtensionCommandContext
  const fire = async (event: string, payload: object = {}) => {
    for (const handler of handlers.get(event) ?? []) await handler({ type: event, ...payload }, ctx)
  }
  return { pi, ctx, notes, statuses, asked, commands, handlers, fire }
}

/** A manager whose daemon answers a checkpoint and then reports the job as `completed`, and records every call. */
function fakeManager(checkpoint: Partial<CheckpointSettings> = {}, options: { mode?: string; ready?: boolean; job?: object } = {}) {
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  const reports: MemCastleFailure[] = []
  const manager = {
    settings: {
      mode: options.mode ?? "full",
      agentIdentity: "pi",
      timeoutMs: 5000,
      checkpoint: { ...DEFAULT_CHECKPOINT, ...checkpoint },
    },
    ready: Promise.resolve(options.ready ?? true),
    session: {
      call: async (tool: string, args: Record<string, unknown>) => {
        calls.push({ tool, args })
        return { id: "job-1", status: "completed", result: { items: 1, duplicates: 0 }, ...options.job }
      },
    },
    report: (error: MemCastleFailure) => reports.push(error),
  } as unknown as McpManager
  return { manager, calls, reports }
}

function setup(manager: McpManager | null, options: HostOptions = {}) {
  const host = fakePi(options)
  const sessions = new CheckpointSessions()
  registerCheckpointAgent(host.pi, () => manager, sessions)
  registerCheckpointTool(host.pi, () => manager, sessions)
  return { ...host, sessions }
}

async function until(condition: () => boolean, what: string) {
  const deadline = Date.now() + 5000
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await Bun.sleep(5)
  }
}

const submissions = <T extends { tool: string }>(calls: T[]) => calls.filter((call) => call.tool === "memcastle_checkpoint")

// --- interval --------------------------------------------------------------------------------------------------

test("a review runs on every Nth finished exchange and not before", async () => {
  const { manager, calls } = fakeManager({ interval: 2, mode: "blocking" })
  const { fire, asked } = setup(manager)

  await fire("agent_end")
  expect(asked).toEqual([])

  await fire("agent_end")
  expect(asked).toHaveLength(1)
  expect(submissions(calls)).toHaveLength(1)
})

test("the review reads what was said, and nothing a tool printed or the extension injected", async () => {
  const { manager } = fakeManager({ interval: 1, mode: "blocking" })
  const { fire, asked } = setup(manager)
  await fire("agent_end")

  expect(asked[0]?.prompt).toContain("User: which database?")
  expect(asked[0]?.prompt).toContain("Assistant: SurrealDB.")
  expect(asked[0]?.prompt).not.toContain("secret tool output")
  expect(asked[0]?.prompt).not.toContain("injected")
  // The shared skill is the reviewing model's instruction, read from skills/ and never copied.
  expect(asked[0]?.systemPrompt).toContain("Checkpoint durable context")
})

test("a submission carries the item, this agent's identity, and no fact", async () => {
  const { manager, calls } = fakeManager({ interval: 1, mode: "blocking" })
  const { fire } = setup(manager)
  await fire("agent_end")
  expect(submissions(calls)[0]?.args).toEqual({
    emergency: false,
    payload: {
      items: [
        {
          destination: "project",
          wing: null,
          name: null,
          content: "We chose SurrealDB.",
          tags: ["db"],
          source: { kind: "manual", uri: null, agent: "pi" },
          fact: null,
        },
      ],
    },
  })
})

test("in silent mode the agent never waits for the review, and a success says nothing", async () => {
  let release!: (reply: string) => void
  const slow = new Promise<string>((resolve) => (release = resolve))
  const { manager, calls } = fakeManager({ interval: 1, mode: "silent" })
  const { fire, notes, statuses } = setup(manager, { answer: () => slow })

  await fire("agent_end") // resolves while the model is still thinking
  expect(calls).toEqual([])

  release(REPLY)
  await until(() => submissions(calls).length === 1, "the background submission")
  await Bun.sleep(20)
  expect(notes).toEqual([])
  expect(statuses).toEqual([])
})

test("in blocking mode the agent waits for the review, which is visible and reports its result", async () => {
  const { manager } = fakeManager({ interval: 1, mode: "blocking" })
  const { fire, notes, statuses } = setup(manager)
  await fire("agent_end")
  expect(statuses).toEqual(["MemCastle: checkpointing...", undefined])
  expect(notes).toEqual([{ message: "MemCastle: Checkpointed 1 item(s).", level: "info" }])
})

test("a failed review is shown to the user even in silent mode, never swallowed", async () => {
  const { manager, reports } = fakeManager({ interval: 1, mode: "silent" })
  const { fire } = setup(manager, { answer: "I have no idea" })
  await fire("agent_end")
  await until(() => reports.length === 1, "the failure report")
  expect(reports[0]?.failureClass).toBe("invalid_input")
})

test("a failed job is reported with how to retry it", async () => {
  const { manager, reports } = fakeManager({ interval: 1, mode: "blocking" }, { job: { status: "failed", error: "disk full" } })
  const { fire } = setup(manager)
  await fire("agent_end")
  expect(reports[0]?.failureClass).toBe("job_failed")
  expect(reports[0]?.toUserMessage()).toContain("memcastle_job_retry")
})

test("nothing worth keeping is quiet in silent mode and says so in blocking mode", async () => {
  const quiet = fakeManager({ interval: 1, mode: "silent" })
  const silent = setup(quiet.manager, { answer: '{"items":[]}' })
  await silent.fire("agent_end")
  await Bun.sleep(20)
  expect(silent.notes).toEqual([])

  const loud = fakeManager({ interval: 1, mode: "blocking" })
  const blocking = setup(loud.manager, { answer: '{"items":[]}' })
  await blocking.fire("agent_end")
  expect(blocking.notes[0]?.message).toContain("nothing worth keeping")
  expect(submissions(loud.calls)).toEqual([])
})

test("a disabled interval, a read-only session and an off session never review", async () => {
  for (const [manager, name] of [
    [fakeManager({ interval: 1, enabled: false, mode: "blocking" }).manager, "disabled"],
    [fakeManager({ interval: 1, mode: "blocking" }, { mode: "read-only" }).manager, "read-only"],
    [null, "off"],
  ] as const) {
    const { fire, asked, notes } = setup(manager)
    await fire("agent_end")
    await Bun.sleep(10)
    expect(asked, name).toEqual([])
    expect(notes, name).toEqual([])
  }
})

test("a daemon that never connected costs no model call and no second notification", async () => {
  const { manager } = fakeManager({ interval: 1, mode: "blocking" }, { ready: false })
  const { fire, asked, notes } = setup(manager)
  await fire("agent_end")
  expect(asked).toEqual([])
  expect(notes).toEqual([])
})

test("the configured model is used when Pi knows it, and an unknown one is reported with the setting to fix", async () => {
  const known = { id: "haiku" }
  const configured = { provider: "anthropic", id: "haiku" }
  const ok = fakeManager({ interval: 1, mode: "blocking", model: configured })
  const first = setup(ok.manager, { known: { "anthropic/haiku": known } })
  await first.fire("agent_end")
  expect(first.asked[0]?.model).toBe(known)

  const bad = fakeManager({ interval: 1, mode: "blocking", model: configured })
  const second = setup(bad.manager)
  await second.fire("agent_end")
  expect(second.asked).toEqual([])
  expect(bad.reports[0]?.toUserMessage()).toContain("MEMCASTLE_CHECKPOINT_MODEL")
})

test("a provider error is reported as such, not parsed as a malformed reply", async () => {
  const { manager, reports } = fakeManager({ interval: 1, mode: "blocking" })
  const { fire } = setup(manager, { stopReason: "error", answer: "" })
  await fire("agent_end")
  expect(reports[0]?.failureClass).toBe("unexpected")
  expect(reports[0]?.message).toContain("quota exceeded")
})

test("ending the session cancels a review in progress, and its failure is not shown to the next one", async () => {
  let seen: AbortSignal | undefined
  const { manager, reports } = fakeManager({ interval: 1, mode: "silent" })
  const { fire, asked } = setup(manager, {
    answer: (signal) => {
      seen = signal
      return new Promise<string>(() => undefined)
    },
  })
  await fire("agent_end")
  await until(() => asked.length === 1, "the model call")
  await fire("session_shutdown")
  expect(seen?.aborted).toBe(true)
  expect(reports).toEqual([])
})

test("a new session counts from zero again", async () => {
  const { manager } = fakeManager({ interval: 2, mode: "blocking" })
  const { fire, asked } = setup(manager)
  await fire("agent_end")
  await fire("session_start", { reason: "new" })
  await fire("agent_end")
  expect(asked).toEqual([])
})

// --- manual ----------------------------------------------------------------------------------------------------

const command = (host: ReturnType<typeof setup>) => host.commands.get(CHECKPOINT_COMMAND) as Command

test("the manual command saves out of cycle, passes the user's words on, and reports the result", async () => {
  const { manager, calls } = fakeManager({ enabled: false })
  const host = setup(manager)
  await command(host).handler("remember the db choice", host.ctx as ExtensionCommandContext)

  expect(host.asked[0]?.prompt).toContain("remember the db choice")
  expect(submissions(calls)).toHaveLength(1)
  expect(host.notes).toEqual([{ message: "MemCastle: Checkpointed 1 item(s).", level: "info" }])
  expect(host.statuses).toEqual(["MemCastle: checkpointing...", undefined])
})

test("a manual checkpoint postpones the next interval review", async () => {
  const { manager } = fakeManager({ interval: 2, mode: "blocking" })
  const host = setup(manager)
  await host.fire("agent_end")
  await command(host).handler("", host.ctx as ExtensionCommandContext)
  const reviewed = host.asked.length
  await host.fire("agent_end")
  expect(host.asked).toHaveLength(reviewed)
})

test("a manual checkpoint in a read-only session says why nothing was saved, as information", async () => {
  const { manager, reports } = fakeManager({}, { mode: "read-only" })
  const host = setup(manager)
  await command(host).handler("", host.ctx as ExtensionCommandContext)
  expect(host.asked).toEqual([])
  expect(reports[0]?.failureClass).toBe("mode_rejected")
})

test("the manual command in an off session says MemCastle is not active, and does nothing", async () => {
  const host = setup(null)
  await command(host).handler("", host.ctx as ExtensionCommandContext)
  expect(host.notes).toEqual([{ message: "MemCastle is not active in this session.", level: "info" }])
  expect(host.asked).toEqual([])
})

test("the manual command with no daemon says nothing was saved and how to start it", async () => {
  const { manager } = fakeManager({}, { ready: false })
  const host = setup(manager)
  await command(host).handler("", host.ctx as ExtensionCommandContext)
  expect(host.notes[0]?.level).toBe("warning")
  expect(host.notes[0]?.message).toContain("memcastle daemon start")
})

test("the manual command with nothing new says so instead of asking the model", async () => {
  const { manager } = fakeManager()
  const host = setup(manager, { branch: [] })
  await command(host).handler("", host.ctx as ExtensionCommandContext)
  expect(host.asked).toEqual([])
  expect(host.notes[0]?.message).toContain("Nothing new")
})

test("loading the extension registers the manual command and the agent_end handler", () => {
  const host = fakePi()
  memcastle(host.pi)
  expect(host.commands.has(CHECKPOINT_COMMAND)).toBe(true)
  expect(host.handlers.has("agent_end")).toBe(true)
})

// --- against a real daemon -------------------------------------------------------------------------------------

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

/** Run the whole extension, as Pi would, against the real daemon, with `env` as the user's configuration. */
async function realSession(env: Record<string, string>, options: HostOptions = {}) {
  const saved = { ...process.env }
  Object.assign(process.env, {
    MEMCASTLE_PALACE_PATH: daemon.palacePath,
    HOME: daemon.clientEnv.HOME,
    XDG_STATE_HOME: daemon.clientEnv.XDG_STATE_HOME,
    MEMCASTLE_WAKE_UP: "false",
    ...env,
  })
  const host = fakePi(options)
  memcastle(host.pi)
  const restore = () => {
    for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
    Object.assign(process.env, saved)
  }
  return { ...host, restore }
}

test("a real session checkpoints every exchange it is told to, and the daemon can recall what it kept", async () => {
  const reply = JSON.stringify({ items: [{ destination: "project", content: "pizzaoventoken the oven is gas.", tags: [] }] })
  const { fire, notes, restore } = await realSession(
    { MEMCASTLE_CHECKPOINT_INTERVAL: "1", MEMCASTLE_CHECKPOINT_MODE: "blocking" },
    { answer: reply },
  )
  try {
    await fire("session_start", { reason: "startup" })
    await fire("agent_end")
    expect(notes.map((note) => note.message)).toEqual(["MemCastle: Checkpointed 1 item(s)."])

    const reader = daemon.session("full")
    expect(JSON.stringify(await reader.call("memcastle_recall", { query: "pizzaoventoken" }))).toContain("pizzaoventoken")
    await reader.close()
  } finally {
    await fire("session_shutdown", { reason: "quit" })
    restore()
  }
})

test("a real read-only session never reviews on its own, and tells the user why when asked to", async () => {
  const { fire, commands, ctx, asked, notes, restore } = await realSession({
    MEMCASTLE_MODE: "read-only",
    MEMCASTLE_CHECKPOINT_INTERVAL: "1",
  })
  try {
    await fire("session_start", { reason: "startup" })
    await fire("agent_end")
    expect(asked).toEqual([])

    await commands.get(CHECKPOINT_COMMAND)?.handler("", ctx as ExtensionCommandContext)
    expect(asked).toEqual([])
    expect(notes.at(-1)?.level).toBe("info")
    expect(notes.at(-1)?.message).toContain("read-only")
  } finally {
    await fire("session_shutdown", { reason: "quit" })
    restore()
  }
})
