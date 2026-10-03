import { afterAll, beforeAll, expect, test } from "bun:test"
import { mkdir, readFile, writeFile } from "node:fs/promises"
import { dirname } from "node:path"
import { connectable, discoverDaemon, registryPath } from "../src/daemon-client.ts"
import { MemCastleFailure } from "../src/failures.ts"
import { resolveSettings } from "../src/settings.ts"
import { TestDaemon } from "./support/daemon.ts"

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
})
afterAll(async () => {
  await daemon.stop()
})

test("the registry path this client computes is the file the real daemon wrote", async () => {
  const path = await registryPath(daemon.palacePath, daemon.clientEnv)
  const info = JSON.parse(await readFile(path, "utf8"))
  expect(info.bind_addr).toMatch(/^127\.0\.0\.1:\d+$/)
  expect(info.pid).toBeGreaterThan(0)
})

test("a daemon on an OS-assigned port is found through its registry file", async () => {
  const endpoint = await discoverDaemon(daemon.settings(), daemon.clientEnv)
  expect(endpoint.source).toBe("registry")
  expect(endpoint.mcpUrl).toBe(`${endpoint.baseUrl}/mcp`)
})

test("an explicit endpoint skips discovery but is still checked with a live request", async () => {
  const live = await discoverDaemon(daemon.settings(), daemon.clientEnv)
  const explicit = await discoverDaemon(daemon.settings({ endpoint: `${live.baseUrl}/` }), daemon.clientEnv)
  expect(explicit).toMatchObject({ source: "explicit", baseUrl: live.baseUrl })
})

test("a stale registry file is not trusted: discovery falls through to the configured address", async () => {
  const live = await discoverDaemon(daemon.settings(), daemon.clientEnv)
  const port = Number(new URL(live.baseUrl).port)
  // A palace nobody serves, whose registry file points at a dead port, while the configured port is the live one.
  const palace = `${daemon.palacePath}-stale`
  const file = await registryPath(palace, daemon.clientEnv)
  await mkdir(dirname(file), { recursive: true })
  await writeFile(file, JSON.stringify({ pid: 1, bind_addr: "127.0.0.1:1", started_at: "x", version: "0" }))
  const settings = resolveSettings({ palacePath: palace, port, timeoutMs: 500 }, daemon.clientEnv)
  expect((await discoverDaemon(settings, daemon.clientEnv)).source).toBe("config")
})

test("no daemon anywhere is a daemon_unavailable failure that says how to start one", async () => {
  const settings = resolveSettings({ palacePath: `${daemon.palacePath}-none`, port: 1, timeoutMs: 500 }, daemon.clientEnv)
  const failure = await discoverDaemon(settings, daemon.clientEnv).catch((error) => error)
  expect(failure).toBeInstanceOf(MemCastleFailure)
  expect(failure.failureClass).toBe("daemon_unavailable")
  expect(failure.toUserMessage()).toContain("memcastle daemon start")
})

test("a wildcard bind address is rewritten to loopback before dialling", () => {
  expect(connectable("0.0.0.0:8420")).toBe("127.0.0.1:8420")
  expect(connectable("[::]:8420")).toBe("[::1]:8420")
  expect(connectable("192.168.1.5:8420")).toBe("192.168.1.5:8420")
  expect(connectable("[::1]:8420")).toBe("[::1]:8420")
  expect(connectable("not an address")).toBe("not an address")
})
