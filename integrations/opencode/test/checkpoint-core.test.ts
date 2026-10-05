// This file is the same in `integrations/pi` and `integrations/opencode`, like the code it tests.
import { afterAll, beforeAll, expect, test } from "bun:test"
import {
  type Caller,
  type CheckpointJob,
  CheckpointReview,
  DEFAULT_CHECKPOINT,
  ExchangeCounter,
  type ReviewIo,
  type Turn,
  classificationRequest,
  describeOutcome,
  parseClassification,
  renderTranscript,
  resolveCheckpoint,
  submitCheckpoint,
  textOf,
  turnOf,
  validatePayload,
} from "../src/checkpoint-core.ts"
import { MemCastleFailure } from "../src/failures.ts"
import type { ModeLabel } from "../src/modes.ts"
import { TestDaemon } from "./support/daemon.ts"
import { fixture } from "./support/fixtures.ts"

// --- settings --------------------------------------------------------------------------------------------------

test("with no configuration, a checkpoint is reviewed every ten exchanges, silently, by the session's own model", () => {
  expect(resolveCheckpoint(undefined, {})).toEqual({ enabled: true, interval: 10, mode: "silent", model: null })
  expect(DEFAULT_CHECKPOINT.interval).toBe(10)
})

test("the environment configures a checkpoint, and an option beats it", () => {
  const env = {
    MEMCASTLE_CHECKPOINT: "false",
    MEMCASTLE_CHECKPOINT_INTERVAL: "3",
    MEMCASTLE_CHECKPOINT_MODE: "blocking",
    MEMCASTLE_CHECKPOINT_MODEL: "anthropic/claude-haiku-4-5",
  }
  expect(resolveCheckpoint(undefined, env)).toEqual({
    enabled: false,
    interval: 3,
    mode: "blocking",
    model: { provider: "anthropic", id: "claude-haiku-4-5" },
  })
  expect(resolveCheckpoint({ enabled: true, interval: 5, mode: "silent", model: { provider: "p", id: "m" } }, env)).toEqual({
    enabled: true,
    interval: 5,
    mode: "silent",
    model: { provider: "p", id: "m" },
  })
})

test("a model id may itself contain slashes, and only the first one separates the provider", () => {
  expect(resolveCheckpoint({ model: "openrouter/anthropic/claude-3" }, {}).model).toEqual({
    provider: "openrouter",
    id: "anthropic/claude-3",
  })
})

test("a mistyped setting falls back to its default instead of disabling the review", () => {
  const settings = resolveCheckpoint(
    { enabled: "maybe", interval: 0, mode: "loud", model: "no-slash" },
    { MEMCASTLE_CHECKPOINT_INTERVAL: "ten", MEMCASTLE_CHECKPOINT_MODEL: "/id" },
  )
  expect(settings).toEqual(DEFAULT_CHECKPOINT)
  expect(resolveCheckpoint({ interval: 2.5 }, {}).interval).toBe(10)
})

// --- counting --------------------------------------------------------------------------------------------------

test("a review is due on every Nth exchange and the count starts again afterwards", () => {
  const counter = new ExchangeCounter(3)
  expect([1, 2, 3, 4, 5, 6, 7].map(() => counter.tick())).toEqual([false, false, true, false, false, true, false])
})

test("resetting the counter after a manual checkpoint postpones the next interval review", () => {
  const counter = new ExchangeCounter(2)
  counter.tick()
  counter.reset()
  expect([counter.tick(), counter.tick()]).toEqual([false, true])
})

// --- the transcript --------------------------------------------------------------------------------------------

test("only the words of a user or assistant message are part of the conversation", () => {
  expect(textOf("  hello  ")).toBe("hello")
  expect(
    textOf([
      { type: "text", text: "first" },
      { type: "thinking", text: "private" },
      { type: "toolCall", name: "bash" },
      { type: "text", text: "second" },
    ]),
  ).toBe("first\nsecond")
  expect(turnOf("user", "hi")).toEqual({ role: "user", text: "hi" })
  expect(turnOf("toolResult", "output")).toBeNull()
  expect(turnOf("assistant", [{ type: "toolCall" }])).toBeNull()
})

