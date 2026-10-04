import { afterAll, afterEach, beforeAll, expect, test } from "bun:test"
import type { PluginInput } from "@opencode-ai/plugin"
import {
  CHECKPOINT_COMMAND,
  CHECKPOINT_TOOL,
  type ReviewHost,
  addCheckpointCommand,
  checkpointArgs,
  createCheckpoints,
} from "../src/checkpoint.ts"
import { type Caller, DEFAULT_CHECKPOINT, type CheckpointSettings, type Turn } from "../src/checkpoint-core.ts"
import { MemCastleFailure } from "../src/failures.ts"
import type { SessionRegistry } from "../src/registry.ts"
import { resolveSettings } from "../src/settings.ts"
import plugin from "../src/index.ts"
import { v1Host } from "../src/v1.ts"
import { v2Turns } from "../src/v2.ts"
import { TestDaemon } from "./support/daemon.ts"

const REPLY = JSON.stringify({ items: [{ destination: "project", content: "We chose SurrealDB.", tags: ["db"] }] })
const TURNS: Turn[] = [
  { role: "user", text: "which database?" },
  { role: "assistant", text: "SurrealDB." },
]

/** A host whose model answers as the test says, and which records what it was asked. */
function fakeHost(answer: string | ((signal: AbortSignal) => Promise<string>) = REPLY) {
  const asked: { sessionId: string; prompt: string; model: unknown; signal: AbortSignal }[] = []
  const reads: string[] = []
  const host: ReviewHost = {
    transcript: async (sessionId) => (reads.push(sessionId), TURNS),
    classify: async (sessionId, request, model, signal, claim) => {
      claim(`reviewer-of-${sessionId}`)
      asked.push({ sessionId, prompt: request.prompt, model, signal })
      return typeof answer === "string" ? answer : answer(signal)
    },
  }
  return { host, asked, reads }
}

/** A MemCastle that completes every checkpoint and records every call. */
function fakeDaemon(options: { job?: object } = {}) {
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  const caller: Caller = {
    async call<T>(tool: string, args: Record<string, unknown> = {}) {
      calls.push({ tool, args })
      return { id: "job-1", status: "completed", result: { items: 1, duplicates: 0 }, ...options.job } as T
    },
  }
  const sessions = { session: () => caller } as unknown as SessionRegistry
  return { sessions, calls }
}

const submissions = <T extends { tool: string }>(calls: T[]) => calls.filter((call) => call.tool === CHECKPOINT_TOOL)

function setup(
  options: { checkpoint?: Partial<CheckpointSettings>; mode?: string; host?: ReviewHost | null; deadlineMs?: number; job?: object } = {},
) {
  const settings = resolveSettings({ mode: options.mode ?? "full", checkpoint: { ...DEFAULT_CHECKPOINT, interval: 1, ...options.checkpoint } }, {})
  const daemon = fakeDaemon({ job: options.job })
  const logs: { level: string; message: string }[] = []
  const reports: unknown[] = []
  const children = new Set<string>()
  const checkpoints = createCheckpoints({
    settings,
    sessions: daemon.sessions,
    children,
    host: options.host === null ? undefined : (options.host ?? fakeHost().host),
    log: async (level, message) => void logs.push({ level, message }),
    report: async (_name, error) => void reports.push(error),
    deadlineMs: options.deadlineMs,
  })
  return { checkpoints, children, logs, reports, ...daemon }
}

// --- interval --------------------------------------------------------------------------------------------------

test("a review runs on every Nth idle event of a session and not before", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints, calls } = setup({ host, checkpoint: { interval: 2, mode: "blocking" } })
  await checkpoints.sessionIdle("ses_1")
  expect(asked).toEqual([])
  await checkpoints.sessionIdle("ses_1")
  expect(asked).toHaveLength(1)
  expect(submissions(calls)[0]?.args).toMatchObject({ emergency: false, payload: { items: [{ destination: "project", source: { agent: "opencode" }, fact: null }] } })
})

