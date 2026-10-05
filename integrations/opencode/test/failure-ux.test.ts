import { afterAll, beforeAll, expect, test } from "bun:test"
import type { PluginInput } from "@opencode-ai/plugin"
import type { ReviewHost } from "../src/checkpoint.ts"
import { type Core, type Level, NOTIFY_THROTTLE_MS, type Notify, createCore } from "../src/core.ts"
import plugin from "../src/index.ts"
import { fixture } from "./support/fixtures.ts"
import { TestDaemon } from "./support/daemon.ts"

interface FailureClassFixture {
  id: string
  severity: string
  message_must_contain: string[]
}
const classes = fixture<{ classes: FailureClassFixture[] }>("failure-classes.json").classes
const shouldShow = (id: string) => classes.find((entry) => entry.id === id) as FailureClassFixture

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

const REPLY = JSON.stringify({ items: [{ destination: "project", content: "We chose SurrealDB.", tags: ["db"] }] })
const host: ReviewHost = {
  transcript: async () => [
    { role: "user", text: "which database?" },
    { role: "assistant", text: "SurrealDB." },
  ],
  classify: async (_sessionId, _request, _model, _signal, claim) => (claim("reviewer"), REPLY),
}

/** A core that reviews on every idle event and waits for it, so one idle event is one reported outcome. */
async function coreWith(options: Record<string, unknown> = {}, now?: () => number) {
  const logs: { level: Level; message: string }[] = []
  const toasts: { severity: string; message: string }[] = []
  const notify: Notify = async (severity, message) => void toasts.push({ severity, message })
  const core = (await createCore(
    { wakeUp: { enabled: false }, palacePath: daemon.palacePath, checkpoint: { interval: 1, mode: "blocking" }, ...options },
    async (level, message) => void logs.push({ level, message }),
    daemon.clientEnv,
    "/work/castle",
    host,
    notify,
    now,
  )) as Core
  return { core, logs, toasts }
}

/** The failures a log recorded, which is every line at or above `info` that is not the startup line. */
const failuresOf = (logs: { level: Level; message: string }[]) => logs.filter((entry) => entry.message.startsWith("checkpoint:"))

test("a daemon that is down is a warning toast and log that say how to start it, and the hook still returns", async () => {
  const { core, logs, toasts } = await coreWith({ endpoint: "http://127.0.0.1:1", timeoutMs: 500 })
  await core.sessionIdle("ses_1")

  const down = shouldShow("daemon_unavailable")
  expect(toasts).toHaveLength(1)
  expect(toasts[0]?.severity).toBe(down.severity)
  for (const text of down.message_must_contain) expect(toasts[0]?.message).toContain(text)
  expect(failuresOf(logs)[0]?.level).toBe("warn")
  await core.dispose()
})

test("a refusal by the session's own mode is an information toast and log, never a warning", async () => {
  // The plugin believes it may write while the daemon's session is read-only, so it is the daemon that refuses.
  const { core, logs, toasts } = await coreWith()
  const readOnly = daemon.session("read-only")
  core.sessions.session = () => readOnly
  await core.sessionIdle("ses_1")

  expect(toasts).toHaveLength(1)
  expect(toasts[0]?.severity).toBe(shouldShow("mode_rejected").severity)
  expect(toasts[0]?.message).toContain("read_only")
  expect(failuresOf(logs)[0]?.level).toBe("info")
  expect(logs.some((entry) => entry.level === "warn" || entry.level === "error")).toBe(false)
  await readOnly.close()
  await core.dispose()
})

test("a failed job puts the daemon's own reason on screen with how to retry, not a generic failure", async () => {
  const { core, logs, toasts } = await coreWith()
  core.sessions.session = (() => ({
    call: async () => ({ id: "job-9", status: "failed", error: "the palace is full", kind: { type: "checkpoint" } }),
  })) as never
  await core.sessionIdle("ses_1")

  const failed = shouldShow("job_failed")
  expect(toasts).toHaveLength(1)
  expect(toasts[0]?.severity).toBe(failed.severity)
  expect(toasts[0]?.message).toContain("the palace is full")
  expect(toasts[0]?.message).toContain("checkpoint job job-9")
  for (const text of failed.message_must_contain) expect(toasts[0]?.message).toContain(text)
  expect(failuresOf(logs)[0]?.level).toBe("warn")
  await core.dispose()
})

