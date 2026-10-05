// The one place the dashboard talks to the daemon: same-origin `fetch` against `/api`.
//
// Every request carries the session's bearer token and memory mode, and every failure becomes an `ApiError` with the
// daemon's own diagnostic (`code`, `error`, `help`), so a view never has to parse a response by hand. A 401 on a
// request that carried a token means the token was revoked or rotated: the session is told, and the login page comes
// back (docs/adr/035). Nothing else is imported from the daemon: this is HTTP, as the CLI's client is.

import type {
  ConfigReport,
  Drawer,
  DrawerHistory,
  DrawerSummary,
  Entity,
  ErrorBody,
  GraphView,
  Job,
  JobControl,
  JobRequest,
  JobStatus,
  MemoryMode,
  Mention,
  Relationship,
  SearchHit,
  SearchOptions,
  SimilarDrawer,
  SourcesReport,
  StatusReport,
  WingDetail,
  Wing,
} from "./types.ts"

export class ApiError extends Error {
  constructor(
    readonly status: number,
    /** `memcastle::<module>::<kind>`, or `network` when no answer came at all. */
    readonly code: string,
    message: string,
    readonly help?: string,
  ) {
    super(message)
    this.name = "ApiError"
  }

  get unauthorized(): boolean {
    return this.status === 401
  }
}

export interface ClientOptions {
  /** Where `/api` lives; empty means the page's own origin, which is how the daemon serves it. */
  baseUrl?: string
  fetch?: typeof fetch
  token: () => string | null
  mode: () => MemoryMode
  /** Called when a request that carried a token is refused: the token is no longer good. */
  onUnauthorized?: () => void
}

type Query = Record<string, string | number | boolean | undefined | null>

function queryString(query: Query | undefined): string {
  const params = new URLSearchParams()
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value !== undefined && value !== null && value !== "") params.set(key, String(value))
  }
  const text = params.toString()
  return text ? `?${text}` : ""
}

/** A drawer's name may contain `/`, which the wildcard route takes, so its segments are escaped one by one. */
function drawerPath(name: string): string {
  return name.split("/").map(encodeURIComponent).join("/")
}

export class MemCastleClient {
  constructor(private readonly options: ClientOptions) {}

  private async request<T>(method: string, path: string, init: { query?: Query; body?: unknown; token?: string | null } = {}): Promise<T> {
    const token = init.token === undefined ? this.options.token() : init.token
    const headers: Record<string, string> = { Accept: "application/json", "X-MemCastle-Mode": this.options.mode() }
    if (token) headers.Authorization = `Bearer ${token}`
    if (init.body !== undefined) headers["Content-Type"] = "application/json"

    let response: Response
    try {
      response = await (this.options.fetch ?? fetch)(`${this.options.baseUrl ?? ""}${path}${queryString(init.query)}`, {
        method,
        headers,
        body: init.body === undefined ? undefined : JSON.stringify(init.body),
      })
    } catch (cause) {
      throw new ApiError(0, "network", `The daemon did not answer: ${cause instanceof Error ? cause.message : String(cause)}`, "Is it running? Try `memcastle status`.")
    }

    if (!response.ok) {
      const body = (await response.json().catch(() => ({}))) as ErrorBody
      // Only a refused *token* ends the session: a 401 with no token is just the login probe being answered.
      if (response.status === 401 && token) this.options.onUnauthorized?.()
      throw new ApiError(response.status, body.code ?? `http_${response.status}`, body.error ?? response.statusText, body.help)
    }
    return (response.status === 204 ? undefined : await response.json()) as T
  }

  /** Whether `token` is accepted, by asking for the one thing every session needs. Used by the login form. */
  status(token?: string | null): Promise<StatusReport> {
    return this.request("GET", "/api/status", { token })
  }
  config(): Promise<ConfigReport> {
    return this.request("GET", "/api/config")
  }

  jobs(filter: { status?: JobStatus; kind?: string; limit?: number } = {}): Promise<Job[]> {
    return this.request("GET", "/api/jobs", { query: filter })
  }
  job(id: string): Promise<Job> {
    return this.request("GET", `/api/jobs/${encodeURIComponent(id)}`)
  }
  submitJob(request: JobRequest): Promise<Job> {
    return this.request("POST", "/api/jobs", { body: { ...request, requested_by: "web" } })
  }
  controlJob(id: string, action: JobControl): Promise<{ status: string }> {
    return this.request("POST", `/api/jobs/${encodeURIComponent(id)}/${action}`)
  }

  sources(): Promise<SourcesReport> {
    return this.request("GET", "/api/sources")
  }

  wings(): Promise<Wing[]> {
    return this.request("GET", "/api/wings")
  }
  wing(name: string): Promise<WingDetail> {
    return this.request("GET", `/api/wings/${encodeURIComponent(name)}`)
  }
  drawers(wing: string, room: string, limit?: number): Promise<DrawerSummary[]> {
    return this.request("GET", `/api/wings/${encodeURIComponent(wing)}/rooms/${encodeURIComponent(room)}/drawers`, { query: { limit } })
  }
  drawer(wing: string, room: string, drawer: string): Promise<Drawer> {
    return this.request("GET", `/api/wings/${encodeURIComponent(wing)}/rooms/${encodeURIComponent(room)}/drawers/${drawerPath(drawer)}`)
  }
  drawerHistory(id: string): Promise<DrawerHistory> {
    return this.request("GET", `/api/drawers/${encodeURIComponent(id)}/history`)
  }
  drawerDuplicates(id: string): Promise<SimilarDrawer[]> {
    return this.request("GET", `/api/drawers/${encodeURIComponent(id)}/duplicates`)
  }

  search(options: SearchOptions): Promise<SearchHit[]> {
    return this.request("GET", "/api/search", { query: { ...options } })
  }

  diary(agentIdentity: string, wing: string, limit?: number): Promise<Drawer[]> {
    return this.request("GET", "/api/diary", { query: { agent_identity: agentIdentity, wing, limit } })
  }
  writeDiary(agentIdentity: string, wing: string, content: string): Promise<Drawer> {
    return this.request("POST", "/api/diary", { body: { agent_identity: agentIdentity, wing, content, requested_by: "web" } })
  }
  writeNote(wing: string, room: string, content: string): Promise<unknown> {
    return this.request("POST", "/api/notes", { body: { wing, room, content, requested_by: "web" } })
  }

  entities(filter: { name?: string; kind?: string; limit?: number } = {}): Promise<Entity[]> {
    return this.request("GET", "/api/entities", { query: filter })
  }
  relationships(id: string, includeExpired = false): Promise<Relationship[]> {
    return this.request("GET", `/api/entities/${encodeURIComponent(id)}/relationships`, { query: { include_expired: includeExpired || undefined } })
  }
  mentions(id: string): Promise<Mention[]> {
    return this.request("GET", `/api/entities/${encodeURIComponent(id)}/mentions`)
  }
  graph(options: { entity?: string; depth?: number; limit?: number } = {}): Promise<GraphView> {
    return this.request("GET", "/api/graph", { query: options })
  }
}
