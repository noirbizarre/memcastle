// One MCP session with the daemon, held open for as long as the agent session lives.
//
// The daemon scopes a memory mode to one MCP connection: the mode lives and dies with the `mcp-session-id` issued at
// `initialize`. Two consequences shape this class.
//   1. Never reconnect per call. A fresh connection starts as `full`, so a read-only or disabled session would
//      silently gain write access.
//   2. Whenever a connection is (re)made, select the mode before anything else, and tear the connection down
//      again if that fails, rather than hand out a session in a mode nobody chose.

import { Client } from "@modelcontextprotocol/sdk/client/index.js"
import { StreamableHTTPClientTransport, StreamableHTTPError } from "@modelcontextprotocol/sdk/client/streamableHttp.js"
import { McpError } from "@modelcontextprotocol/sdk/types.js"
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
  private client: Client | null = null
  private transport: StreamableHTTPClientTransport | null = null
  private connecting: Promise<void> | null = null
  private lastEndpoint = "the MemCastle daemon"
  private connectCount = 0

  constructor(private readonly options: SessionOptions) {}

  /** How many connections this session has made. More than one means it reconnected. */
  get connects(): number {
    return this.connectCount
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
    const { client, transport } = this
    this.client = null
    this.transport = null
    // Ask the daemon to forget the session (best effort), then close; neither may throw during shutdown.
    await transport?.terminateSession().catch(() => undefined)
    await client?.close().catch(() => undefined)
  }

  /**
   * Call a MemCastle tool and return its JSON answer.
   *
   * @throws MemCastleFailure for every failure; an error is never turned into an empty result.
   */
  async call<T = unknown>(tool: string, args: Record<string, unknown> = {}): Promise<T> {
    await this.connect()
    // `connect` has set `client`; the check narrows the type and covers a close() racing this call.
    if (!this.client) throw failureFromTransport(this.lastEndpoint, new Error("the session was closed"))
    return this.invoke<T>(this.client, tool, args)
  }

  /** The mode the daemon reports for this session: the only way to check what it was actually given. */
  async reportedMode(): Promise<ModeLabel> {
    const status = await this.call<{ mode: string }>("memcastle_status")
    return toModeLabel(status.mode)
  }

  private async open(): Promise<void> {
    const endpoint = await this.options.endpoint()
    this.lastEndpoint = endpoint.baseUrl
    const headers: Record<string, string> = {}
    if (this.options.token !== null) headers.Authorization = `Bearer ${this.options.token}`
    const transport = new StreamableHTTPClientTransport(new URL(endpoint.mcpUrl), { requestInit: { headers } })
    const client = new Client({ name: this.options.clientName, version: "0.0.0" })
    try {
      await client.connect(transport)
    } catch (error) {
      await client.close().catch(() => undefined)
      throw this.classify(error)
    }
    this.client = client
    this.transport = transport
    this.connectCount += 1
    try {
      // First thing on every new connection, before any caller can use it.
      await this.invoke(client, "memcastle_set_mode", { mode: toWireMode(this.options.mode) })
    } catch (error) {
      await this.close()
      throw error
    }
  }

  private async invoke<T>(client: Client, tool: string, args: Record<string, unknown>): Promise<T> {
    let result: unknown
    try {
      result = await client.callTool({ name: tool, arguments: args }, undefined, { timeout: this.options.timeoutMs })
    } catch (error) {
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
    if (error instanceof StreamableHTTPError && typeof error.code === "number") {
      // The SDK embeds the response body in its message; recover it so `help` reaches the user.
      const body = error.message.slice(error.message.indexOf("{") >= 0 ? error.message.indexOf("{") : 0)
      return failureFromStatus(error.code, body)
    }
    // A JSON-RPC error means the daemon answered, so it is reachable and it is not a transport failure.
    if (error instanceof McpError) return new MemCastleFailure("unexpected", error.message)
    return failureFromTransport(this.lastEndpoint, error)
  }
}
