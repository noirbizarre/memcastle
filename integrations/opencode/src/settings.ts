// Settings: plugin options first, then the environment variables MemCastle's own CLI already reads.
//
// An integration follows the daemon's conventions instead of inventing its own, so a user who has set
// `MEMCASTLE_AUTH_TOKEN` or `MEMCASTLE_PALACE_PATH` for the CLI gets the same behaviour here.

import { homedir } from "node:os"
import { join } from "node:path"
import { type ModeLabel, toWireMode } from "./modes.ts"
import { DEFAULT_KEEP_ALIVE_MS } from "./session.ts"
import { type WakeUpSettings, resolveWakeUp } from "./wake-up-core.ts"

export interface Settings {
  /** An explicit `http://host:port`, which skips discovery. */
  endpoint: string | null
  /** The bearer token, if the daemon needs one. Never logged: use {@link describeSettings} for output. */
  token: string | null
  /** The palace whose registry file locates the daemon. */
  palacePath: string
  /** The configured listener, used when no registry file points at a live daemon. */
  bind: string
  port: number
  /** The mode this integration selects for every session it opens. */
  mode: ModeLabel
  /** A free string stored with diary and wake-up calls; the daemon never validates it. */
  agentIdentity: string
  /** How long to wait for the daemon before giving up on a call. */
  timeoutMs: number
  /** How often an open connection is pinged so the daemon does not drop it as idle; `0` turns that off. */
  keepAliveMs: number
  /**
   * Wake-up on session start. Nested because `mode` above is the memory mode, and wake-up's own `mode`
   * (`sync` or `async`) is a different thing that must not be confused with it.
   */
  wakeUp: WakeUpSettings
}

type Env = Readonly<Record<string, string | undefined>>

/** A non-blank string, or a number written as one (a port in a JSON options object is a number). */
function text(value: unknown): string | null {
  if (typeof value === "number") return String(value)
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null
}

/** The default palace, as the daemon resolves it: `$XDG_DATA_HOME/memcastle/default`. */
function defaultPalacePath(env: Env): string {
  const data = text(env.XDG_DATA_HOME)
  // The daemon ignores a relative XDG path, and so must this, or the registry hash would differ.
  const base = data?.startsWith("/") ? data : join(env.HOME ?? homedir(), ".local", "share")
  return join(base, "memcastle", "default")
}

/** A non-negative number of milliseconds, or the default for anything missing or invalid. */
function keepAlive(value: unknown): number {
  const ms = typeof value === "number" || typeof value === "string" ? Number(value) : Number.NaN
  return Number.isFinite(ms) && ms >= 0 && value !== "" ? ms : DEFAULT_KEEP_ALIVE_MS
}

/**
 * Resolve the settings.
 *
 * Throws `InvalidModeError` for a mode that is not `full`, `read-only` or `off`: a typo must not turn a
 * session the user meant to protect into a `full` one, so the caller fails closed.
 */
export function resolveSettings(options: Record<string, unknown> | undefined, env: Env = process.env): Settings {
  const opts = options ?? {}
  const mode = text(opts.mode) ?? text(env.MEMCASTLE_MODE) ?? "full"
  toWireMode(mode) // validates, and throws on anything unknown
  const port = Number(text(opts.port) ?? text(env.MEMCASTLE_PORT) ?? 8420)
  return {
    endpoint: text(opts.endpoint),
    token: text(opts.token) ?? text(env.MEMCASTLE_AUTH_TOKEN),
    palacePath: text(opts.palacePath) ?? text(env.MEMCASTLE_PALACE_PATH) ?? defaultPalacePath(env),
    bind: text(opts.bind) ?? text(env.MEMCASTLE_BIND) ?? "127.0.0.1",
    port: Number.isInteger(port) && port >= 0 && port <= 65535 ? port : 8420,
    mode: mode as ModeLabel,
    agentIdentity: text(opts.agentIdentity) ?? "opencode",
    timeoutMs: Number(opts.timeoutMs) > 0 ? Number(opts.timeoutMs) : 5000,
    // Unlike the timeout, zero is meaningful here (no pings), so only a missing or invalid value falls back.
    keepAliveMs: keepAlive(opts.keepAliveMs),
    wakeUp: resolveWakeUp(opts.wakeUp, env),
  }
}

/** The settings as they may appear in a log: everything but the token. */
export function describeSettings(settings: Settings): Record<string, unknown> {
  const { token, ...rest } = settings
  return { ...rest, token: token === null ? null : "[redacted]" }
}