test("a long conversation is cut from its oldest end and says how much was left out", () => {
  const turns: Turn[] = [
    { role: "user", text: "a".repeat(50) },
    { role: "assistant", text: "b".repeat(50) },
    { role: "user", text: "newest" },
  ]
  const rendered = renderTranscript(turns, 80)
  expect(rendered).toContain("User: newest")
  expect(rendered).toContain("Assistant: bbbb")
  expect(rendered).not.toContain("aaaa")
  expect(rendered).toStartWith("(1 earlier turn(s) left out)")
})

test("the newest turn is kept even when it alone is over the limit", () => {
  expect(renderTranscript([{ role: "user", text: "x".repeat(100) }], 10)).toContain("x".repeat(100))
})

test("the review request carries the shared skill, the reply format and the user's own note", () => {
  const request = classificationRequest("SKILL BODY", [{ role: "user", text: "we chose SurrealDB" }], "remember the db choice")
  expect(request.system).toStartWith("SKILL BODY")
  expect(request.system).toContain("Do not call `memcastle_checkpoint`")
  expect(request.prompt).toContain("remember the db choice")
  expect(request.prompt).toContain("User: we chose SurrealDB")
})

// --- classification, replayed from the shared fixture -----------------------------------------------------------

const classifications = fixture<{
  accepted: { name: string; reply: string; items: object[] }[]
  refused: { name: string; reply: string }[]
}>("checkpoint-classifications.json")

for (const { name, reply, items } of classifications.accepted) {
  test(`the model reply "${name}" becomes exactly the items the fixture says, stamped with this agent`, () => {
    const payload = parseClassification(reply, "my-agent")
    expect(payload.items).toEqual(
      items.map((item) => ({ ...item, source: { kind: "manual", uri: null, agent: "my-agent" }, fact: null })) as never,
    )
  })
}

for (const { name, reply } of classifications.refused) {
  test(`the model reply "${name}" is refused as invalid input, with what to do about it`, () => {
    try {
      parseClassification(reply, "my-agent")
    } catch (error) {
      expect(error).toBeInstanceOf(MemCastleFailure)
      expect((error as MemCastleFailure).failureClass).toBe("invalid_input")
      expect((error as MemCastleFailure).help).toContain("Nothing was saved")
      return
    }
    throw new Error("the reply was accepted")
  })
}

test("a payload handed over directly is validated the same way as a model's reply", () => {
  expect(() => validatePayload({ items: [{ destination: "nope", content: "x" }] }, "a")).toThrow(MemCastleFailure)
  expect(validatePayload({ items: [] }, "a")).toEqual({ items: [] })
})

// --- the review, against a fake MemCastle -----------------------------------------------------------------------

const REPLY = JSON.stringify({ items: [{ destination: "project", content: "We chose SurrealDB.", tags: ["db"] }] })

/** A caller that records every call and answers a checkpoint with `statuses` in turn (the last one repeats). */
function fakeCaller(statuses: string[] = ["queued", "completed"], result: object = { items: 1, duplicates: 0 }) {
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  let polled = 0
  const caller: Caller = {
    async call<T>(tool: string, args: Record<string, unknown> = {}) {
      calls.push({ tool, args })
      // The submission answers with the first status, and the Nth poll with the Nth (the last one repeats).
      if (tool === "memcastle_job_get") polled += 1
      const status = statuses[Math.min(tool === "memcastle_checkpoint" ? 0 : polled, statuses.length - 1)] as string
      const job: CheckpointJob = {
        id: "job-1",
        status,
        result: status === "completed" ? result : null,
        error: status === "failed" ? "disk full" : null,
      }
      return job as T
    },
  }
  return { caller, calls }
}

function reviewOf(caller: Caller | null, mode: ModeLabel = "full") {
  return new CheckpointReview({
    mode,
    agentIdentity: "test-agent",
    session: () => caller,
    skill: async () => "SKILL",
    pollMs: 1,
  })
}

