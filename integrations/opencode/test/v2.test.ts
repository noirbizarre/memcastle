import { afterEach, beforeEach, expect, spyOn, test } from "bun:test"
import type { Plugin } from "@opencode/plugin"
import plugin from "../src/index.ts"

interface RegisteredTool {
  name: string
  description: string
  input: unknown
  execute: (input: unknown, context: { sessionID: string }) => Promise<{ content: string }>
}
interface RegisteredCommand {
  name: string
  execute: (invocation: { sessionID: string; prompt: { text: string } }) => Promise<void>
}

/** A V2 context that records what the plugin registers and lets the test push events into its subscription. */
function fakeContext(options: Record<string, unknown> = {}, reply = '{"items":[]}') {
  const asked: unknown[] = []
  const hooks: { domain: string; name: string }[] = []
  const tools: RegisteredTool[] = []
  const commands: RegisteredCommand[] = []
  const synthetic: unknown[] = []
  // The callbacks too, so a test can fire a hook the way OpenCode does.
  const callbacks = new Map<string, (input: unknown) => Promise<void> | void>()
  const queue: unknown[] = []
  let wake: (() => void) | undefined
  let signal: AbortSignal | undefined
  const registration = async () => ({ dispose: async () => undefined })
  const ctx = {
    options,
    location: { directory: "/work/started-here" },
    event: {
      subscribe: async function* (opts?: { signal?: AbortSignal }) {
        signal = opts?.signal
        while (!signal?.aborted) {
          if (queue.length === 0) await new Promise<void>((resolve) => ((wake = resolve), signal?.addEventListener("abort", () => resolve())))
          while (queue.length > 0) yield queue.shift()
        }
      },
    },
    session: {
      hook: async (name: string, callback: (input: unknown) => Promise<void> | void) => (
        hooks.push({ domain: "session", name }), callbacks.set(name, callback), registration()
      ),
      // The conversation as OpenCode 2 reports it, for the interval review.
      context: async () => [
        { type: "user", text: "which database?" },
        { type: "assistant", content: [{ type: "text", text: "SurrealDB." }] },
      ],
      synthetic: async (input: unknown) => (synthetic.push(input), {}),
    },
    tool: {
      hook: async (name: string) => (hooks.push({ domain: "tool", name }), registration()),
      // The checkpoint tool is added through an editor, which this one records.
      transform: async (callback: (editor: unknown) => void) => (callback({ add: (tool: unknown) => tools.push(tool as RegisteredTool) }), registration()),
    },
    command: {
      transform: async (callback: (editor: unknown) => void) => (callback({ add: (command: unknown) => commands.push(command as RegisteredCommand) }), registration()),
    },
    // The shared skills are registered with OpenCode's own skill mechanism (recall.test.ts holds that to the files).
    skill: { transform: async () => registration() },
    generate: { text: async (input: unknown) => (asked.push(input), { text: reply }) },
  } as unknown as Plugin.Context
  return {
    ctx,
    hooks,
    tools,
    commands,
    synthetic,
    asked,
    /** Run the registered session hook `name`, as OpenCode does before a model request. */
    runHook: async (name: string, input: unknown) => void (await callbacks.get(name)?.(input)),
    push: (event: unknown) => (queue.push(event), wake?.()),
    aborted: () => signal?.aborted === true,
  }
}

const saved = { ...process.env }
beforeEach(() => {
  // The plugin reports through the console in V2; keep the test output readable.
  spyOn(console, "info").mockImplementation(() => undefined)
})
afterEach(() => {
  ;(console.info as unknown as { mockRestore?: () => void }).mockRestore?.()
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
})

test("setup registers the lifecycle hooks and returns a cleanup function", async () => {
  const fake = fakeContext()
  const cleanup = await plugin.setup(fake.ctx)
  expect(fake.tools.map((tool) => tool.name)).toEqual(["memcastle_checkpoint"])
  expect(fake.commands.map((command) => command.name)).toEqual(["memcastle-checkpoint"])
  expect(fake.hooks).toEqual([
    { domain: "session", name: "context" },
    { domain: "session", name: "compaction" },
  ])
  expect(typeof cleanup).toBe("function")
  await cleanup?.()
})

test("cleanup aborts the event subscription so a reloaded plugin leaves no subscriber behind", async () => {
  const fake = fakeContext()
  const cleanup = await plugin.setup(fake.ctx)
  await Bun.sleep(10)
  expect(fake.aborted()).toBe(false)
  await cleanup?.()
  expect(fake.aborted()).toBe(true)
})

test("an event the plugin does not care about, or a malformed one, is ignored", async () => {
  const fake = fakeContext()
  const cleanup = await plugin.setup(fake.ctx)
  fake.push({ type: "session.idle", data: { sessionID: "ses_1" } })
  fake.push({ type: "session.deleted" })
  fake.push({ type: "session.deleted", data: { sessionID: 42 } })
  await Bun.sleep(10)
  await cleanup?.()
})

