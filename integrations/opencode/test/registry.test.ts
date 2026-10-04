import { afterAll, beforeAll, expect, test } from "bun:test"
import { SessionRegistry } from "../src/registry.ts"
import { TestDaemon } from "./support/daemon.ts"

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

function registry(overrides: Record<string, unknown> = {}) {
  return new SessionRegistry(daemon.settings(overrides), daemon.clientEnv)
}

test("the same OpenCode session always gets the same connection", () => {
  const sessions = registry()
  expect(sessions.session("ses_1")).toBe(sessions.session("ses_1"))
  expect(sessions.size).toBe(1)
})

test("a session costs no connection until it first touches MemCastle", async () => {
  const sessions = registry()
  const session = sessions.session("ses_lazy")
  expect(session.connected).toBe(false)
  expect(session.connects).toBe(0)
  await session.call("memcastle_status")
  expect(session.connects).toBe(1)
  await sessions.closeAll()
})

test("two OpenCode sessions in one process get two daemon sessions that reconnect independently", async () => {
  const sessions = registry({ mode: "read-only" })
  const one = sessions.session("ses_a")
  const two = sessions.session("ses_b")
  await one.connect()
  await two.connect()
  expect(one.sessionId).not.toBe(two.sessionId)

  await daemon.forget(one)

  expect(await one.reportedMode()).toBe("read-only")
  expect(await two.reportedMode()).toBe("read-only")
  expect(one.connects).toBe(2)
  expect(two.connects).toBe(1)
  await sessions.closeAll()
})

test("closing a session ends its connection and forgets it, and closing an unknown one is harmless", async () => {
  const sessions = registry()
  const session = sessions.session("ses_close")
  await session.connect()

  await sessions.close("ses_close")
  await sessions.close("ses_never_seen")

  expect(session.connected).toBe(false)
  expect(sessions.size).toBe(0)
})

test("closing every session closes every connection", async () => {
  const sessions = registry()
  const all = ["ses_x", "ses_y", "ses_z"].map((id) => sessions.session(id))
  await Promise.all(all.map((session) => session.connect()))

  await sessions.closeAll()

  expect(all.every((session) => !session.connected)).toBe(true)
  expect(sessions.size).toBe(0)
})
