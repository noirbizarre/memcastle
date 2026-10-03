import { afterAll, beforeAll, expect, test } from "bun:test"
import { MemCastleFailure } from "../src/failures.ts"
import { TestDaemon } from "./support/daemon.ts"

const TOKEN = `mc_${"a1".repeat(32)}`
const WRONG = `mc_${"b2".repeat(32)}`

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start({ token: TOKEN })
})
afterAll(async () => {
  await daemon.stop()
})

async function failureOf(body: () => Promise<unknown>): Promise<MemCastleFailure> {
  const error = await body().then(
    () => null,
    (error: unknown) => error,
  )
  expect(error).toBeInstanceOf(MemCastleFailure)
  return error as MemCastleFailure
}

test("a daemon that needs a token is still found, because its health check is the one open route", async () => {
  const session = daemon.session("full", { token: TOKEN })
  expect(await session.reportedMode()).toBe("full")
  await session.close()
})

test("a missing token is an unauthorized failure that says to set MEMCASTLE_AUTH_TOKEN", async () => {
  const session = daemon.session("full", { token: "" })
  const failure = await failureOf(() => session.connect())
  expect(failure.failureClass).toBe("unauthorized")
  expect(failure.toUserMessage()).toContain("MEMCASTLE_AUTH_TOKEN")
})

test("a wrong token is refused as unauthorized and its value never appears in the message", async () => {
  const session = daemon.session("full", { token: WRONG })
  const failure = await failureOf(() => session.connect())
  expect(failure.failureClass).toBe("unauthorized")
  expect(failure.code).toBe("memcastle::auth::unauthorized")
  expect(failure.toUserMessage()).not.toContain(WRONG)
  expect(String(failure.stack)).not.toContain(WRONG)
})
