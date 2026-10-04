import { expect, test } from "bun:test"
import { createCore, type Level } from "../src/core.ts"

function recorder() {
  const logs: { level: Level; message: string; extra?: unknown }[] = []
  return { logs, log: async (level: Level, message: string, extra?: Record<string, unknown>) => void logs.push({ level, message, extra }) }
}

test("an unknown mode fails closed and says why, so a typo never becomes full access", async () => {
  const { log, logs } = recorder()
  expect(await createCore({ mode: "readonly" }, log, {})).toBeUndefined()
  expect(logs[0]?.level).toBe("error")
  expect(logs[0]?.message).toContain("read-only")
})

test("an off session produces no handlers, as if MemCastle were not installed", async () => {
  const { log } = recorder()
  expect(await createCore({ mode: "off" }, log, {})).toBeUndefined()
})

test("a deleted OpenCode session forgets its connection, and an unknown one is not an error", async () => {
  const { log } = recorder()
  const core = await createCore({}, log, {})
  expect(core).toBeDefined()
  core!.sessions.session("ses_1")
  expect(core!.sessions.size).toBe(1)
  await core!.sessionDeleted("ses_1")
  await core!.sessionDeleted("ses_unknown")
  expect(core!.sessions.size).toBe(0)
})

test("dispose closes every connection", async () => {
  const { log } = recorder()
  const core = await createCore({}, log, {})
  core!.sessions.session("a")
  core!.sessions.session("b")
  await core!.dispose()
  expect(core!.sessions.size).toBe(0)
})

test("a failing host logger never turns a handler failure into a thrown error", async () => {
  let calls = 0
  const log = async (level: Level) => {
    // The startup line must succeed; every later report fails.
    if (++calls > 1 || level === "error") throw new Error("log is down")
  }
  const core = await createCore({}, log, {})
  core!.sessions.close = async () => {
    throw new Error("close failed")
  }
  await expect(core!.sessionDeleted("x")).resolves.toBeUndefined()
})

test("a handler failure is reported as a warning rather than thrown", async () => {
  const { log, logs } = recorder()
  const core = await createCore({}, log, {})
  core!.sessions.close = async () => {
    throw new Error("close failed")
  }
  await core!.sessionDeleted("x")
  expect(logs.at(-1)?.level).toBe("warn")
  expect(logs.at(-1)?.message).toContain("close failed")
})

test("the startup log never contains the token", async () => {
  const { log, logs } = recorder()
  await createCore({ token: "mc_super-secret-token-value" }, log, {})
  expect(JSON.stringify(logs)).not.toContain("super-secret")
})