test("a payload the daemon refuses is a warning that carries the daemon's help", async () => {
  const { core, toasts } = await coreWith()
  const real = daemon.session("full")
  // An empty `items` list is what the daemon refuses as invalid input; the plugin's own validation would stop it first.
  core.sessions.session = (() => ({ call: (_tool: string, _args: unknown) => real.call("memcastle_checkpoint", { payload: { items: [] } }) })) as never
  await core.sessionIdle("ses_1")

  expect(toasts).toHaveLength(1)
  expect(toasts[0]?.severity).toBe(shouldShow("invalid_input").severity)
  await real.close()
  await core.dispose()
})

test("the same failure is not toasted again within the throttle window, but is always logged", async () => {
  let clock = 1_000
  const { core, logs, toasts } = await coreWith({ endpoint: "http://127.0.0.1:1", timeoutMs: 500 }, () => clock)
  await core.sessionIdle("ses_1")
  await core.sessionIdle("ses_1")
  expect(toasts).toHaveLength(1)
  expect(failuresOf(logs)).toHaveLength(2)

  clock += NOTIFY_THROTTLE_MS
  await core.sessionIdle("ses_1")
  expect(toasts).toHaveLength(2)
  await core.dispose()
})

test("a toast that cannot be shown never turns a reported failure into a thrown one", async () => {
  const logs: { level: Level; message: string }[] = []
  const core = (await createCore(
    { wakeUp: { enabled: false }, endpoint: "http://127.0.0.1:1", timeoutMs: 500, checkpoint: { interval: 1, mode: "blocking" } },
    async (level, message) => void logs.push({ level, message }),
    daemon.clientEnv,
    "/work/castle",
    host,
    async () => {
      throw new Error("no TUI")
    },
  )) as Core
  await core.sessionIdle("ses_1")
  expect(failuresOf(logs)[0]?.level).toBe("warn")
  await core.dispose()
})

test("the OpenCode 1 plugin shows a failure as a titled toast in the TUI, and survives a client with no TUI", async () => {
  const toasts: { title?: string; message: string; variant: string }[] = []
  const logs: { level: string; message: string }[] = []
  const withTui = {
    app: { log: async ({ body }: { body: { level: string; message: string } }) => (logs.push(body), {}) },
    tui: { showToast: async ({ body }: { body: { title?: string; message: string; variant: string } }) => (toasts.push(body), true) },
  }
  const options = {
    endpoint: "http://127.0.0.1:1",
    timeoutMs: 500,
    wakeUp: { enabled: true, mode: "sync" },
    forceMemoryRecall: { level: "off" },
  }
  // The wake-up is the first thing a session asks the daemon for, so a daemon that is down is met right here.
  const meet = async (client: object) => {
    const hooks = await plugin.server({ client, directory: "/work/castle" } as unknown as PluginInput, options)
    const info = { id: "ses_1", directory: "/work/castle" }
    await hooks.event?.({ event: { type: "session.created", properties: { info } } as never })
    await hooks["experimental.chat.system.transform"]?.({ sessionID: "ses_1" } as never, { system: [] })
    await hooks.dispose?.()
  }

  await meet(withTui)
  expect(toasts).toHaveLength(1)
  expect(toasts[0]).toMatchObject({ title: "MemCastle", variant: shouldShow("daemon_unavailable").severity })
  expect(toasts[0]?.message).toContain("memcastle daemon start")
  expect(logs.some((entry) => entry.level === "warn" && entry.message.startsWith("wake-up:"))).toBe(true)

  // `opencode run` has a client with no `tui`: the toast cannot be shown, and the hook must still not throw.
  await meet({ app: withTui.app })
})
