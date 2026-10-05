// Six sessions, two integrations, three modes, one daemon, all at once.
//
// The daemon proves once that it keeps each MCP session's mode apart (`tests/in_process/integration_contract.rs`). What only a
// client test can prove is the other half: that each integration, loaded in the same process as another one in a
// different mode, does what its own mode dictates and nothing else. A `full` session's checkpoint must land while an
// `off` session beside it does nothing, and a `read-only` session must read without ever attempting a write.
//
// The wire is the witness: `network.ts` records which actor made which request, so "an off session did nothing" and
// "a read-only session never called a write tool" are statements about requests, not about what a handler returned.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test"
import { submitCheckpoint } from "../../pi/src/checkpoint-core.ts"
import { TestDaemon } from "./support/daemon.ts"
import { configure, restoreEnv } from "./support/env.ts"
import { fixture } from "./support/fixtures.ts"
import { as, requestsBy, startRecording, stopRecording, toolsCalledBy, unattributed } from "./support/network.ts"
import { openCodePlugin } from "./support/opencode.ts"
import { piSession } from "./support/pi.ts"

const isolation = fixture<{ marker: { token: string; payload: object } }>("off-isolation.json")
const modes = fixture<{ operations: { tool: string; class: string }[] }>("modes.json")
const TOKEN = isolation.marker.token

/** Every tool the daemon refuses to a read-only session, so a read-only actor must never call one. */
const WRITE_TOOLS = new Set(modes.operations.filter((operation) => operation.class === "write").map((operation) => operation.tool))

const OPENCODE_TOKEN = "mixedopencodefulltoken"
const OPENCODE_READ_ONLY_TOKEN = "mixedopencodereadonlytoken"
const PI_REVIEW_TEXT = "pi review output"

const payload = (token: string) => ({ items: [{ destination: "project", content: `${token} written by a session`, tags: [] }] })

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
  const seeder = daemon.session("full")
  await submitCheckpoint(seeder, isolation.marker.payload as never)
  await seeder.close()
  startRecording()
})
afterAll(async () => {
  stopRecording()
  await daemon.stop()
})
afterEach(restoreEnv)

/** A Pi session in `mode`, started right after its own settings were applied. */
async function startPi(actor: string, mode: "full" | "read-only" | "off") {
  configure(daemon, { mode, eagerCheckpoint: true, recall: true })
  const session = piSession()
  await as(actor, () => session.fire("session_start", { reason: "startup" }))
  return session
}

/** An OpenCode process in `mode`, loaded right after its own settings were applied. */
function startOpenCode(actor: string, mode: "full" | "read-only" | "off") {
  configure(daemon, { mode, recall: true })
  return as(actor, () => openCodePlugin({}))
}

