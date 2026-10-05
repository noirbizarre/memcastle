// A fake `fetch` for the unit tests: it records every request and answers from a table, so what the client sent is
// something to assert on. The daemon suite (`test/daemon`) is where the real protocol is held to account.

export interface Recorded {
  url: string
  method: string
  headers: Record<string, string>
  body: unknown
}

type Answer = { status?: number; body?: unknown } | ((request: Recorded) => { status?: number; body?: unknown })

export function fakeFetch(answer: Answer): { fetch: typeof fetch; requests: Recorded[] } {
  const requests: Recorded[] = []
  const impl = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request: Recorded = {
      url: String(input),
      method: init?.method ?? "GET",
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: typeof init?.body === "string" ? JSON.parse(init.body) : undefined,
    }
    requests.push(request)
    const { status = 200, body = {} } = typeof answer === "function" ? answer(request) : answer
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } })
  }
  return { fetch: impl as typeof fetch, requests }
}

export function memoryStorage(initial: Record<string, string> = {}) {
  const items = new Map(Object.entries(initial))
  return {
    items,
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
    removeItem: (key: string) => void items.delete(key),
  }
}

export const UNAUTHORIZED = { status: 401, body: { code: "memcastle::auth::unauthorized", error: "a token is required", help: "send a bearer token" } }

export const STATUS = {
  version: "0.2.0",
  uptime_secs: 10,
  palace_name: "default",
  drawer_count: 0,
  jobs_queued: 0,
  jobs_running: 0,
  jobs_paused: 0,
  mode: "full",
  pid: 1,
  started_at: "2026-10-05T00:00:00Z",
  bind_addr: "127.0.0.1:8420",
  palace_path: "/p",
  datastore: { ok: true, backend: "embedded", location: "/p/db", error: null, migration_version: 3, latest_version: 3, pending: [] },
  auth_enabled: true,
}
