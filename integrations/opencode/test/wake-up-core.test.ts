import { expect, test } from "bun:test"
import {
  DEFAULT_WAKE_UP,
  PendingWakeUp,
  fetchWakeUp,
  renderWakeUp,
  resolveWakeUp,
  toWingName,
  wingFor,
  type Caller,
  type WakeUpSettings,
} from "../src/wake-up-core.ts"

const settings = (overrides: Partial<WakeUpSettings> = {}): WakeUpSettings => ({ ...DEFAULT_WAKE_UP, ...overrides })

/** A promise the test settles by hand, which is how "genuinely blocks" is observed without sleeping. */
function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

/** Whether `promise` has settled yet, after the event loop has had every chance to settle it. */
async function isSettled(promise: Promise<unknown>): Promise<boolean> {
  let settled = false
  void promise.then(
    () => (settled = true),
    () => (settled = true),
  )
  await Bun.sleep(20)
  return settled
}

// --- settings -------------------------------------------------------------------------------------------------

test("wake-up is on, asynchronous and scoped to the project when nothing is configured", () => {
  expect(resolveWakeUp(undefined, {})).toEqual({ enabled: true, mode: "async", source: "project", wing: null })
})

test("the wake-up options win over the environment, key by key", () => {
  const env = {
    MEMCASTLE_WAKE_UP: "false",
    MEMCASTLE_WAKE_UP_MODE: "sync",
    MEMCASTLE_WAKE_UP_SOURCE: "user",
  }
  expect(resolveWakeUp(undefined, env)).toMatchObject({ enabled: false, mode: "sync", source: "user" })
  expect(resolveWakeUp({ enabled: true, mode: "async" }, env)).toMatchObject({
    enabled: true,
    mode: "async",
    source: "user",
  })
})

test("the environment variables an operator would write are understood, in any case", () => {
  for (const [value, expected] of [["TRUE", true], ["1", true], ["On", true], ["no", false], ["0", false], ["OFF", false]] as const) {
    expect(resolveWakeUp(undefined, { MEMCASTLE_WAKE_UP: value }).enabled).toBe(expected)
  }
  expect(resolveWakeUp(undefined, { MEMCASTLE_WAKE_UP_MODE: "SYNC" }).mode).toBe("sync")
})

test("a mistyped wake-up value falls back to its default instead of breaking the session", () => {
  const settings = resolveWakeUp({ enabled: "maybe", mode: "later", source: "everywhere" }, {})
  expect(settings).toEqual(DEFAULT_WAKE_UP)
})

test("a custom source takes its wing from the options or the environment", () => {
  expect(resolveWakeUp({ source: "custom", wing: "notes" }, {})).toMatchObject({ source: "custom", wing: "notes" })
  expect(resolveWakeUp(undefined, { MEMCASTLE_WAKE_UP_SOURCE: "custom", MEMCASTLE_WAKE_UP_WING: "notes" })).toMatchObject({
    source: "custom",
    wing: "notes",
  })
})

test("a custom source with no wing falls back to the project, because there is nothing else to ask about", () => {
  expect(resolveWakeUp({ source: "custom" }, {})).toMatchObject({ source: "project", wing: null })
  expect(resolveWakeUp({ source: "custom", wing: "  " }, {})).toMatchObject({ source: "project", wing: null })
})

test("a wing is ignored unless the source is custom", () => {
  expect(resolveWakeUp({ source: "user", wing: "notes" }, {})).toMatchObject({ source: "user", wing: null })
})

// --- the wing a session asks about ---------------------------------------------------------------------------

test("each source maps to the wing it names", () => {
  expect(wingFor(settings({ source: "user" }), "/work/memcastle")).toBe("preferences")
  expect(wingFor(settings({ source: "project" }), "/work/memcastle")).toBe("memcastle")
  expect(wingFor(settings({ source: "custom", wing: "notes" }), "/work/memcastle")).toBe("notes")
  expect(wingFor(settings({ source: "none" }), "/work/memcastle")).toBeUndefined()
})

test("a trailing slash does not change the project wing", () => {
  expect(wingFor(settings(), "/work/memcastle/")).toBe("memcastle")
})

test("the filesystem root names no project, so no wing is asked about rather than a guess", () => {
  expect(wingFor(settings(), "/")).toBeUndefined()
})

test("a directory name the daemon would refuse is made acceptable", () => {
  expect(toWingName("  spaced  ")).toBe("spaced")
  expect(toWingName("a/b")).toBe("a-b")
  expect(toWingName("tab\there")).toBe("tabhere")
  expect(toWingName("\n")).toBeNull()
  expect(toWingName("123e4567-e89b-12d3-a456-426614174000")).toBe("project-123e4567-e89b-12d3-a456-426614174000")
  expect(toWingName("123e4567e89b12d3a456426614174000")).toBe("project-123e4567e89b12d3a456426614174000")
})

