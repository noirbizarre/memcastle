import { afterAll, beforeAll, expect, test } from "bun:test"
import { MemCastleFailure } from "../src/failures.ts"
import { fixture } from "./support/fixtures.ts"
import { McpManager } from "../src/mcp-manager.ts"
import { TestDaemon } from "./support/daemon.ts"

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

function notes() {
  const seen: { message: string; level: string }[] = []
  return { seen, notify: (message: string, level: "info" | "warning" | "error") => seen.push({ message, level }) }
}

function manager(mode: "full" | "read-only" | "off" = "full", overrides: Record<string, unknown> = {}) {
  return new McpManager(daemon.settings({ mode, ...overrides }), daemon.clientEnv)
}

test("starting opens one session in the chosen mode and says nothing when all is well", async () => {
  const { seen, notify } = notes()
  const mcp = manager("read-only")
  expect(await mcp.start(notify)).toBe(true)
  expect(await mcp.session?.reportedMode()).toBe("read-only")
  expect(seen).toEqual([])
  await mcp.stop()
})

test("stopping is idempotent and leaves no session behind", async () => {
  const mcp = manager()
  await mcp.start(() => undefined)
  await mcp.stop()
  await mcp.stop()
  expect(mcp.session).toBeNull()
})

test("starting again replaces the connection instead of leaking the old one, as a reload does", async () => {
  const mcp = manager()
  await mcp.start(() => undefined)
  const first = mcp.session
  await mcp.start(() => undefined)
  expect(mcp.session).not.toBe(first)
  expect(first?.connected).toBe(false)
  expect(mcp.session?.connected).toBe(true)
  await mcp.stop()
})

test("a session that ends while the handshake is in flight is neither connected nor reported", async () => {
  const { seen, notify } = notes()
  const mcp = manager()
  const starting = mcp.start(notify)
  await mcp.stop()
  expect(await starting).toBe(false)
  expect(mcp.session).toBeNull()
  expect(seen).toEqual([])
})

test("an unreachable daemon is reported once, as a warning that says how to start it, and start still returns", async () => {
  const { seen, notify } = notes()
  const mcp = manager("full", { palacePath: `${daemon.palacePath}-none`, endpoint: "http://127.0.0.1:1", timeoutMs: 500 })
  expect(await mcp.start(notify)).toBe(false)
  expect(seen).toHaveLength(1)
  expect(seen[0]?.level).toBe("warning")
  expect(seen[0]?.message).toContain("memcastle daemon start")
  await mcp.stop()
})

test("a refusal by the session's own mode is information, and any other class is a warning or an error", () => {
  const { seen, notify } = notes()
  const mcp = manager()
  mcp.report(new MemCastleFailure("mode_rejected", "refused", "memcastle::mode::forbidden", "It is read-only."), notify)
  mcp.report(new MemCastleFailure("unauthorized", "needs a token", null, "Set MEMCASTLE_AUTH_TOKEN."), notify)
  mcp.report(new MemCastleFailure("unexpected", "boom"), notify)
  mcp.report(new Error("plain"), notify)
  expect(seen.map((entry) => entry.level)).toEqual(["info", "warning", "error", "error"])
  expect(seen[0]?.message).toContain("It is read-only.")
})

test("a failure the real daemon sends is shown with the severity the shared fixture promises for its class", async () => {
  const classes = fixture<{ classes: { id: string; severity: string }[] }>("failure-classes.json").classes
  const severityOf = (id: string) => classes.find((entry) => entry.id === id)?.severity ?? "missing"
  const { seen, notify } = notes()

  // A read-only session asking for a write: the daemon refuses it, and that is the session's own choice at work.
  const readOnly = manager("read-only")
  await readOnly.start(notify)
  const refused = await readOnly.session
    ?.call("memcastle_checkpoint", {
      payload: {
        items: [{ destination: "general", content: "x", tags: [], source: { kind: "manual", uri: null, agent: "test-agent" }, fact: null }],
      },
    })
    .catch((error: unknown) => error)
  readOnly.report(refused, notify)
  await readOnly.stop()

  // A full session sending an empty payload: the daemon refuses it as malformed.
  const full = manager("full")
  await full.start(notify)
  const malformed = await full.session?.call("memcastle_checkpoint", { payload: { items: [] } }).catch((error: unknown) => error)
  full.report(malformed, notify)
  await full.stop()

  expect(seen.map((entry) => entry.level)).toEqual([severityOf("mode_rejected"), severityOf("invalid_input")])
  // The daemon's own words reach the user, including what to do next.
  expect(seen[0]?.message.startsWith("MemCastle: ")).toBe(true)
  expect(seen[0]?.message).toContain("is not permitted")
})
