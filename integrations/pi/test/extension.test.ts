import { afterEach, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent"
import memcastle from "../src/extension.ts"

type Handler = (event: unknown, ctx: ExtensionContext) => unknown

/** The only parts of Pi the extension touches: `on`, `registerCommand`, and `ui.notify`. */
function fakePi(cwd = "/work/memcastle") {
  const handlers = new Map<string, Handler[]>()
  const notes: { message: string; level: string }[] = []
  const pi = {
    on(event: string, handler: Handler) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler])
      return () => undefined
    },
    // Wake-up registers a command at load; this test only needs it to be accepted.
    registerCommand: () => undefined,
  } as unknown as ExtensionAPI
  const ctx = { cwd, ui: { notify: (message: string, level: string) => notes.push({ message, level }) } } as unknown as ExtensionContext
  const fire = async (event: string, payload: object = {}) => {
    for (const handler of handlers.get(event) ?? []) await handler({ type: event, ...payload }, ctx)
  }
  return { pi, handlers, notes, fire }
}

const saved = { ...process.env }
afterEach(() => {
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
})

async function until(condition: () => boolean, what: string) {
  const deadline = Date.now() + 5000
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`)
    await Bun.sleep(20)
  }
}

test("loading the extension only registers handlers: nothing is opened until a session starts", () => {
  process.env.MEMCASTLE_MODE = "readonly" // would be reported if the factory read settings
  const { pi, handlers, notes } = fakePi()
  memcastle(pi)
  expect([...handlers.keys()].sort()).toEqual(["agent_end", "before_agent_start", "session_before_compact", "session_shutdown", "session_start"])
  expect(notes).toEqual([])
})

test("a session starts at once with the daemon down, and the user is told once how to start it", async () => {
  process.env.MEMCASTLE_PORT = "1"
  process.env.MEMCASTLE_PALACE_PATH = "/nonexistent/memcastle-palace"
  const { pi, notes, fire } = fakePi()
  memcastle(pi)

  await fire("session_start", { reason: "startup" })
  await until(() => notes.length > 0, "the daemon-unavailable notification")

  expect(notes).toHaveLength(1)
  expect(notes[0]?.level).toBe("warning")
  expect(notes[0]?.message).toContain("memcastle daemon start")
  await fire("session_shutdown", { reason: "quit" })
})

test("an unknown mode fails closed: the user is told, and nothing connects", async () => {
  process.env.MEMCASTLE_MODE = "readonly"
  const { pi, notes, fire } = fakePi()
  memcastle(pi)
  await fire("session_start", { reason: "startup" })
  expect(notes).toHaveLength(1)
  expect(notes[0]?.level).toBe("error")
  expect(notes[0]?.message).toContain("read-only")
})

test("an off session leaves MemCastle entirely out of Pi: no connection, no notification", async () => {
  process.env.MEMCASTLE_MODE = "off"
  process.env.MEMCASTLE_PORT = "1"
  const { pi, notes, fire } = fakePi()
  memcastle(pi)
  await fire("session_start", { reason: "startup" })
  await Bun.sleep(100)
  expect(notes).toEqual([])
})

test("shutting down is idempotent, and works when the session never started", async () => {
  const { pi, fire } = fakePi()
  memcastle(pi)
  await fire("session_shutdown", { reason: "quit" })
  await fire("session_shutdown", { reason: "quit" })
})

// --- the project file --------------------------------------------------------------------------------------------

/** A directory whose project file holds an unknown key, which the contract refuses. */
function brokenProject(): string {
  const root = mkdtempSync(join(tmpdir(), "memcastle-pi-project-"))
  mkdirSync(join(root, ".config"), { recursive: true })
  writeFileSync(join(root, ".config/memcastle.toml"), "[memcastle]\ntoken = \"hunter2\"\n")
  return root
}

test("a broken project file is reported once as a warning and the session still starts", async () => {
  const root = brokenProject()
  try {
    process.env.MEMCASTLE_PORT = "1"
    process.env.MEMCASTLE_PALACE_PATH = "/nonexistent/memcastle-palace"
    process.env.HOME = "/nonexistent-home"
    const { pi, notes, fire } = fakePi(root)
    memcastle(pi)
    await fire("session_start", { reason: "startup" })
    await until(() => notes.some((note) => note.message.includes("memcastle daemon start")), "the daemon-unavailable notification")

    const project = notes.filter((note) => note.message.includes("unknown key"))
    expect(project).toHaveLength(1)
    expect(project[0]?.level).toBe("warning")
    expect(project[0]?.message).not.toContain("hunter2")
    await fire("session_shutdown", { reason: "quit" })
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("an off session never reads the project file, so a broken one is not even reported", async () => {
  const root = brokenProject()
  try {
    process.env.MEMCASTLE_MODE = "off"
    process.env.HOME = "/nonexistent-home"
    const { pi, notes, fire } = fakePi(root)
    memcastle(pi)
    await fire("session_start", { reason: "startup" })
    await Bun.sleep(100)
    expect(notes).toEqual([])
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