// --- the briefing ---------------------------------------------------------------------------------------------

test("an empty palace renders nothing at all, not even a heading", () => {
  expect(renderWakeUp({ diary: null, recent_highlights: [] })).toBeNull()
  expect(renderWakeUp({})).toBeNull()
  expect(renderWakeUp({ diary: { content: "  " }, recent_highlights: [{ content: "" }, {}] })).toBeNull()
})

test("the diary and every highlight are quoted verbatim", () => {
  const rendered = renderWakeUp({
    diary: { content: "Shipped the daemon.\nNext: wake-up." },
    recent_highlights: [{ content: "Prefers tabs" }, { content: "Uses SurrealDB" }],
  })
  expect(rendered).toContain("Shipped the daemon.\nNext: wake-up.")
  expect(rendered).toContain("Prefers tabs")
  expect(rendered).toContain("Uses SurrealDB")
  expect(rendered).toContain("established fact")
})

test("a briefing with only highlights has no diary section, and one with only a diary has no highlights section", () => {
  expect(renderWakeUp({ recent_highlights: [{ content: "h" }] })).not.toContain("diary")
  expect(renderWakeUp({ diary: { content: "d" } })).not.toContain("highlights")
})

test("fetching asks the daemon with the identity and the wing, and leaves the wing out when there is none", async () => {
  const calls: { tool: string; args: unknown }[] = []
  const caller: Caller = {
    async call(tool, args) {
      calls.push({ tool, args })
      return { diary: null, recent_highlights: [] } as never
    },
  }
  await fetchWakeUp(caller, "pi", "memcastle")
  await fetchWakeUp(caller, "pi", undefined)
  expect(calls).toEqual([
    { tool: "memcastle_wake_up", args: { agent_identity: "pi", wing: "memcastle" } },
    { tool: "memcastle_wake_up", args: { agent_identity: "pi" } },
  ])
})

// --- the in-flight request ------------------------------------------------------------------------------------

test("a synchronous first request genuinely waits until the wake-up call resolves", async () => {
  const call = deferred<string | null>()
  const pending = new PendingWakeUp(() => call.promise, () => undefined)

  const first = pending.available("sync", 10_000)
  expect(await isSettled(first)).toBe(false)

  call.resolve("briefing")
  expect(await first).toBe("briefing")
})

test("an asynchronous first request does not wait, and the briefing is there once the call has resolved", async () => {
  const call = deferred<string | null>()
  const pending = new PendingWakeUp(() => call.promise, () => undefined)

  expect(await pending.available("async", 10_000)).toBeNull()

  call.resolve("briefing")
  await pending.whenSettled
  expect(await pending.available("async", 10_000)).toBe("briefing")
})

test("a synchronous request gives up waiting after the timeout, and a later request picks the briefing up", async () => {
  const call = deferred<string | null>()
  const pending = new PendingWakeUp(() => call.promise, () => undefined)

  const started = Date.now()
  expect(await pending.available("sync", 50)).toBeNull()
  expect(Date.now() - started).toBeLessThan(2000)

  call.resolve("late briefing")
  await pending.whenSettled
  expect(await pending.available("sync", 50)).toBe("late briefing")
})

test("only the first synchronous request waits: a slow daemon does not stall every later one", async () => {
  const call = deferred<string | null>()
  const pending = new PendingWakeUp(() => call.promise, () => undefined)

  expect(await pending.available("sync", 20)).toBeNull()
  const second = pending.available("sync", 10_000)
  expect(await isSettled(second)).toBe(true)
})

test("a failing call is reported once and leaves no briefing, without ever rejecting", async () => {
  const errors: unknown[] = []
  const pending = new PendingWakeUp(async () => {
    throw new Error("daemon down")
  }, (error) => errors.push(error))

  expect(await pending.available("sync", 1000)).toBeNull()
  expect(await pending.available("async", 1000)).toBeNull()
  expect(errors).toHaveLength(1)
  expect(pending.settled).toBe(true)
})

test("a request that throws before returning a promise is a failure like any other", async () => {
  const errors: unknown[] = []
  const pending = new PendingWakeUp(() => {
    throw new Error("sync throw")
  }, (error) => errors.push(error))
  expect(await pending.available("sync", 1000)).toBeNull()
  expect(errors).toHaveLength(1)
})

test("a notifier that throws does not turn a handled failure into an unhandled one", async () => {
  const pending = new PendingWakeUp(async () => {
    throw new Error("daemon down")
  }, () => {
    throw new Error("notifier broke")
  })
  expect(await pending.available("sync", 1000)).toBeNull()
})