test("each session counts its own idle events", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints } = setup({ host, checkpoint: { interval: 2, mode: "blocking" } })
  await checkpoints.sessionIdle("a")
  await checkpoints.sessionIdle("b")
  expect(asked).toEqual([])
})

test("in silent mode the idle hook returns before the model answers, and a success says nothing", async () => {
  let release!: (reply: string) => void
  const slow = new Promise<string>((resolve) => (release = resolve))
  const { host } = fakeHost(() => slow)
  const { checkpoints, calls, logs } = setup({ host, checkpoint: { mode: "silent" } })

  await checkpoints.sessionIdle("ses_1")
  expect(calls).toEqual([])

  release(REPLY)
  const deadline = Date.now() + 5000
  while (submissions(calls).length === 0 && Date.now() < deadline) await Bun.sleep(5)
  expect(submissions(calls)).toHaveLength(1)
  await Bun.sleep(10)
  // The outcome is logged at info (it is information, not a failure), and nothing is reported.
  expect(logs.every((entry) => entry.level === "info")).toBe(true)
})

test("in blocking mode the idle hook waits for the review to finish", async () => {
  const { checkpoints, calls, logs } = setup({ checkpoint: { mode: "blocking" } })
  await checkpoints.sessionIdle("ses_1")
  expect(submissions(calls)).toHaveLength(1)
  expect(logs.at(-1)?.message).toBe("checkpoint: Checkpointed 1 item(s).")
})

test("a failed review is reported even in silent mode, never swallowed", async () => {
  const { host } = fakeHost("I have no idea")
  const { checkpoints, reports } = setup({ host, checkpoint: { mode: "silent" } })
  await checkpoints.sessionIdle("ses_1")
  const deadline = Date.now() + 5000
  while (reports.length === 0 && Date.now() < deadline) await Bun.sleep(5)
  expect((reports[0] as MemCastleFailure).failureClass).toBe("invalid_input")
})

test("a disabled interval, a read-only session and a host that cannot review never review", async () => {
  for (const options of [{ checkpoint: { enabled: false } }, { mode: "read-only" }, { host: null }] as const) {
    const { host, asked } = fakeHost()
    const { checkpoints, calls } = setup({ ...options, host: "host" in options ? options.host : host })
    await checkpoints.sessionIdle("ses_1")
    // Turning the interval off does not turn off the checkpoint before a compaction: that is context about to be lost.
    if (!("checkpoint" in options)) await checkpoints.compacting("ses_1")
    expect(asked).toEqual([])
    expect(calls).toEqual([])
  }
})

test("turning the interval off leaves the emergency checkpoint before a compaction on", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints, calls } = setup({ host, checkpoint: { enabled: false } })
  await checkpoints.compacting("ses_1")
  expect(asked).toHaveLength(1)
  expect(submissions(calls)[0]?.args.emergency).toBe(true)
})

test("a subagent session is not reviewed: the parent already carries its conversation", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints, children } = setup({ host })
  children.add("ses_child")
  await checkpoints.sessionIdle("ses_child")
  await checkpoints.compacting("ses_child")
  expect(asked).toEqual([])
})

test("a reviewer session is claimed before it runs, so it is never reviewed or counted itself", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints, children } = setup({ host, checkpoint: { mode: "blocking" } })
  await checkpoints.sessionIdle("ses_1")
  expect(children.has("reviewer-of-ses_1")).toBe(true)

  await checkpoints.sessionIdle("reviewer-of-ses_1")
  expect(asked).toHaveLength(1)
})

test("the checkpoint tool refuses a reviewer that calls it, so a reviewer cannot skip validation", async () => {
  const { checkpoints, children, calls } = setup()
  children.add("reviewer")
  const failure = await checkpoints.checkpoint("reviewer", { payload: { items: [] } }).catch((error: unknown) => error)
  expect(failure).toBeInstanceOf(MemCastleFailure)
  expect(calls).toEqual([])
})