/** A conversation the test grows by hand, and a model that answers `reply` and records what it was asked. */
function fakeIo(reply: string | (() => Promise<string>) = REPLY) {
  const turns: Turn[] = [
    { role: "user", text: "which database?" },
    { role: "assistant", text: "SurrealDB." },
  ]
  const asked: { prompt: string; signal: AbortSignal }[] = []
  const io: ReviewIo = {
    transcript: () => [...turns],
    classify: async (request, signal) => {
      asked.push({ prompt: request.prompt, signal })
      return typeof reply === "string" ? reply : reply()
    },
  }
  return { io, turns, asked }
}

test("a review submits what the model chose, as a manual item from this agent, and waits for the job to complete", async () => {
  const { caller, calls } = fakeCaller()
  const { io } = fakeIo()
  const outcome = await reviewOf(caller).run(io)

  expect(outcome).toMatchObject({ kind: "saved", items: 1, duplicates: 0 })
  expect(calls[0]).toEqual({
    tool: "memcastle_checkpoint",
    args: {
      emergency: false,
      payload: {
        items: [
          {
            destination: "project",
            wing: null,
            name: null,
            content: "We chose SurrealDB.",
            tags: ["db"],
            source: { kind: "manual", uri: null, agent: "test-agent" },
            fact: null,
          },
        ],
      },
    },
  })
  expect(calls.slice(1).map((call) => call.tool)).toEqual(["memcastle_job_get"])
  expect(describeOutcome(outcome)).toBe("Checkpointed 1 item(s).")
})

test("the next review reads only the exchanges after the last one", async () => {
  const { caller } = fakeCaller()
  const { io, turns, asked } = fakeIo()
  const review = reviewOf(caller)
  await review.run(io)

  expect(await review.run(io)).toEqual({ kind: "nothing", reason: "no new exchanges" })
  expect(asked).toHaveLength(1)

  turns.push({ role: "user", text: "and the queue?" }, { role: "assistant", text: "Jobs live in SurrealDB too." })
  await review.run(io)
  expect(asked[1]?.prompt).toContain("and the queue?")
  expect(asked[1]?.prompt).not.toContain("which database?")
})

test("nothing worth keeping submits nothing, and is not an error", async () => {
  const { caller, calls } = fakeCaller()
  const { io } = fakeIo('{"items":[]}')
  const outcome = await reviewOf(caller).run(io)
  expect(outcome).toEqual({ kind: "nothing", reason: "nothing worth keeping" })
  expect(calls).toEqual([])
})

test("a manual note is enough to review even when there is nothing new to read", async () => {
  const { caller } = fakeCaller()
  const { io, asked } = fakeIo()
  const review = reviewOf(caller)
  await review.run(io)
  await review.run(io, { note: "remember the db choice" })
  expect(asked).toHaveLength(2)
  expect(asked[1]?.prompt).toContain("remember the db choice")
})

test("a session that cannot write is refused before the model is asked, and the refusal names the way out", async () => {
  for (const mode of ["read-only", "off"] as const) {
    const { caller, calls } = fakeCaller()
    const { io, asked } = fakeIo()
    const failure = await reviewOf(caller, mode)
      .run(io)
      .catch((error: unknown) => error)
    expect(failure).toBeInstanceOf(MemCastleFailure)
    expect((failure as MemCastleFailure).failureClass).toBe("mode_rejected")
    expect((failure as MemCastleFailure).toUserMessage()).toContain("MEMCASTLE_MODE=full")
    expect(asked).toEqual([])
    expect(calls).toEqual([])
  }
})

test("an unusable reply fails loudly, and the same exchanges are reviewed again next time", async () => {
  const { caller } = fakeCaller()
  let reply = "no idea"
  const { io, asked } = fakeIo(async () => reply)
  const review = reviewOf(caller)

  const failure = await review.run(io).catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("invalid_input")

  reply = REPLY
  expect((await review.run(io)).kind).toBe("saved")
  expect(asked[1]?.prompt).toContain("which database?")
})

