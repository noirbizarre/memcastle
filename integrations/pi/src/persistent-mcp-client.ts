// One MCP session with the daemon, held open for as long as the agent session lives.
//
// The connection is built on Pi's own MCP client library (`@earendil-works/pi-mcp`, the one Pi uses for its MCP servers),
// not on a second implementation of the protocol; it is bundled into the extension because Pi does not supply it.
//
// The daemon scopes a memory mode to one MCP connection: the mode lives and dies with the `mcp-session-id` issued at
// `initialize`. Two consequences shape this class.
//   1. Never reconnect per call. A fresh connection starts as `full`, so a read-only or disabled session would
//      silently gain write access.
//   2. Whenever a connection is (re)made, select the mode before anything else, and tear the connection down
//      again if that fails, rather than hand out a session in a mode nobody chose.
//   3. The daemon forgets a session after five idle minutes, after a restart, or when it is deleted, and answers
//      the next request with HTTP 404. That is a lost session, not a failed call: it is replaced (mode re-selected)
//      and the call is retried once. A periodic ping keeps a quiet session from reaching the idle limit at all.

import {
  McpClient,
  McpConnectionClosedError,
  McpError,
  McpHttpError,
  McpSessionExpiredError,
  McpTimeoutError,
  StreamableHttpTransport,
} from "@earendil-works/pi-mcp"
import type { Endpoint } from "./daemon-client.ts"
import {
  MemCastleFailure,
  failureFromBody,
  failureFromStatus,
  failureFromTransport,
  parseErrorBody,
} from "./failures.ts"
import { type ModeLabel, toModeLabel, toWireMode } from "./modes.ts"

export interface SessionOptions {
  /** Resolve the daemon on every (re)connect, because a restarted daemon may listen elsewhere. */
  endpoint: () => Promise<Endpoint>
  token: string | null
  mode: ModeLabel
  timeoutMs: number
  /** Names this client in the daemon's MCP handshake. */
  clientName: string
  /**
   * How often to ping an open connection so the daemon's idle limit (five minutes) never drops it.
   * `0` turns pinging off. Defaults to {@link DEFAULT_KEEP_ALIVE_MS}.
   */
  keepAliveMs?: number
}

/** Well inside the daemon's five-minute idle limit, so one lost ping still leaves a second chance. */
export const DEFAULT_KEEP_ALIVE_MS = 120_000

/**
 * The daemon no longer knows this session (HTTP 404). It rejects such a request before running anything, so
 * sending the same call again on a new session cannot apply it twice. Never escapes {@link McpSession.call}
 * on the first occurrence; it is only surfaced when the replacement session is lost as well.
 */
class SessionLost extends MemCastleFailure {
  constructor(endpoint: string) {
    super(
      "unexpected",
      `MemCastle at ${endpoint} no longer knows this session.`,
      null,
      "Retry the operation; a new session is opened automatically.",
    )
    this.name = "SessionLost"
  }
}

/** The text of a tool result's first content block, which is where every MemCastle tool puts its answer. */
function firstText(result: unknown): string {
  const content = (result as { content?: unknown }).content
  if (Array.isArray(content)) {
    const block = content.find((item): item is { type: "text"; text: string } => item?.type === "text")
    if (block) return block.text
  }
  return ""
}

export class McpSession {
  private client: McpClient | null = null
  private transport: StreamableHttpTransport | null = null
  private connecting: Promise<void> | null = null
  private lastEndpoint = "the MemCastle daemon"
  private connectCount = 0
  private pingCount = 0
  private keepAlive: ReturnType<typeof setInterval> | null = null
  /** Connections closed because the daemon forgot their session, as opposed to closed on purpose by `close()`. */
  private readonly lostClients = new WeakSet<McpClient>()

  constructor(private readonly options: SessionOptions) {}

  /** How many connections this session has made. More than one means it reconnected. */
  get connects(): number {
    return this.connectCount
  }

  /** How many keep-alive pings the daemon has answered. Zero means the session was never idle long enough to need one. */
  get pings(): number {
    return this.pingCount
  }

  /** The daemon's `mcp-session-id` for the current connection, or `null` when not connected. */
  get sessionId(): string | null {
    return this.transport?.sessionId ?? null
  }

  get connected(): boolean {
    return this.client !== null
  }

  /** Connect and select the mode. Idempotent, and concurrent callers share one attempt. */
  connect(): Promise<void> {
    if (this.client) return Promise.resolve()
    this.connecting ??= this.open().finally(() => {
      this.connecting = null
    })
    return this.connecting
  }

  /** Drop the connection and make a new one, re-selecting the mode before anything else uses it. */
  async reconnect(): Promise<void> {
    await this.close()
    await this.connect()
  }

  async close(): Promise<void> {
    const { client } = this
    this.client = null
    this.transport = null
    // Before the awaits below: a ping must not fire at a connection that is being torn down.
    this.stopKeepAlive()
    // Closing the client closes the transport, which asks the daemon to forget the session (best effort, bounded to
    // a second); it must not throw during shutdown.
    await client?.close().catch(() => undefined)
  }

  /**
   * Call a MemCastle tool and return its JSON answer.
   *
   * @throws MemCastleFailure for every failure; an error is never turned into an empty result.
   */
  async call<T = unknown>(tool: string, args: Record<string, unknown> = {}): Promise<T> {
    const client = await this.ready()
    try {
      return await this.invoke<T>(client, tool, args)
    } catch (error) {
      if (!(error instanceof SessionLost)) throw error
      // Replace the lost session once. A second loss is reported, because looping would hide a daemon that
      // keeps refusing its own sessions.
      await this.discard(client)
      return this.invoke<T>(await this.ready(), tool, args)
    }
  }

