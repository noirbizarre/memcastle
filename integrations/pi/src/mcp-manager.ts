// Owns the one MCP session a Pi session has with MemCastle, and turns its failures into something the user sees.
//
// Pi runs one agent session at a time, so there is one connection to look after (the OpenCode integration, which
// shares a process between sessions, keeps a registry instead). The connection is opened when the Pi session starts
// and closed when it ends, and between the two it is the same MCP session, so the memory mode chosen at the start
// holds for every call.

import { discoverDaemon } from "./daemon-client.ts"
import { type Severity, presentFailure } from "./failures.ts"
import { McpSession } from "./persistent-mcp-client.ts"
import type { ProjectContext } from "./project-core.ts"
import type { Settings } from "./settings.ts"

type Env = Readonly<Record<string, string | undefined>>

/** How a failure is shown. Pi's notification levels are `info`, `warning` and `error`. */
export type Notify = (message: string, level: Severity) => void

/** A session for `settings`, not yet connected. `env` says where the registry file is, and exists for the tests. */
export function createSession(settings: Settings, env: Env = process.env): McpSession {
  return new McpSession({
    endpoint: () => discoverDaemon(settings, env),
    token: settings.token,
    mode: settings.mode,
    timeoutMs: settings.timeoutMs,
    keepAliveMs: settings.keepAliveMs,
    clientName: "memcastle-pi",
  })
}

export class McpManager {
  private current: McpSession | null = null
  private starting: Promise<boolean> = Promise.resolve(false)

  constructor(
    readonly settings: Settings,
    private readonly env: Env = process.env,
    /** The project this Pi session works in (`.config/memcastle.toml` and `MEMCASTLE_*`), or `null` for none. */
    readonly project: ProjectContext | null = null,
  ) {}

  /** The live session, or `null` before `start` and after `stop`. */
  get session(): McpSession | null {
    return this.current
  }

  /**
   * Open the session for this Pi session and report, once, if the daemon cannot be used.
   * A down daemon never blocks or fails the Pi session: the extension carries on without MemCastle and says so.
   * Returns whether MemCastle is usable.
   */
  start(notify: Notify): Promise<boolean> {
    const starting = this.open(notify)
    this.starting = starting
    return starting
  }

  /**
   * Whether the latest `start` ended with a usable connection. Resolves `false` before any `start`.
   * A capability that needs the connection waits on this instead of connecting again: a failed start has already
   * told the user, and a second attempt from the capability would tell them twice.
   */
  get ready(): Promise<boolean> {
    return this.starting
  }

  private async open(notify: Notify): Promise<boolean> {
    // The same Pi session can be started twice (a reload), and the old connection must not be leaked.
    // `current` is replaced before anything is awaited, so a `stop` that arrives next sees the new session.
    const previous = this.current
    const session = createSession(this.settings, this.env)
    this.current = session
    await previous?.close()
    try {
      await session.connect()
      // The Pi session may have ended while the handshake was in flight; a connection nobody owns would leak.
      if (this.current !== session) {
        await session.close()
        return false
      }
      return true
    } catch (error) {
      // Likewise: a failure after `stop` is about a session that no longer exists, and is not worth a notification.
      if (this.current !== session) return false
      this.report(error, notify)
      return false
    }
  }

  /** Tell the user about a failure, with the daemon's own `help`. Anything unrecognised is shown as an error. */
  report(error: unknown, notify: Notify): void {
    const { severity, message } = presentFailure(error)
    notify(`MemCastle: ${message}`, severity)
  }

  /** Idempotent, because cancellation, reload and process exit can all converge on the same shutdown. */
  async stop(): Promise<void> {
    const session = this.current
    this.current = null
    await session?.close()
  }
}