test("an unknown mode fails closed: nothing is registered and nothing is subscribed", async () => {
  const error = spyOn(console, "error").mockImplementation(() => undefined)
  const fake = fakeContext({ mode: "readonly" })
  expect(await plugin.setup(fake.ctx)).toBeUndefined()
  expect(fake.hooks).toEqual([])
  expect(error.mock.calls.flat().join(" ")).toContain("read-only")
  error.mockRestore()
})

test("an off session registers nothing", async () => {
  const fake = fakeContext({ mode: "off" })
  expect(await plugin.setup(fake.ctx)).toBeUndefined()
  expect(fake.hooks).toEqual([])
})

test("the startup log never contains the token", async () => {
  const info = console.info as unknown as { mock: { calls: unknown[][] } }
  const fake = fakeContext({ token: "mc_super-secret-token-value" })
  await (await plugin.setup(fake.ctx))?.()
  expect(JSON.stringify(info.mock.calls)).toContain("MemCastle plugin ready.")
  expect(JSON.stringify(info.mock.calls)).not.toContain("super-secret")
})

// --- checkpoints -----------------------------------------------------------------------------------------------

const until = async (condition: () => boolean, what: string) => {
  const deadline = Date.now() + 5000
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await Bun.sleep(5)
  }
}

test("a session.idle event counts towards the interval, and the review reads the conversation OpenCode 2 reports", async () => {
  process.env.MEMCASTLE_PORT = "1"
  process.env.MEMCASTLE_PALACE_PATH = "/nonexistent/memcastle-palace"
  const fake = fakeContext({ checkpoint: { interval: 1, mode: "blocking", model: "anthropic/haiku" } })
  const cleanup = await plugin.setup(fake.ctx)
  fake.push({ type: "session.idle", data: { sessionID: "ses_1" } })
  await until(() => fake.asked.length === 1, "the review's model call")

  const request = fake.asked[0] as { prompt: string; model: unknown }
  expect(request.prompt).toContain("User: which database?")
  expect(request.prompt).toContain("Assistant: SurrealDB.")
  // The shared skill is the reviewer's instruction, in front of the conversation.
  expect(request.prompt).toContain("Checkpoint durable context")
  expect(request.model).toEqual({ id: "haiku", providerID: "anthropic" })
  await cleanup?.()
})

test("the compaction hook hands the messages it already holds to the emergency review", async () => {
  process.env.MEMCASTLE_PORT = "1"
  process.env.MEMCASTLE_PALACE_PATH = "/nonexistent/memcastle-palace"
  const fake = fakeContext({})
  const cleanup = await plugin.setup(fake.ctx)
  await fake.runHook("compaction", {
    sessionID: "ses_1",
    messages: [
      { role: "user", content: [{ type: "text", text: "a decision made just now" }] },
      { role: "assistant", content: [{ type: "text", text: "noted" }] },
    ],
  })
  expect(fake.asked).toHaveLength(1)
  expect((fake.asked[0] as { prompt: string }).prompt).toContain("a decision made just now")
  await cleanup?.()
})

test("the slash command shows its answer in the session, including a refusal with what to do", async () => {
  process.env.MEMCASTLE_PORT = "1"
  process.env.MEMCASTLE_PALACE_PATH = "/nonexistent/memcastle-palace"
  const fake = fakeContext({})
  const cleanup = await plugin.setup(fake.ctx)
  await fake.commands[0]?.execute({ sessionID: "ses_1", prompt: { text: "remember the db" } })

  expect(fake.asked).toHaveLength(1)
  expect((fake.asked[0] as { prompt: string }).prompt).toContain("remember the db")
  // The model found nothing to keep, which is a result and is shown as one.
  expect(JSON.stringify(fake.synthetic[0])).toContain("nothing worth keeping")
  await cleanup?.()
})

test("the tool turns a failure into an error carrying the help, and a read-only session is refused", async () => {
  const fake = fakeContext({ mode: "read-only" })
  const cleanup = await plugin.setup(fake.ctx)
  const failure = await fake.tools[0]?.execute({}, { sessionID: "ses_1" }).catch((error: unknown) => error)
  expect((failure as Error).message).toContain("MEMCASTLE_MODE=full")
  expect(fake.asked).toEqual([])
  await cleanup?.()
})

test("a host without the tool editor loses only the checkpoint tool, and the plugin says so", async () => {
  const warn = spyOn(console, "warn").mockImplementation(() => undefined)
  const fake = fakeContext({})
  ;(fake.ctx.tool as unknown as { transform: unknown }).transform = async () => {
    throw new Error("no tools here")
  }
  const cleanup = await plugin.setup(fake.ctx)
  expect(warn.mock.calls.flat().join(" ")).toContain("checkpoint tool and command could not be registered")
  expect(fake.hooks.map((hook) => hook.name)).toContain("compaction")
  await cleanup?.()
  warn.mockRestore()
})
