// The OpenCode sessions' MCP connections, one per OpenCode `sessionID`.
//
// OpenCode's own MCP client shares one connection across every session in a process, but MemCastle's memory mode
// belongs to a connection. So that two sessions can run in different modes, this plugin opens its own connection per
// session (see docs/research.md). Connections are made lazily, on first use, so a session that never touches
// MemCastle costs nothing, and a missing daemon does not slow OpenCode's startup.

import { discoverDaemon } from "./daemon.ts"
import { McpSession } from "./session.ts"
import type { Settings } from "./settings.ts"

export class SessionRegistry {
  private readonly sessions = new Map<string, McpSession>()

  constructor(
    private readonly settings: Settings,
    private readonly env: Readonly<Record<string, string | undefined>> = process.env,
  ) {}

  /** The connection for an OpenCode session, created (not yet connected) on first request. */
  session(sessionId: string): McpSession {
    let session = this.sessions.get(sessionId)
    if (!session) {
      session = new McpSession({
        endpoint: () => discoverDaemon(this.settings, this.env),
        token: this.settings.token,
        mode: this.settings.mode,
        timeoutMs: this.settings.timeoutMs,
        keepAliveMs: this.settings.keepAliveMs,
        clientName: "memcastle-opencode",
      })
      this.sessions.set(sessionId, session)
    }
    return session
  }

  get size(): number {
    return this.sessions.size
  }

  /** Forget a session and close its connection; closing an unknown session is not an error. */
  async close(sessionId: string): Promise<void> {
    const session = this.sessions.get(sessionId)
    this.sessions.delete(sessionId)
    await session?.close()
  }

  async closeAll(): Promise<void> {
    const ids = [...this.sessions.keys()]
    await Promise.all(ids.map((id) => this.close(id)))
  }
}
