import { afterEach, expect, test } from "bun:test"
import type { PluginInput } from "@opencode-ai/plugin"
import plugin from "../src/index.ts"

/** The only part of OpenCode's client the plugin touches in the foundation: structured logging. */
function fakeInput() {
  const logs: { level: string; message: string; extra?: unknown }[] = []
  const client = {
    app: {
      log: async ({ body }: { body: { level: string; message: string; extra?: unknown } }) => {
        logs.push(body)
        return {}
      },
    },
  }
  return { input: { client } as unknown as PluginInput, logs }
}

const saved = { ...process.env }
afterEach(() => {
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
})

test("the plugin module has an id, a V1 server function and a V2 setup function, and nothing else for OpenCode to mistake for a plugin", () => {
  expect(plugin.id).toBe("memcastle")
  expect(typeof plugin.server).toBe("function")
  expect(typeof plugin.setup).toBe("function")
  expect(Object.keys(plugin).sort()).toEqual(["id", "server", "setup"])
})

test("the plugin loads with no daemon running, because it connects lazily rather than at startup", async () => {
  process.env.MEMCASTLE_PORT = "1"
  const { input, logs } = fakeInput()
  const hooks = await plugin.server(input)
  expect(Object.keys(hooks)).toEqual(
    expect.arrayContaining(["config", "event", "experimental.chat.system.transform", "experimental.session.compacting", "dispose"]),
  )
  expect(logs.map((entry) => entry.level)).toEqual(["info"])
  await hooks.dispose?.()
})

test("a checkpoint hook that fails never throws into OpenCode, and the failure reaches the log", async () => {
  process.env.MEMCASTLE_PORT = "1"
  process.env.MEMCASTLE_PALACE_PATH = "/nonexistent/memcastle-palace"
  const { input, logs } = fakeInput()
  const hooks = await plugin.server(input)
  await hooks.event?.({ event: { type: "session.idle", properties: { sessionID: "ses_1" } } as never })
  // The fake client cannot read a conversation, so the review fails, and compaction must go ahead regardless.
  await hooks["experimental.session.compacting"]?.({ sessionID: "ses_1" }, { context: [] })
  expect(logs.some((entry) => entry.level === "warn" && entry.message.startsWith("emergency checkpoint"))).toBe(true)
  await hooks.dispose?.()
})

test("the plugin adds the checkpoint tool and the slash command, once", async () => {
  const { input } = fakeInput()
  const hooks = await plugin.server(input)
  expect(Object.keys(hooks.tool ?? {})).toEqual(["memcastle_checkpoint"])
  const config: { command?: Record<string, unknown> } = {}
  await hooks.config?.(config as never)
  expect(Object.keys(config.command ?? {})).toEqual(["memcastle-checkpoint", "memcastle-audit", "memcastle-repair"])
  await hooks.dispose?.()
})

test("an unknown mode fails closed: the plugin does nothing and says why", async () => {
  const { input, logs } = fakeInput()
  const hooks = await plugin.server(input, { mode: "readonly" })
  expect(Object.keys(hooks)).toEqual([])
  expect(logs[0]?.level).toBe("error")
  expect(logs[0]?.message).toContain("read-only")
})

test("an off session leaves MemCastle entirely out of OpenCode", async () => {
  const { input } = fakeInput()
  expect(Object.keys(await plugin.server(input, { mode: "off" }))).toEqual([])
})

test("the startup log line never contains the token", async () => {
  const { input, logs } = fakeInput()
  await (await plugin.server(input, { token: "mc_super-secret-token-value" })).dispose?.()
  expect(JSON.stringify(logs)).not.toContain("super-secret")
})
