// Finding the daemon.
//
// The registry file is a hint, never the truth: a crashed daemon leaves one behind, and a pid can be reused.
// Whether a daemon is there is answered by `GET /api/health`, which is the one route that needs no token.
// Discovery therefore reads the hint, then checks it with a live request, and falls back to the configured address.

import { createHash } from "node:crypto"
import { readFile, realpath } from "node:fs/promises"
import { homedir } from "node:os"
import { isIP } from "node:net"
import { join } from "node:path"
import { failureFromTransport } from "./failures.ts"
import type { Settings } from "./settings.ts"

export interface Endpoint {
  /** `http://host:port`, with no trailing slash. */
  baseUrl: string
  /** The MCP endpoint of that daemon. */
  mcpUrl: string
  /** Where the address came from, for a diagnostic that says so. */
  source: "explicit" | "registry" | "config"
}

type Env = Readonly<Record<string, string | undefined>>

/** `$XDG_STATE_HOME`, or `~/.local/state`; a relative or empty value is ignored, as the daemon ignores it. */
function stateHome(env: Env): string {
  const state = env.XDG_STATE_HOME?.trim()
  return state?.startsWith("/") ? state : join(env.HOME ?? homedir(), ".local", "state")
}

/**
 * Where the registry file for `palacePath` lives: `<state>/memcastle/run/<sha256(canonical path)[..16]>/daemon.json`.
 * Keyed by the canonical path, as the daemon keys it, so two spellings of one directory agree on one file.
 */
export async function registryPath(palacePath: string, env: Env = process.env): Promise<string> {
  // The daemon falls back to the path as given when it does not exist yet, and so does this.
  const canonical = await realpath(palacePath).catch(() => palacePath)
  const digest = createHash("sha256").update(canonical).digest("hex").slice(0, 16)
  return join(stateHome(env), "memcastle", "run", digest, "daemon.json")
}

/**
 * The address to dial for a daemon bound to `bindAddr` (`host:port`).
 * A wildcard address listens everywhere but is not something a client can dial on every platform, so it becomes
 * loopback. Anything that does not parse is passed through untouched.
 */
export function connectable(bindAddr: string): string {
  const split = bindAddr.lastIndexOf(":")
  if (split < 0) return bindAddr
  const host = bindAddr.slice(0, split).replace(/^\[|\]$/g, "")
  const port = bindAddr.slice(split + 1)
  if (host === "0.0.0.0") return `127.0.0.1:${port}`
  if (host === "::") return `[::1]:${port}`
  return isIP(host) === 6 ? `[${host}]:${port}` : bindAddr
}

function endpointFor(baseUrl: string, source: Endpoint["source"]): Endpoint {
  const trimmed = baseUrl.replace(/\/+$/, "")
  return { baseUrl: trimmed, mcpUrl: `${trimmed}/mcp`, source }
}

/** Is a MemCastle daemon answering at `baseUrl`? A refused connection, a timeout or a non-200 all mean no. */
export async function isHealthy(baseUrl: string, timeoutMs: number): Promise<boolean> {
  try {
    const response = await fetch(`${baseUrl}/api/health`, { signal: AbortSignal.timeout(timeoutMs) })
    return response.ok
  } catch {
    return false
  }
}

async function registeredAddress(settings: Settings, env: Env): Promise<string | null> {
  try {
    const info: unknown = JSON.parse(await readFile(await registryPath(settings.palacePath, env), "utf8"))
    const bindAddr = (info as { bind_addr?: unknown }).bind_addr
    return typeof bindAddr === "string" ? connectable(bindAddr) : null
  } catch {
    // A missing, unreadable or malformed file is simply an absent hint.
    return null
  }
}

/**
 * Find a live daemon: an explicit endpoint, then the registry file for the palace, then the configured address.
 * Every candidate is verified with a live request, so a stale registry file falls through to the next one.
 *
 * @throws MemCastleFailure of class `daemon_unavailable`, saying how to start it, when none answers.
 */
export async function discoverDaemon(settings: Settings, env: Env = process.env): Promise<Endpoint> {
  const candidates: Endpoint[] = []
  if (settings.endpoint !== null) candidates.push(endpointFor(settings.endpoint, "explicit"))
  else {
    const registered = await registeredAddress(settings, env)
    if (registered !== null) candidates.push(endpointFor(`http://${registered}`, "registry"))
    candidates.push(endpointFor(`http://${connectable(`${settings.bind}:${settings.port}`)}`, "config"))
  }

  for (const candidate of candidates) {
    if (await isHealthy(candidate.baseUrl, settings.timeoutMs)) return candidate
  }
  const last = candidates[candidates.length - 1] as Endpoint
  throw failureFromTransport(last.baseUrl, new Error("no daemon answered its health check"))
}