test("three modes across two integrations share one daemon, and each behaves exactly as its own mode dictates", async () => {
  // Each actor is loaded while its own environment is applied, one after the other, then they all run together.
  const piFull = await startPi("pi-full", "full")
  const piReadOnly = await startPi("pi-read-only", "read-only")
  const piOff = await startPi("pi-off", "off")
  const ocFull = await startOpenCode("oc-full", "full")
  const ocReadOnly = await startOpenCode("oc-read-only", "read-only")
  const ocOff = await startOpenCode("oc-off", "off")

  const results = await Promise.all([
    // Pi, full: briefed, reminded, and its interval review is saved.
    as("pi-full", async () => {
      const first = await piFull.fire("before_agent_start", { prompt: "hello" })
      await piFull.fire("agent_end")
      return { first, asked: piFull.asked }
    }),
    // Pi, read-only: briefed and reminded, but nothing is reviewed and the manual command refuses before any model call.
    as("pi-read-only", async () => {
      const first = await piReadOnly.fire("before_agent_start", { prompt: "hello" })
      await piReadOnly.fire("agent_end")
      await piReadOnly.command("memcastle-checkpoint")
      await piReadOnly.fire("session_before_compact", { reason: "threshold" })
      return { first, asked: piReadOnly.asked, notes: piReadOnly.notes }
    }),
    // Pi, off: as if MemCastle were not installed.
    as("pi-off", async () => {
      const first = await piOff.fire("before_agent_start", { prompt: "hello" })
      await piOff.fire("agent_end")
      await piOff.command("memcastle-checkpoint")
      await piOff.command("memcastle-wake-up")
      await piOff.fire("session_before_compact", { reason: "threshold" })
      return { first, asked: piOff.asked, notes: piOff.notes }
    }),
    // OpenCode, full: briefed, reminded, and the model's own checkpoint is saved.
    as("oc-full", async () => {
      await ocFull.created("ses_full")
      const system = await ocFull.systemPrompt("ses_full")
      const saved = await ocFull.callTool("memcastle_checkpoint", { payload: payload(OPENCODE_TOKEN) }, "ses_full")
      return { system, saved }
    }),
    // OpenCode, read-only: briefed and reminded, and a checkpoint is refused with the way out.
    as("oc-read-only", async () => {
      await ocReadOnly.created("ses_ro")
      const system = await ocReadOnly.systemPrompt("ses_ro")
      const refusal = await ocReadOnly
        .callTool("memcastle_checkpoint", { payload: payload(OPENCODE_READ_ONLY_TOKEN) }, "ses_ro")
        .catch((error: unknown) => error)
      await ocReadOnly.compact("ses_ro")
      return { system, refusal }
    }),
    // OpenCode, off: no hooks, no tools, nothing to call.
    as("oc-off", async () => {
      await ocOff.created("ses_off")
      const system = await ocOff.systemPrompt("ses_off")
      const saved = await ocOff.callTool("memcastle_checkpoint", { payload: payload("mixedopencodeofftoken") }, "ses_off")
      await ocOff.idle("ses_off")
      await ocOff.compact("ses_off")
      return { system, saved }
    }),
  ])
  const [piFullResult, piReadOnlyResult, piOffResult, ocFullResult, ocReadOnlyResult, ocOffResult] = results

  // --- what each session was shown ------------------------------------------------------------------------------
  for (const [name, first] of [
    ["pi-full", piFullResult.first],
    ["pi-read-only", piReadOnlyResult.first],
  ] as const) {
    expect(first?.message?.content, `${name} wake-up`).toContain(TOKEN)
    expect(first?.systemPrompt, `${name} reminder`).toContain("memcastle_search")
  }
  for (const [name, system] of [
    ["oc-full", ocFullResult.system],
    ["oc-read-only", ocReadOnlyResult.system],
  ] as const) {
    expect(system.join("\n"), `${name} wake-up`).toContain(TOKEN)
    expect(system.join("\n"), `${name} reminder`).toContain("memcastle_search")
  }
  // An off session is shown exactly what the host built, and told nothing but that MemCastle is not active.
  expect(piOffResult.first).toBeUndefined()
  expect(piOffResult.notes.map((note) => note.message)).toEqual([
    "MemCastle is not active in this session.",
    "MemCastle is not active in this session.",
  ])
  expect(ocOffResult.system).toEqual(["BASE SYSTEM PROMPT"])
  expect(ocOffResult.saved).toBeNull()

  // --- what each session wrote ----------------------------------------------------------------------------------
  expect(piFullResult.asked).toHaveLength(1)
  expect(piReadOnlyResult.asked, "a read-only Pi session never pays for a review").toEqual([])
  expect(piReadOnlyResult.notes.at(-1)).toMatchObject({ level: "info" })
  expect(piOffResult.asked).toEqual([])
  expect(ocFullResult.saved).toBe("Checkpointed 1 item(s).")
  expect((ocReadOnlyResult.refusal as Error).message).toContain("MEMCASTLE_MODE=full")

  // --- what the daemon holds: the full sessions' checkpoints landed, nobody else's did -------------------------------
  // The test's own reader is an actor too, so that nothing on the wire is left unattributed.
  await as("reader", async () => {
    const reader = daemon.session("full")
    try {
      const recalled = async (query: string) => JSON.stringify(await reader.call("memcastle_recall", { query }))
      expect(await recalled(OPENCODE_TOKEN), "the full OpenCode session's checkpoint").toContain(OPENCODE_TOKEN)
      expect(await recalled("review"), "the full Pi session's review").toContain(PI_REVIEW_TEXT)
      expect(await recalled(OPENCODE_READ_ONLY_TOKEN), "a read-only session's refused checkpoint").not.toContain(OPENCODE_READ_ONLY_TOKEN)
      expect(await recalled("mixedopencodeofftoken"), "an off session's checkpoint").not.toContain("mixedopencodeofftoken")
    } finally {
      await reader.close()
    }
  })

  // --- what each session asked of the daemon ----------------------------------------------------------------------
  for (const actor of ["pi-off", "oc-off"]) expect(requestsBy(actor), `${actor} must not touch the daemon`).toEqual([])
  for (const actor of ["pi-read-only", "oc-read-only"]) {
    const called = toolsCalledBy(actor)
    expect(called.length, `${actor} reads`).toBeGreaterThan(0)
    expect(called.filter((tool) => WRITE_TOOLS.has(tool)), `${actor} must never attempt a write`).toEqual([])
  }
  for (const actor of ["pi-full", "oc-full"]) expect(toolsCalledBy(actor), `${actor} wakes up`).toContain("memcastle_wake_up")
  expect(toolsCalledBy("pi-full")).toContain("memcastle_checkpoint")
  expect(toolsCalledBy("oc-full")).toContain("memcastle_checkpoint")
  expect(unattributed()).toEqual([])

  await Promise.all([
    as("pi-full", () => piFull.fire("session_shutdown", { reason: "quit" })),
    as("pi-read-only", () => piReadOnly.fire("session_shutdown", { reason: "quit" })),
    as("pi-off", () => piOff.fire("session_shutdown", { reason: "quit" })),
    as("oc-full", async () => ocFull.hooks.dispose?.()),
    as("oc-read-only", async () => ocReadOnly.hooks.dispose?.()),
    as("oc-off", async () => ocOff.hooks.dispose?.()),
  ])
  expect(requestsBy("pi-off").concat(requestsBy("oc-off"))).toEqual([])
})
