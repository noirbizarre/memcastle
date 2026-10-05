// Finding the daemon the dev server proxies `/api` to.
//
// The dashboard is a client of the REST API and nothing else, so development needs only an address. It is looked up the
// way the integrations look it up (their `daemon-client.ts`): an explicit `MEMCASTLE_URL`, else the registry file the
// running daemon writes for its palace, verified with the one route that needs no token. The few lines are copied, not
// imported: `web/` must install and build on its own, with none of an integration's dependencies (docs/adr/035).

import { createHash } from "node:crypto"
import { readFile, realpath } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"

type Env = Readonly<Record<string, string | undefined>>

/** An absolute `$NAME`, or the home-relative default; a relative value is ignored, as the daemon ignores it. */
function xdg(env: Env, name: string, fallback: string): string {
  const value = env[name]?.trim()
  return value?.startsWith("/") ? value : join(env.HOME ?? homedir(), fallback)
}

/** The palace the daemon serves by default: `$XDG_DATA_HOME/memcastle/default`. */
export function defaultPalace(env: Env = process.env): string {
  return env.MEMCASTLE_PALACE_PATH?.trim() || join(xdg(env, "XDG_DATA_HOME", ".local/share"), "memcastle", "default")
}

/** Where the registry file for `palacePath` lives: `<state>/memcastle/run/<sha256(canonical path)[..16]>/daemon.json`. */
export async function registryPath(palacePath: string, env: Env = process.env): Promise<string> {
  const canonical = await realpath(palacePath).catch(() => palacePath)
  const digest = createHash("sha256").update(canonical).digest("hex").slice(0, 16)
  return join(xdg(env, "XDG_STATE_HOME", ".local/state"), "memcastle", "run", digest, "daemon.json")
}

/** `0.0.0.0` and `::` are listen addresses, not places to connect to. */
export function connectable(bindAddr: string): string {
  if (bindAddr.startsWith("0.0.0.0:")) return `127.0.0.1:${bindAddr.slice("0.0.0.0:".length)}`
  if (bindAddr.startsWith("[::]:")) return `[::1]:${bindAddr.slice("[::]:".length)}`
  return bindAddr
}

export async function isHealthy(baseUrl: string, timeoutMs = 1000): Promise<boolean> {
  try {
    const response = await fetch(`${baseUrl}/api/health`, { signal: AbortSignal.timeout(timeoutMs) })
    return response.ok
  } catch {
    return false
  }
}

/**
 * The daemon's base URL: `MEMCASTLE_URL`, else the registry entry of the palace, else the default listener.
 * Never throws: a dev server started before the daemon still starts, and its proxy errors say what is missing.
 */
export async function discoverDaemon(env: Env = process.env): Promise<string> {
  const explicit = env.MEMCASTLE_URL?.trim().replace(/\/+$/, "")
  if (explicit) return explicit
  try {
    const registry = JSON.parse(await readFile(await registryPath(defaultPalace(env), env), "utf8")) as { bind_addr?: string }
    if (registry.bind_addr) {
      const url = `http://${connectable(registry.bind_addr)}`
      if (await isHealthy(url)) return url
    }
  } catch {
    // No registry file: nothing is running for this palace, or it is elsewhere.
  }
  return "http://127.0.0.1:8420"
}