// --- emergency -------------------------------------------------------------------------------------------------

test("compaction submits an emergency checkpoint from the transcript it is given, without reading the session back", async () => {
  const { host, reads, asked } = fakeHost()
  const { checkpoints, calls } = setup({ host })
  await checkpoints.compacting("ses_1", [{ role: "user", text: "decision made in this very turn" }])

  expect(reads).toEqual([])
  expect(asked[0]?.prompt).toContain("decision made in this very turn")
  expect(submissions(calls)[0]?.args.emergency).toBe(true)
})

test("the emergency checkpoint is not waited for: the queued job is Critical priority and compaction goes ahead", async () => {
  const { host } = fakeHost()
  const { checkpoints, calls } = setup({ host, job: { status: "queued" } })
  await checkpoints.compacting("ses_1")
  expect(calls.map((call) => call.tool)).toEqual([CHECKPOINT_TOOL])
})

test("compaction stops waiting for a slow model after the deadline, and the review still reports for itself", async () => {
  let release!: (reply: string) => void
  const slow = new Promise<string>((resolve) => (release = resolve))
  const { host } = fakeHost(() => slow)
  const { checkpoints, calls } = setup({ host, deadlineMs: 20 })

  const started = Date.now()
  await checkpoints.compacting("ses_1")
  expect(Date.now() - started).toBeLessThan(2000)
  expect(calls).toEqual([])

  release(REPLY)
  const deadline = Date.now() + 5000
  while (submissions(calls).length === 0 && Date.now() < deadline) await Bun.sleep(5)
  expect(submissions(calls)[0]?.args.emergency).toBe(true)
})

test("a failing emergency review is reported and never stops the compaction", async () => {
  const { host } = fakeHost("not json")
  const { checkpoints, reports } = setup({ host })
  await expect(checkpoints.compacting("ses_1")).resolves.toBeUndefined()
  expect((reports[0] as MemCastleFailure).failureClass).toBe("invalid_input")
})

test("a compaction with no session id, which a host may send, does nothing", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints } = setup({ host })
  await checkpoints.compacting(undefined)
  expect(asked).toEqual([])
})

// --- the tool --------------------------------------------------------------------------------------------------

test("the tool submits a payload the model classified itself, validated and stamped with this agent", async () => {
  const { checkpoints, calls } = setup()
  const answer = await checkpoints.checkpoint("ses_1", {
    payload: { items: [{ destination: "preference", content: "Prefers tabs.", source: { agent: "forged" }, fact: { op: "x" } }] },
  })
  expect(answer).toBe("Checkpointed 1 item(s).")
  expect(submissions(calls)[0]?.args.payload).toEqual({
    items: [
      { destination: "preference", wing: null, name: null, content: "Prefers tabs.", tags: [], source: { kind: "manual", uri: null, agent: "opencode" }, fact: null },
    ],
  })
})

test("the tool refuses a payload that is wrong, with what to fix, and submits nothing", async () => {
  const { checkpoints, calls } = setup()
  const failure = await checkpoints.checkpoint("ses_1", { payload: { items: [{ destination: "nope", content: "x" }] } }).catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("invalid_input")
  expect(calls).toEqual([])
})

test("the tool with no payload reviews the conversation itself, with the user's note", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints } = setup({ host })
  expect(await checkpoints.checkpoint("ses_1", { note: "remember the db" })).toBe("Checkpointed 1 item(s).")
  expect(asked[0]?.prompt).toContain("remember the db")
})

test("an emergency payload through the tool is queued at once rather than waited for", async () => {
  const { checkpoints, calls } = setup({ job: { status: "queued" } })
  const answer = await checkpoints.checkpoint("ses_1", { payload: { items: [{ destination: "general", content: "x" }] }, emergency: true })
  expect(answer).toContain("Queued")
  expect(calls.map((call) => call.tool)).toEqual([CHECKPOINT_TOOL])
  expect(calls[0]?.args.emergency).toBe(true)
})

