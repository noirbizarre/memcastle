import { afterEach, beforeEach, expect, spyOn, test } from "bun:test"
import type { Plugin } from "@opencode/plugin"
import plugin from "../src/index.ts"

/** A V2 context that records what the plugin registers and lets the test push events into its subscription. */
function fakeContext(options: Record<string, unknown> = {}) {
  const hooks: { domain: string; name: string }[] = []
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
    },
    tool: { hook: async (name: string) => (hooks.push({ domain: "tool", name }), registration()) },
    // The shared skills are registered with OpenCode's own skill mechanism (recall.test.ts holds that to the files).
    skill: { transform: async () => registration() },
  } as unknown as Plugin.Context
  return {
    ctx,
    hooks,
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
  expect(fake.hooks).toEqual([
    { domain: "session", name: "context" },
    { domain: "session", name: "compaction" },
    { domain: "tool", name: "execute.before" },
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