  /** The mode the daemon reports for this session: the only way to check what it was actually given. */
  async reportedMode(): Promise<ModeLabel> {
    const status = await this.call<{ mode: string }>("memcastle_status")
    return toModeLabel(status.mode)
  }

  /** The connected client, connecting first if needed. */
  private async ready(): Promise<McpClient> {
    await this.connect()
    // `connect` has set `client`; the check narrows the type and covers a close() racing this call.
    if (!this.client) throw failureFromTransport(this.lastEndpoint, new Error("the session was closed"))
    return this.client
  }

  /**
   * Drop `stale` if it is still the current connection. Several callers can lose the same session together, and
   * only the first may close it: a later one would otherwise close the replacement the first already opened.
   */
  private async discard(stale: McpClient): Promise<void> {
    if (this.client !== stale) return
    // Marked first: calls still in flight on `stale` fail with "connection closed" once it is torn down, and
    // that is the same loss as their own 404, not a failure of the call.
    this.lostClients.add(stale)
    await this.close()
  }

  private startKeepAlive(client: McpClient): void {
    const every = this.options.keepAliveMs ?? DEFAULT_KEEP_ALIVE_MS
    if (every <= 0) return
    this.keepAlive = setInterval(() => {
      client.ping({ timeoutMs: this.options.timeoutMs }).then(
        () => {
          this.pingCount += 1
        },
        // Pinging must never throw into the agent. A lost session or a dead daemon closes the connection, so the
        // next real call reconnects; any other failure is left for that call to report.
        (error: unknown) => {
          const lost = this.classifyLoss(error)
          if (lost) void this.discard(client)
        },
      )
    }, every)
    // A forgotten timer must not keep the agent's process alive after everything else has finished.
    this.keepAlive.unref?.()
  }

  private stopKeepAlive(): void {
    if (this.keepAlive) clearInterval(this.keepAlive)
    this.keepAlive = null
  }

  /** Whether `error` means the connection is gone (session forgotten, or nothing answering). */
  private classifyLoss(error: unknown): boolean {
    if (error instanceof McpSessionExpiredError) return true
    // The daemon answered, so it is there and the session is not known to be lost.
    if (error instanceof McpHttpError || error instanceof McpError || error instanceof McpTimeoutError) return false
    return this.classify(error).failureClass === "daemon_unavailable"
  }

  private async open(): Promise<void> {
    const endpoint = await this.options.endpoint()
    this.lastEndpoint = endpoint.baseUrl
    const headers: Record<string, string> = {}
    if (this.options.token !== null) headers.Authorization = `Bearer ${this.options.token}`
    // The daemon never pushes anything to a client, so no idle server-to-client stream is held open; the ping below is
    // what keeps the session alive.
    const transport = new StreamableHttpTransport({ url: endpoint.mcpUrl, headers, openGetStream: false })
    const client = new McpClient({ name: this.options.clientName, version: "0.0.0", requestTimeoutMs: this.options.timeoutMs })
    try {
      await client.connect(transport)
    } catch (error) {
      await client.close().catch(() => undefined)
      throw this.classify(error)
    }
    this.client = client
    this.transport = transport
    this.connectCount += 1
    this.startKeepAlive(client)
    try {
      // First thing on every new connection, before any caller can use it.
      await this.invoke(client, "memcastle_set_mode", { mode: toWireMode(this.options.mode) })
    } catch (error) {
      await this.close()
      throw error
    }
  }

  private async invoke<T>(client: McpClient, tool: string, args: Record<string, unknown>): Promise<T> {
    let result: unknown
    try {
      result = await client.callTool(tool, args, { timeoutMs: this.options.timeoutMs })
    } catch (error) {
      // The daemon has dropped the session; `call` replaces it, and a ping or a closed connection leaves it to them.
      if (error instanceof McpSessionExpiredError) throw new SessionLost(this.lastEndpoint)
      if (error instanceof McpConnectionClosedError && this.lostClients.has(client)) throw new SessionLost(this.lastEndpoint)
      const failure = this.classify(error)
      // A transport failure means the connection is gone; the next call must reconnect (and re-select the mode).
      if (failure.failureClass === "daemon_unavailable") await this.close()
      throw failure
    }
    const text = firstText(result)
    if ((result as { isError?: boolean }).isError) {
      throw failureFromBody(parseErrorBody(text) ?? { error: text || `${tool} failed`, code: null, help: null })
    }
    try {
      return JSON.parse(text) as T
    } catch {
      throw new MemCastleFailure("unexpected", `${tool} answered with something that is not JSON.`, null, text || null)
    }
  }

  private classify(error: unknown): MemCastleFailure {
    if (error instanceof MemCastleFailure) return error
    // The response body travels with the status, so the daemon's `help` reaches the user.
    if (error instanceof McpHttpError) return failureFromStatus(error.status, error.body)
    // A JSON-RPC error or a call that ran out of time means the daemon answered or is slow, not that it is gone:
    // it is not a transport failure, and the session stays open.
    if (error instanceof McpError || error instanceof McpTimeoutError) return new MemCastleFailure("unexpected", error.message)
    return failureFromTransport(this.lastEndpoint, error)
  }
}