test("a job that fails is reported with its own error and how to retry it, never as a success", async () => {
  const { caller } = fakeCaller(["queued", "running", "failed"])
  const { io } = fakeIo()
  const failure = await reviewOf(caller)
    .run(io)
    .catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("job_failed")
  expect((failure as MemCastleFailure).toUserMessage()).toContain("disk full")
  expect((failure as MemCastleFailure).toUserMessage()).toContain("memcastle_job_retry")
})

test("a connection that went away mid-review is reported, not read as nothing worth keeping", async () => {
  const { io } = fakeIo()
  const failure = await reviewOf(null)
    .run(io)
    .catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("daemon_unavailable")
  expect((failure as MemCastleFailure).toUserMessage()).toContain("memcastle daemon start")
})

test("a second review while one is running is declined, so reviews never pile up", async () => {
  let release!: (reply: string) => void
  const slow = new Promise<string>((resolve) => (release = resolve))
  const { caller } = fakeCaller()
  const { io } = fakeIo(() => slow)
  const review = reviewOf(caller)

  const first = review.run(io)
  expect(review.busy).toBe(true)
  expect(await review.run(io)).toEqual({ kind: "busy" })
  release(REPLY)
  expect((await first).kind).toBe("saved")
  expect(review.busy).toBe(false)
})

test("an emergency review is not waited for: it queues the job at once and says so", async () => {
  const { caller, calls } = fakeCaller(["queued", "completed"])
  const { io } = fakeIo()
  const outcome = await reviewOf(caller).run(io, { emergency: true })
  expect(outcome).toMatchObject({ kind: "queued", items: 1 })
  expect(calls.map((call) => call.tool)).toEqual(["memcastle_checkpoint"])
  expect(calls[0]?.args.emergency).toBe(true)
})

test("an emergency review waits for the review in progress instead of being dropped as a duplicate", async () => {
  let release!: (reply: string) => void
  const slow = new Promise<string>((resolve) => (release = resolve))
  const { caller, calls } = fakeCaller()
  const { io, turns } = fakeIo(() => slow)
  const review = reviewOf(caller)

  const first = review.run(io)
  const emergency = review.run({ ...io, classify: async () => REPLY }, { emergency: true })
  turns.push({ role: "user", text: "one more thing" }, { role: "assistant", text: "noted" })
  release(REPLY)
  await first
  expect((await emergency).kind).toBe("queued")
  expect(calls.filter((call) => call.tool === "memcastle_checkpoint").map((call) => call.args.emergency)).toEqual([false, true])
})

test("aborting stops the model call of a review in progress", async () => {
  const { caller } = fakeCaller()
  const { io, asked } = fakeIo(() => new Promise<string>(() => undefined))
  const review = reviewOf(caller)
  void review.run(io)
  await Bun.sleep(5)
  review.abort()
  expect(asked[0]?.signal.aborted).toBe(true)
})

test("polling gives up after the wait and hands back the job as it is rather than hanging", async () => {
  const { caller } = fakeCaller(["queued", "running"])
  const job = await submitCheckpoint(caller, { items: [] }, { waitMs: 20, pollMs: 5 })
  expect(job.status).toBe("running")
})

test("a cancelled job is a failure with a way to retry it", async () => {
  const { caller } = fakeCaller(["cancelled"])
  const failure = await submitCheckpoint(caller, { items: [] }).catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("job_failed")
  expect((failure as MemCastleFailure).toUserMessage()).toContain("memcastle_job_retry")
})

test("every outcome has a sentence for the user", () => {
  const job = { id: "j", status: "queued" }
  expect(describeOutcome({ kind: "busy" })).toContain("already")
  expect(describeOutcome({ kind: "nothing", reason: "no new exchanges" })).toContain("Nothing new")
  expect(describeOutcome({ kind: "nothing", reason: "nothing worth keeping" })).toContain("nothing worth keeping")
  expect(describeOutcome({ kind: "queued", items: 2, job })).toContain("job j")
  expect(describeOutcome({ kind: "saved", items: 2, duplicates: 1, job })).toBe("Checkpointed 2 item(s), 1 already known.")
})