test("the tool with no payload in a host that cannot review says so instead of pretending", async () => {
  const { checkpoints } = setup({ host: null })
  const failure = await checkpoints.checkpoint("ses_1", {}).catch((error: unknown) => error)
  expect((failure as MemCastleFailure).message).toContain("no way to review")
})

test("the tool in a read-only session is refused by the client before any model call", async () => {
  const { host, asked } = fakeHost()
  const { checkpoints } = setup({ host, mode: "read-only" })
  const failure = await checkpoints.checkpoint("ses_1", {}).catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("mode_rejected")
  expect(asked).toEqual([])
})

test("the tool in a read-only session never submits the model's own payload either, so no write is attempted", async () => {
  const { checkpoints, calls } = setup({ mode: "read-only" })
  const failure = await checkpoints
    .checkpoint("ses_1", { payload: { items: [{ destination: "general", content: "x" }] }, emergency: true })
    .catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("mode_rejected")
  expect((failure as MemCastleFailure).toUserMessage()).toContain("MEMCASTLE_MODE=full")
  expect(calls).toEqual([])
})

test("forgetting a session stops its review and ends its report", async () => {
  let seen: AbortSignal | undefined
  const { host, asked } = fakeHost((signal) => ((seen = signal), new Promise<string>(() => undefined)))
  const { checkpoints, reports } = setup({ host })
  await checkpoints.sessionIdle("ses_1")
  while (asked.length === 0) await Bun.sleep(2)
  checkpoints.forget("ses_1")
  expect(seen?.aborted).toBe(true)
  expect(reports).toEqual([])
})

test("the tool's arguments are read defensively, since a host hands them over untyped", () => {
  expect(checkpointArgs(undefined)).toEqual({ payload: undefined, emergency: false, note: undefined })
  expect(checkpointArgs({ payload: { items: [] }, emergency: true, note: "n" })).toEqual({ payload: { items: [] }, emergency: true, note: "n" })
  expect(checkpointArgs({ emergency: "yes", note: 3 })).toEqual({ payload: undefined, emergency: false, note: undefined })
})

test("the slash command is added once, and a command the user already defined with that name is left alone", () => {
  const config: { command?: Record<string, unknown> } = {}
  addCheckpointCommand(config)
  const added = config.command?.[CHECKPOINT_COMMAND]
  expect(added).toMatchObject({ template: expect.stringContaining(CHECKPOINT_TOOL) })
  addCheckpointCommand(config)
  expect(config.command?.[CHECKPOINT_COMMAND]).toBe(added)

  const own = { template: "mine" }
  const custom = { command: { [CHECKPOINT_COMMAND]: own } }
  addCheckpointCommand(custom)
  expect(custom.command[CHECKPOINT_COMMAND]).toBe(own)
})

// --- OpenCode 1's host -----------------------------------------------------------------------------------------

/** The slice of OpenCode 1's client the host touches, answering as the test says. */
function fakeClient(options: { reply?: string; failCreate?: boolean } = {}) {
  const events: string[] = []
  const prompts: { body: Record<string, unknown>; path: { id: string } }[] = []
  const client = {
    app: { log: async () => ({}) },
    session: {
      messages: async () => ({
        data: [
          { info: { role: "user", model: { providerID: "anthropic", modelID: "sonnet" } }, parts: [{ type: "text", text: "which database?" }] },
          { info: { role: "assistant" }, parts: [{ type: "text", text: "SurrealDB." }, { type: "tool", tool: "bash" }] },
          { info: { role: "user", model: { providerID: "openai", modelID: "gpt" } }, parts: [{ type: "text", text: "injected", synthetic: true }, { type: "text", text: "thanks", ignored: true }] },
          { info: { role: "assistant" }, parts: [{ type: "text", text: "You're welcome." }] },
        ],
      }),
      create: async ({ body }: { body: Record<string, unknown> }) => {
        events.push(`create ${String(body.parentID)}`)
        return options.failCreate ? { error: { message: "no" } } : { data: { id: "ses_reviewer" } }
      },
      prompt: async (input: { body: Record<string, unknown>; path: { id: string } }) => {
        events.push(`prompt ${input.path.id}`)
        prompts.push(input)
        return { data: { parts: [{ type: "text", text: options.reply ?? REPLY }, { type: "reasoning", text: "ignored" }] } }
      },
      delete: async ({ path }: { path: { id: string } }) => (events.push(`delete ${path.id}`), {}),
    },
  }
  return { client: client as unknown as PluginInput["client"], events, prompts }
}

