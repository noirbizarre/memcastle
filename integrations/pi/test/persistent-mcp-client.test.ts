import { afterAll, beforeAll, expect, test } from "bun:test"
import { mkdtemp } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { MemCastleFailure, failureFromJob } from "../src/failures.ts"
import { type ModeLabel, toWireMode } from "../src/modes.ts"
import { TestDaemon, waitForJob } from "./support/daemon.ts"
import { fixture, substitute } from "./support/fixtures.ts"

interface Operation {
  id: string
  class: "read" | "write" | "ungated"
  tool: string
  arguments: Record<string, unknown>
}
const modes = fixture<{
  labels: Record<ModeLabel, string>
  rejected_wire_values: string[]
  rules: Record<string, Record<Operation["class"], "ok" | "mode_forbidden">>
  operations: Operation[]
}>("modes.json")

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

/** Run `body` and return the failure it threw, or fail the test if it did not throw one. */
async function failureOf(body: () => Promise<unknown>): Promise<MemCastleFailure> {
  const error = await body().then(
    () => null,
    (error: unknown) => error,
  )
  expect(error).toBeInstanceOf(MemCastleFailure)
  return error as MemCastleFailure
}

test("a new session selects its mode before anything else and the daemon reads it back", async () => {
  for (const label of Object.keys(modes.labels) as ModeLabel[]) {
    const session = daemon.session(label)
    expect(await session.reportedMode()).toBe(label)
    await session.close()
  }
})

test("three sessions in three modes share one daemon without changing each other's mode", async () => {
  const sessions = (Object.keys(modes.labels) as ModeLabel[]).map((label) => [label, daemon.session(label)] as const)
  for (const [, session] of sessions) await session.connect()
  for (const [label, session] of sessions) expect(await session.reportedMode()).toBe(label)
  await Promise.all(sessions.map(([, session]) => session.close()))
})

test("one session serves wake-up, recall and a job listing without reconnecting", async () => {
  const session = daemon.session("read-only")
  await session.connect()
  const id = session.sessionId
  expect(id).toBeTruthy()

  await session.call("memcastle_wake_up", { agent_identity: "test-agent", wing: "conformance" })
  await session.call("memcastle_recall", { query: "anything" })
  await session.call("memcastle_job_list")

  expect(session.sessionId).toBe(id)
  expect(session.connects).toBe(1)
  expect(await session.reportedMode()).toBe("read-only")
  await session.close()
})

test("a reconnect re-selects the mode instead of silently falling back to full", async () => {
  const session = daemon.session("read-only")
  await session.connect()
  const first = session.sessionId

  await session.reconnect()

  expect(session.sessionId).not.toBe(first)
  expect(session.connects).toBe(2)
  expect(await session.reportedMode()).toBe("read-only")
  await session.close()
})

test("concurrent first calls share one connection rather than racing to open several", async () => {
  const session = daemon.session("full")
  await Promise.all([session.call("memcastle_status"), session.call("memcastle_status"), session.call("memcastle_job_list")])
  expect(session.connects).toBe(1)
  await session.close()
})

test("a wire value the daemon rejects is refused with the invalid-input class and leaves the mode unchanged", async () => {
  const session = daemon.session("read-only")
  for (const rejected of modes.rejected_wire_values) {
    const failure = await failureOf(() => session.call("memcastle_set_mode", { mode: rejected }))
    expect(failure.failureClass).toBe("invalid_input")
    expect(failure.code).toBe("memcastle::input::invalid")
  }
  expect(await session.reportedMode()).toBe("read-only")
  await session.close()
})

test("every operation in the shared fixture behaves as the mode rules say, through this client", async () => {
  const mineDir = await mkdtemp(join(tmpdir(), "memcastle-mine-"))
  for (const [label, wire] of Object.entries(modes.labels) as [ModeLabel, string][]) {
    expect(toWireMode(label)).toBe(wire as never)
    const session = daemon.session(label)
    for (const operation of modes.operations) {
      const args = substitute(operation.arguments, "mine_dir", mineDir)
      const expected = modes.rules[wire]?.[operation.class]
      const label_ = `${operation.id} in ${label}`
      if (expected === "ok") {
        await session.call(operation.tool, args) // throws on any failure
      } else {
        const failure = await failureOf(() => session.call(operation.tool, args))
        expect(failure.failureClass, label_).toBe("mode_rejected")
        expect(failure.code, label_).toBe("memcastle::app::mode_forbidden")
        expect(failure.help, label_).toBeTruthy()
      }
    }
    await session.close()
  }
})

test("a malformed checkpoint is an invalid_input failure that carries the daemon's help", async () => {
  const session = daemon.session("full")
  const failure = await failureOf(() => session.call("memcastle_checkpoint", { payload: { items: [] } }))
  expect(failure.failureClass).toBe("invalid_input")
  expect(failure.toUserMessage().length).toBeGreaterThan(failure.message.length)
  await session.close()
})

test("a checkpoint job that fails is reported as a job_failed failure with the job's own error", async () => {
  const session = daemon.session("full")
  const item = (content: string) => ({
    destination: "general",
    name: "same-name",
    content,
    tags: [],
    source: { kind: "manual", uri: null, agent: "test-agent" },
    fact: null,
  })
  // Two different drawers cannot share one name in a room, so the second job fails.
  const first = await session.call<{ id: string }>("memcastle_checkpoint", { payload: { items: [item("one")] } })
  expect((await waitForJob(session, first.id)).status).toBe("completed")
  const second = await session.call<{ id: string }>("memcastle_checkpoint", { payload: { items: [item("two")] } })

  const failure = failureFromJob(await waitForJob(session, second.id))
  expect(failure?.failureClass).toBe("job_failed")
  expect(failure?.message).toContain("failed")
  await session.close()
})

test("after the daemon goes away calls fail as daemon_unavailable instead of returning nothing", async () => {
  const gone = await TestDaemon.start()
  const session = gone.session("full", { timeoutMs: 1000 })
  await session.connect()
  await gone.stop()
  const failure = await failureOf(() => session.call("memcastle_status"))
  expect(failure.failureClass).toBe("daemon_unavailable")
  expect(failure.toUserMessage()).toContain("memcastle daemon start")
  expect(session.connected).toBe(false)
})