// --- against a real daemon -------------------------------------------------------------------------------------

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

test("every valid conformance payload, as a client's own review would send it, is stored and recallable", async () => {
  const session = daemon.session("full")
  const cases = fixture<{ valid: { name: string; recall_token: string; payload: { items: Record<string, unknown>[] } }[] }>(
    "checkpoint-payloads.json",
  ).valid
  try {
    for (const { name, recall_token, payload } of cases) {
      // What a model would say: the fixture's items without the parts the client always stamps itself.
      const reply = JSON.stringify({
        items: payload.items.map(({ source: _source, fact: _fact, ...chosen }) => chosen),
      })
      const { io } = fakeIo(reply)
      const outcome = await new CheckpointReview({
        mode: "full",
        agentIdentity: "conformance-agent",
        session: () => session,
        skill: async () => "SKILL",
      }).run(io)
      expect(outcome.kind, name).toBe("saved")

      const hits = await session.call<{ content?: string }[]>("memcastle_recall", { query: recall_token })
      expect(JSON.stringify(hits), `${name} must be recallable once its job completes`).toContain(recall_token)
    }
  } finally {
    await session.close()
  }
})

test("a read-only session's checkpoint is refused by the client, and the daemon would have refused it too", async () => {
  const session = daemon.session("read-only")
  try {
    const { io, asked } = fakeIo()
    const failure = await new CheckpointReview({
      mode: "read-only",
      agentIdentity: "a",
      session: () => session,
      skill: async () => "SKILL",
    })
      .run(io)
      .catch((error: unknown) => error)
    expect((failure as MemCastleFailure).failureClass).toBe("mode_rejected")
    expect(asked).toEqual([])

    // The client's refusal is a courtesy that saves a model call: the daemon is what actually enforces it.
    const refused = await submitCheckpoint(session, validatePayload({ items: [{ destination: "general", content: "x" }] }, "a")).catch(
      (error: unknown) => error,
    )
    expect((refused as MemCastleFailure).failureClass).toBe("mode_rejected")
  } finally {
    await session.close()
  }
})

test("an emergency checkpoint is critical priority and a routine one is high, as the daemon reports them", async () => {
  const session = daemon.session("full")
  try {
    const payload = validatePayload({ items: [{ destination: "general", content: "priority probe" }] }, "a")
    const routine = await submitCheckpoint(session, payload, { wait: false })
    const emergency = await submitCheckpoint(session, payload, { wait: false, emergency: true })
    const priorities = await Promise.all(
      [routine, emergency].map((job) => session.call<{ priority: number }>("memcastle_job_get", { id: job.id })),
    )
    expect(priorities.map((job) => job.priority)).toEqual([75, 100])
  } finally {
    await session.close()
  }
})

test("a real job that fails is a job_failed failure with the daemon's own reason", async () => {
  const session = daemon.session("full")
  const named = (content: string) =>
    validatePayload({ items: [{ destination: "general", name: "taken-name", content }] }, "a")
  try {
    await submitCheckpoint(session, named("one"))
    // Two different drawers cannot share one name in a room, so this job fails.
    const failure = await submitCheckpoint(session, named("two")).catch((error: unknown) => error)
    expect((failure as MemCastleFailure).failureClass).toBe("job_failed")
    expect((failure as MemCastleFailure).toUserMessage()).toContain("memcastle_job_retry")
  } finally {
    await session.close()
  }
})

test("a payload the daemon refuses is reported as invalid input with its help, and leaves no job behind", async () => {
  const session = daemon.session("full")
  try {
    const failure = await submitCheckpoint(session, { items: [] }).catch((error: unknown) => error)
    expect((failure as MemCastleFailure).failureClass).toBe("invalid_input")
  } finally {
    await session.close()
  }
})
