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

/**
 * A fake `fetch` for `GET /api/events`: every call opens a stream the test writes to (`send`) and ends (`close`), and
 * the requests made are recorded. `answer` makes the next call a refusal instead.
 */
export function fakeStream() {
  const requests: Recorded[] = []
  const encoder = new TextEncoder()
  const controllers: ReadableStreamDefaultController<Uint8Array>[] = []
  let refusal: { status: number; body: unknown } | undefined
  const impl = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    requests.push({ url: String(input), method: init?.method ?? "GET", headers: (init?.headers ?? {}) as Record<string, string>, body: undefined })
    if (refusal) {
      const { status, body } = refusal
      return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } })
    }
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controllers.push(controller)
        // Leaving on purpose ends the read, as a real aborted `fetch` does.
        init?.signal?.addEventListener("abort", () => controller.error(new DOMException("aborted", "AbortError")))
      },
    })
    return new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } })
  }
  const latest = () => controllers[controllers.length - 1]!
  return {
    fetch: impl as typeof fetch,
    requests,
    /** How many streams have been opened. */
    get opened() {
      return controllers.length
    },
    /** Write raw text to the newest stream. */
    send: (text: string) => latest().enqueue(encoder.encode(text)),
    /** The daemon's `open` frame, then nothing: what a live stream starts with. */
    open: () => latest().enqueue(encoder.encode('event: open\ndata: {}\n\n')),
    /** One change notice. */
    emit: (event: Record<string, unknown>) => latest().enqueue(encoder.encode(`event: ${String(event.kind)}\ndata: ${JSON.stringify(event)}\n\n`)),
    /** The daemon closing the newest stream. */
    close: () => latest().close(),
    /** Answer the next call with a refusal. */
    refuse: (status: number, body: unknown = {}) => void (refusal = { status, body }),
  }
}
