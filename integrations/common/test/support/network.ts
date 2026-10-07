// Records every HTTP request an integration makes, and which actor made it.
//
// An `off` session must make no request at all, and "no request" can only be proved by watching the wire:
// asserting that nothing was injected would also pass for a session whose request failed. Both integrations reach the
// daemon through `fetch` (the MCP client's transport, the MCP SDK's for OpenCode and Pi's own for Pi, and the health check alike), so one wrapper sees everything.
//
// Attribution is by `AsyncLocalStorage`, because the actors run concurrently in one process and a background wake-up
// or review started by one of them must still be counted as that actor's, long after the handler that started it
// returned.

import { AsyncLocalStorage } from "node:async_hooks"

export interface Recorded {
  actor: string
  method: string
  url: string
  /** The MCP tool a `tools/call` invoked, or `null` for any other request (a health check, a handshake, a ping). */
  tool: string | null
}

const actors = new AsyncLocalStorage<string>()
const requests: Recorded[] = []
const real = globalThis.fetch

/** The MCP tool named by a JSON-RPC body, or `null` when the body is not a `tools/call`. */
function toolOf(body: unknown): string | null {
  if (typeof body !== "string") return null
  try {
    const message = JSON.parse(body) as { method?: string; params?: { name?: string } }
    return message.method === "tools/call" ? (message.params?.name ?? null) : null
  } catch {
    // Not JSON (an empty body, a stream): it is a request all the same, just not a tool call.
    return null
  }
}

/** Start recording. Idempotent, and undone by {@link stopRecording}. */
export function startRecording(): void {
  requests.length = 0
  globalThis.fetch = Object.assign(
    async (input: Parameters<typeof fetch>[0], init?: Parameters<typeof fetch>[1]) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url
      requests.push({
        actor: actors.getStore() ?? "unattributed",
        method: init?.method ?? (input instanceof Request ? input.method : "GET"),
        url,
        tool: toolOf(init?.body),
      })
      return real(input, init)
    },
    { preconnect: real.preconnect },
  ) as typeof fetch
}

export function stopRecording(): void {
  globalThis.fetch = real
}

/** Run `body` as `actor`: every request it causes, now or later, is recorded under that name. */
export function as<T>(actor: string, body: () => Promise<T>): Promise<T> {
  return actors.run(actor, body)
}

/** Everything `actor` has requested since recording started. */
export function requestsBy(actor: string): Recorded[] {
  return requests.filter((request) => request.actor === actor)
}

/** The tools `actor` has called, in order. */
export function toolsCalledBy(actor: string): string[] {
  return requestsBy(actor).flatMap((request) => (request.tool === null ? [] : [request.tool]))
}

/** Requests no actor claimed: a test bug, because everything an integration does must run inside {@link as}. */
export function unattributed(): Recorded[] {
  return requestsBy("unattributed")
}