test("OpenCode 1's host reads only what was said: no tool parts, no synthetic or ignored text", async () => {
  const { client } = fakeClient()
  expect(await v1Host(client).transcript("ses_1")).toEqual([
    { role: "user", text: "which database?" },
    { role: "assistant", text: "SurrealDB." },
    { role: "assistant", text: "You're welcome." },
  ])
})

test("OpenCode 1's host asks in a child session it claims first, with the checkpoint tool off, and removes it after", async () => {
  const { client, events, prompts } = fakeClient()
  const claimed: string[] = []
  const reply = await v1Host(client).classify(
    "ses_1",
    { system: "SYSTEM", prompt: "PROMPT" },
    null,
    new AbortController().signal,
    (id) => (events.push(`claim ${id}`), claimed.push(id)),
  )

  expect(reply).toBe(REPLY)
  expect(events).toEqual(["create ses_1", "claim ses_reviewer", "prompt ses_reviewer", "delete ses_reviewer"])
  expect(prompts[0]?.body).toMatchObject({
    system: "SYSTEM",
    tools: { [CHECKPOINT_TOOL]: false },
    parts: [{ type: "text", text: "PROMPT" }],
    // The session's own model: the last user message's.
    model: { providerID: "openai", modelID: "gpt" },
  })
})

test("OpenCode 1's host uses the configured model when there is one, and still removes the child when asking fails", async () => {
  const { client, events, prompts } = fakeClient()
  const host = v1Host(client)
  await host.classify("ses_1", { system: "s", prompt: "p" }, { provider: "anthropic", id: "haiku" }, new AbortController().signal, () => undefined)
  expect(prompts[0]?.body.model).toEqual({ providerID: "anthropic", modelID: "haiku" })

  ;(client.session as unknown as { prompt: () => Promise<unknown> }).prompt = async () => ({ error: { message: "quota" } })
  const failure = await host.classify("ses_1", { system: "s", prompt: "p" }, null, new AbortController().signal, () => undefined).catch((e: unknown) => e)
  expect((failure as MemCastleFailure).message).toContain("quota")
  expect(events.at(-1)).toBe("delete ses_reviewer")
})

test("OpenCode 1's host reports a child session it could not open instead of reviewing nothing", async () => {
  const { client } = fakeClient({ failCreate: true })
  const failure = await v1Host(client).classify("ses_1", { system: "s", prompt: "p" }, null, new AbortController().signal, () => undefined).catch((e: unknown) => e)
  expect((failure as MemCastleFailure).failureClass).toBe("unexpected")
})

// --- OpenCode 2's turns ----------------------------------------------------------------------------------------

test("OpenCode 2's messages become turns: a user message's text and an assistant message's text parts only", () => {
  expect(
    v2Turns([
      { type: "user", text: "which database?" },
      { type: "assistant", content: [{ type: "reasoning", text: "hmm" }, { type: "text", text: "SurrealDB." }, { type: "tool" }] },
      { type: "skill" },
      { type: "assistant", content: [{ type: "tool" }] },
      null,
    ]),
  ).toEqual(TURNS)
  // The compaction hook hands over provider-level messages, which say `role` and hold a list of parts.
  expect(
    v2Turns([
      { role: "system", content: [{ type: "text", text: "rules" }] },
      { role: "user", content: [{ type: "text", text: "which database?" }] },
      { role: "tool", content: [{ type: "text", text: "output" }] },
      { role: "assistant", content: [{ type: "text", text: "SurrealDB." }] },
    ]),
  ).toEqual(TURNS)
  expect(v2Turns([])).toEqual([])
})

// --- the whole plugin against a real daemon --------------------------------------------------------------------

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

const saved = { ...process.env }
afterEach(() => {
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
})

/** The plugin as OpenCode 1 loads it, configured for the real daemon. */
async function realPlugin(options: Record<string, unknown>, reply: string) {
  Object.assign(process.env, {
    MEMCASTLE_PALACE_PATH: daemon.palacePath,
    HOME: daemon.clientEnv.HOME,
    XDG_STATE_HOME: daemon.clientEnv.XDG_STATE_HOME,
    MEMCASTLE_WAKE_UP: "false",
  })
  const { client } = fakeClient({ reply })
  const hooks = await plugin.server({ client, directory: "/work/real" } as unknown as PluginInput, options)
  return hooks
}

const idle = (sessionID: string) => ({ event: { type: "session.idle", properties: { sessionID } } as never })

test("a real session is checkpointed on the interval, and what was kept can be recalled", async () => {
  const reply = JSON.stringify({ items: [{ destination: "project", content: "ovenpelletstoken the oven burns pellets.", tags: [] }] })
  const hooks = await realPlugin({ checkpoint: { interval: 1, mode: "blocking" } }, reply)
  try {
    await hooks.event?.(idle("ses_real"))
    const reader = daemon.session("full")
    expect(JSON.stringify(await reader.call("memcastle_recall", { query: "ovenpelletstoken" }))).toContain("ovenpelletstoken")
    await reader.close()
  } finally {
    await hooks.dispose?.()
  }
})

test("a real compaction queues a Critical emergency checkpoint, and the daemon reports its priority", async () => {
  const reply = JSON.stringify({ items: [{ destination: "general", content: "emergencytoken before compaction.", tags: [] }] })
  const hooks = await realPlugin({ checkpoint: { enabled: false } }, reply)
  try {
    await hooks["experimental.session.compacting"]?.({ sessionID: "ses_real" }, { context: [] })
    const reader = daemon.session("full")
    const jobs = await reader.call<{ id: string; priority: number; kind: { payload?: unknown } }[]>("memcastle_job_list", {})
    const mine = jobs.filter((job) => JSON.stringify(job.kind).includes("emergencytoken"))
    expect(mine.map((job) => job.priority)).toEqual([100])
    await reader.close()
  } finally {
    await hooks.dispose?.()
  }
})

test("the plugin registers the checkpoint tool and command, and the tool saves a model's own payload", async () => {
  const hooks = await realPlugin({}, REPLY)
  try {
    const config: { command?: Record<string, unknown> } = {}
    await hooks.config?.(config as never)
    expect(config.command?.[CHECKPOINT_COMMAND]).toBeDefined()

    const tool = hooks.tool?.[CHECKPOINT_TOOL]
    expect(tool?.description).toContain("MemCastle")
    const answer = await tool?.execute(
      { payload: { items: [{ destination: "general", content: "tooltokenpayload stored by the model.", tags: [] }] } } as never,
      { sessionID: "ses_tool" } as never,
    )
    expect(answer).toBe("Checkpointed 1 item(s).")
  } finally {
    await hooks.dispose?.()
  }
})

test("the tool turns a failure into an error that carries the help, so the model can act on it", async () => {
  const hooks = await realPlugin({ mode: "read-only" }, REPLY)
  try {
    const failure = await Promise.resolve(
      hooks.tool?.[CHECKPOINT_TOOL]?.execute({} as never, { sessionID: "ses_ro" } as never),
    ).catch((error: unknown) => error)
    expect(failure).toBeInstanceOf(Error)
    expect((failure as Error).message).toContain("MEMCASTLE_MODE=full")
  } finally {
    await hooks.dispose?.()
  }
})
