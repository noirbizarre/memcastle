import { flushPromises, mount } from "@vue/test-utils"
import Aura from "@openvue/themes/aura"
import OpenVue from "openvue/config"
import ToastService from "openvue/toastservice"
import { describe, expect, it, vi } from "vitest"
import type { Component } from "vue"
import { createAppRouter } from "../src/router.ts"
import { createSession, SESSION } from "../src/session.ts"
import JobsView from "../src/views/JobsView.vue"
import LaunchView from "../src/views/LaunchView.vue"
import AppShell from "../src/layout/AppShell.vue"
import GraphView from "../src/views/GraphView.vue"
import OverviewView from "../src/views/OverviewView.vue"
import PalaceView from "../src/views/PalaceView.vue"
import DiaryView from "../src/views/DiaryView.vue"
import MaintenanceView from "../src/views/MaintenanceView.vue"
import TriggersView from "../src/views/TriggersView.vue"
import SearchView from "../src/views/SearchView.vue"
import SettingsView from "../src/views/SettingsView.vue"
import { fakeFetch, memoryStorage, STATUS, type Recorded } from "./support/fetch.ts"

// The canvas needs a real browser: a stand-in records the elements drawn and lets a test tap a node.
const graph = vi.hoisted(() => ({ elements: [] as { data: { id: string } }[], taps: [] as ((event: { target: { id: () => string } }) => void)[], destroyed: 0 }))
vi.mock("cytoscape", () => ({
  default: (options: { elements: { data: { id: string } }[] }) => {
    graph.elements = options.elements
    return {
      on: (_event: string, _selector: string, handler: (event: { target: { id: () => string } }) => void) => graph.taps.push(handler),
      destroy: () => void graph.destroyed++,
    }
  },
}))

const CONFIG = {
  bind_addr: "127.0.0.1:8420",
  palace_path: "/palace",
  backend: "embedded",
  location: "/palace/db",
  auth_enabled: false,
  web: { enabled: true, built: true },
  assets: { source: "installed", root: "/usr/share/memcastle" },
  jobs: { max_concurrency: 2, drain_timeout_secs: 30, lease_ttl_secs: 60 },
  embeddings: { provider: "none", model: null },
  extraction: { provider: "heuristic", model: null },
  mining: { chunk_chars: 1200, max_documents: 500, registries: 1, dedup_enabled: true },
}

const HIT = {
  id: "drawer:abcdef0123456789",
  content: "The castle keeps its memory in one database.",
  score: 0.8123,
  source: { kind: "note", uri: "note://castle" },
  valid_from: "2026-10-01T00:00:00Z",
  valid_to: null,
  signals: { lexical: 0.5 },
  via: ["entity:keep"],
}

const JOB = {
  id: "job:0123456789abcdef",
  kind: { type: "audit" },
  status: "completed",
  priority: 0,
  created_at: "2026-10-05T00:00:00Z",
  started_at: "2026-10-05T00:00:01Z",
  completed_at: "2026-10-05T00:00:02Z",
  requested_by: "web",
  progress: { current: 1, total: 1, message: null },
  attempt: 1,
}

type Answer = { status?: number; body?: unknown }

/** Mount a view signed in, with every request answered by `answer` (by method and path). */
async function show(view: Component, answer: (request: Recorded) => Answer, mode: "full" | "read_only" = "full") {
  // Live, and without the session's own status probe, which is not what a page asked for.
  const seen: Recorded[] = []
  const { fetch } = fakeFetch((request) => {
    if (!request.url.endsWith("/api/status")) seen.push(request)
    return answer(request)
  })
  const session = createSession({ fetch, storage: memoryStorage() })
  session.state.mode = mode
  const router = createAppRouter(session)
  const wrapper = mount(view, {
    global: { plugins: [router, ToastService, [OpenVue, { theme: { preset: Aura } }]], provide: { [SESSION as symbol]: session } },
  })
  await flushPromises()
  // The session's own status probe is not what a page asked for.
  return { wrapper, requests: seen }
}

const path = (request: Recorded) => new URL(request.url, "http://daemon").pathname

describe("the search page", () => {
  it("sends the query with the chosen filters and shows each hit with how it ranked", async () => {
    const { wrapper, requests } = await show(SearchView, () => ({ body: [HIT] }))

    await wrapper.find("input#q").setValue("  castle  ")
    await wrapper.find("input#wing").setValue("keep")
    await wrapper.find("form").trigger("submit")
    await flushPromises()

    const search = new URL(requests[0]!.url, "http://daemon")
    expect(search.searchParams.get("q")).toBe("castle")
    expect(search.searchParams.get("wing")).toBe("keep")
    expect(wrapper.text()).toContain("0.812")
    expect(wrapper.text()).toContain("lexical 0.50")
    expect(wrapper.text()).toContain("via entity:keep")
  })

  it("does not ask the daemon for an empty query", async () => {
    const { wrapper, requests } = await show(SearchView, () => ({ body: [] }))

    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(requests).toHaveLength(0)
  })

  it("says so when nothing matched", async () => {
    const { wrapper } = await show(SearchView, () => ({ body: [] }))

    await wrapper.find("input#q").setValue("nothing")
    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(wrapper.text()).toContain("Nothing matched")
  })

  it("shows the daemon's refusal instead of stale hits", async () => {
    const { wrapper } = await show(SearchView, () => ({ status: 500, body: { code: "memcastle::search::failed", error: "the index is down", help: "restart it" } }))

    await wrapper.find("input#q").setValue("castle")
    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(wrapper.text()).toContain("the index is down")
  })
})

describe("the diary page", () => {
  const entries = [{ id: "drawer:1", content: "Shipped the dashboard.", created_at: "2026-10-05T10:00:00Z" }]

  async function diary(mode: "full" | "read_only" = "full") {
    const shown = await show(DiaryView, (request) => (request.method === "POST" ? { body: entries[0] } : { body: entries }), mode)
    await shown.wrapper.find("input#agent").setValue("pi")
    await shown.wrapper.find("input#wing").setValue("castle")
    return shown
  }

  it("reads the diary of an agent in a wing", async () => {
    const { wrapper, requests } = await diary()

    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(requests[0]!.url).toContain("agent_identity=pi")
    expect(wrapper.text()).toContain("Shipped the dashboard.")
  })

  it("says so when the diary is empty", async () => {
    const { wrapper } = await show(DiaryView, () => ({ body: [] }))
    await wrapper.find("input#agent").setValue("pi")
    await wrapper.find("input#wing").setValue("castle")

    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(wrapper.text()).toContain("No entries yet")
  })

  it("writes an entry as the agent and reads the diary again", async () => {
    const { wrapper, requests } = await diary()

    await wrapper.find("textarea#entry").setValue("A new entry")
    await wrapper.findAll("button").find((button) => button.text() === "Write entry")!.trigger("click")
    await flushPromises()

    const post = requests.find((request) => request.method === "POST")!
    expect(post.body).toMatchObject({ agent_identity: "pi", wing: "castle", content: "A new entry" })
    expect(requests.at(-1)!.method).toBe("GET")
  })

  it("does not let a read only session write", async () => {
    const { wrapper } = await diary("read_only")

    expect(wrapper.text()).toContain("The session is read only.")
    const write = wrapper.findAll("button").find((button) => button.text() === "Write entry")!
    expect(write.attributes("disabled")).toBeDefined()
  })

  it("shows the failure of a read", async () => {
    const { wrapper } = await show(DiaryView, () => ({ status: 404, body: { code: "memcastle::wing::missing", error: "no such wing", help: "create it" } }))
    await wrapper.find("input#agent").setValue("pi")
    await wrapper.find("input#wing").setValue("castle")

    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(wrapper.text()).toContain("no such wing")
  })
})

describe("the settings page", () => {
  it("shows the daemon's configuration and the session's mode", async () => {
    const { wrapper } = await show(SettingsView, () => ({ body: CONFIG }))

    expect(wrapper.text()).toContain("/palace/db")
    expect(wrapper.text()).toContain("Job concurrency")
    expect(wrapper.text()).toContain("Authentication is off")
    expect(wrapper.text()).toContain("full access")
  })

  it("does not warn about authentication when a token is required", async () => {
    const { wrapper } = await show(SettingsView, () => ({ body: { ...CONFIG, auth_enabled: true } }))

    expect(wrapper.text()).not.toContain("Authentication is off")
    expect(wrapper.text()).toContain("token required")
  })

  it("reads the configuration again on refresh", async () => {
    const { wrapper, requests } = await show(SettingsView, () => ({ body: CONFIG }))

    await wrapper.findAll("button").find((button) => button.text().includes("Refresh"))!.trigger("click")
    await flushPromises()

    expect(requests.filter((request) => path(request) === "/api/config")).toHaveLength(2)
  })
})

describe("the triggers page", () => {
  const TRIGGER = {
    name: "nightly",
    miner: "docs",
    type: "schedule",
    enabled: true,
    status: "failing",
    reason: "no route to the source",
    running: true,
    settings: { every: "1d", at: "03:30" },
    last_fired_at: "2026-10-05T03:30:00Z",
    fired: 4,
    coalesced: 1,
    duplicates: 0,
    next_due: "2026-10-06T03:30:00Z",
    last_error: "no route to the source",
    consecutive_failures: 2,
  }
  const report = (triggers: unknown[], webhook = { enabled: false, bind: "127.0.0.1", port: 8787, allow_remote: false }) => ({ body: { triggers, webhook } })

  it("says plainly that nothing runs unattended when no trigger is configured", async () => {
    const { wrapper } = await show(TriggersView, () => report([]))

    expect(wrapper.text()).toContain("nothing runs unattended")
    expect(wrapper.text()).toContain("Off")
  })

  it("shows each trigger with where it stands, when it fires next and what last went wrong", async () => {
    const { wrapper, requests } = await show(TriggersView, () => report([TRIGGER]))

    expect(requests.map(path)).toEqual(["/api/triggers"])
    expect(wrapper.text()).toContain("nightly")
    expect(wrapper.text()).toContain("failing")
    expect(wrapper.text()).toContain("every=1d at=03:30")
    expect(wrapper.text()).toContain("no route to the source")
    expect(wrapper.text()).toContain("4 runs")
    expect(wrapper.text()).toContain("1 joined")
  })

  it("names where the webhook listener is, and offers no way to change a trigger", async () => {
    const { wrapper, requests } = await show(TriggersView, () =>
      report([], { enabled: true, bind: "127.0.0.1", port: 8787, allow_remote: false, listening: "127.0.0.1:8787" } as never),
    )

    expect(wrapper.text()).toContain("127.0.0.1:8787")
    expect(wrapper.findAll("button").filter((button) => /enable|disable|fire|remove/i.test(button.text()))).toHaveLength(0)
    expect(requests.every((request) => request.method === "GET")).toBe(true)
  })
})

describe("the maintenance page", () => {
  const answer = (request: Recorded): Answer => (request.method === "POST" ? { body: JOB } : { body: [JOB] })

  it("shows the newest audit and repair", async () => {
    const { wrapper, requests } = await show(MaintenanceView, answer)

    expect(wrapper.text()).toContain("Audit")
    expect(wrapper.text()).toContain("Repair")
    expect(requests.map((request) => new URL(request.url, "http://daemon").searchParams.get("kind"))).toEqual(expect.arrayContaining(["audit", "repair"]))
  })

  it("submits an audit without asking", async () => {
    const confirm = vi.fn(() => true)
    window.confirm = confirm
    const { wrapper, requests } = await show(MaintenanceView, answer)

    await wrapper.findAll("button").find((button) => button.text() === "Run audit")!.trigger("click")
    await flushPromises()

    expect(requests.some((request) => request.method === "POST" && (request.body as { type: string }).type === "audit")).toBe(true)
    expect(confirm).not.toHaveBeenCalled()
  })

  it("does not remove anything when the user declines the repair", async () => {
    window.confirm = vi.fn(() => false)
    const { wrapper, requests } = await show(MaintenanceView, answer)

    await wrapper.findAll("button").find((button) => button.text() === "Run")!.trigger("click")
    await flushPromises()

    expect(requests.some((request) => request.method === "POST" && (request.body as { dry_run?: boolean }).dry_run === false)).toBe(false)
  })
})

describe("the jobs page", () => {
  const job = (id: string, status: string, extra: Record<string, unknown> = {}) => ({ ...JOB, id: `job:${id}0000000000`, status, ...extra })
  const jobs = [
    job("run", "running", { progress: { current: 1, total: 4, message: null }, completed_at: null }),
    job("que", "queued"),
    job("pau", "paused"),
    job("bad", "failed", { error: "the source vanished" }),
    job("ok", "completed", { result: { drawers: 3 } }),
  ]
  const answer = (request: Recorded): Answer => (request.method === "POST" ? { body: { status: "ok" } } : { body: jobs })
  const button = (wrapper: Awaited<ReturnType<typeof show>>["wrapper"], label: string) => wrapper.findAll("button").filter((candidate) => candidate.text() === label)

  it("splits the jobs into active and finished ones", async () => {
    const { wrapper } = await show(JobsView, answer)

    expect(wrapper.text()).toContain("Active 3")
    expect(wrapper.text()).toContain("History 2")
  })

  it("offers the transitions the daemon would accept for each state", async () => {
    const { wrapper } = await show(JobsView, answer)

    expect(button(wrapper, "pause")).toHaveLength(1)
    expect(button(wrapper, "resume")).toHaveLength(1)
    expect(button(wrapper, "retry")).toHaveLength(1)
    // Running, queued and paused jobs can each be cancelled; a finished one cannot.
    expect(button(wrapper, "cancel")).toHaveLength(3)
  })

  it("asks the daemon to pause a running job and reads the list again", async () => {
    const { wrapper, requests } = await show(JobsView, answer)

    await button(wrapper, "pause")[0]!.trigger("click")
    await flushPromises()

    expect(requests.some((request) => request.method === "POST" && path(request) === "/api/jobs/job%3Arun0000000000/pause")).toBe(true)
    expect(requests.at(-1)!.method).toBe("GET")
  })

  it("shows the failure when the daemon refuses a transition", async () => {
    const { wrapper, requests } = await show(JobsView, (request) => (request.method === "POST" ? { status: 409, body: { code: "memcastle::job::conflict", error: "not now", help: "wait" } } : { body: jobs }))

    await button(wrapper, "retry")[0]!.trigger("click")
    await flushPromises()

    expect(requests.filter((request) => request.method === "POST")).toHaveLength(1)
  })

  it("reads only the chosen state from the daemon", async () => {
    const { wrapper, requests } = await show(JobsView, answer)

    await wrapper.findAll("button").find((candidate) => candidate.text() === "failed")!.trigger("click")
    await flushPromises()

    expect(requests.at(-1)!.url).toContain("status=failed")
  })

  it("opens the details of a job with its parameters and result", async () => {
    const { wrapper } = await show(JobsView, answer)

    await button(wrapper, "Details").at(-1)!.trigger("click")
    await flushPromises()

    expect(document.body.textContent).toContain("Parameters")
    expect(document.body.textContent).toContain('"drawers": 3')
  })
})

describe("the launch page", () => {
  const sources = { adapters: [{ name: "pi", state: "enabled", options: [{ name: "since", description: "a date" }] }, { name: "off", state: "disabled", options: [] }], sources: [] }
  const answer = (request: Recorded): Answer => (request.method === "POST" ? { body: JOB } : { body: sources })
  const submitted = (requests: Recorded[]) => requests.filter((request) => request.method === "POST").map((request) => request.body as Record<string, unknown>)

  it("mines a directory by absolute path", async () => {
    const { wrapper, requests } = await show(LaunchView, answer)

    await wrapper.find("input#mine-path").setValue("/work/project")
    await wrapper.findAll("button").find((candidate) => candidate.text() === "Submit mine job")!.trigger("click")
    await flushPromises()

    expect(submitted(requests)[0]).toMatchObject({ type: "mine" })
  })

  it("will not mine a relative path", async () => {
    const { wrapper } = await show(LaunchView, answer)

    await wrapper.find("input#mine-path").setValue("relative/path")

    expect(wrapper.findAll("button").find((candidate) => candidate.text() === "Submit mine job")!.attributes("disabled")).toBeDefined()
  })

  it("offers only the sources that are enabled", async () => {
    const { wrapper } = await show(LaunchView, answer)

    await wrapper.findAll("button").find((candidate) => candidate.text() === "Installed source")!.trigger("click")

    expect(wrapper.find("input#mine-locator").exists()).toBe(true)
    expect(wrapper.text()).not.toContain("off")
  })

  it("submits an extract job and an embed job for a wing", async () => {
    const { wrapper, requests } = await show(LaunchView, answer)

    await wrapper.find("input#extract-wing").setValue("castle")
    await wrapper.findAll("button").find((candidate) => candidate.text() === "Submit extract job")!.trigger("click")
    await flushPromises()
    await wrapper.findAll("button").find((candidate) => candidate.text() === "Submit embed job")!.trigger("click")
    await flushPromises()

    expect(submitted(requests).map((body) => [body.type, body.wing])).toEqual([["extract", "castle"], ["embed", "castle"]])
  })

  it("does not let a read only session submit anything", async () => {
    const { wrapper } = await show(LaunchView, answer, "read_only")

    expect(wrapper.text()).toContain("The session is read only")
    expect(wrapper.findAll("button").find((candidate) => candidate.text() === "Submit extract job")!.attributes("disabled")).toBeDefined()
  })

  it("keeps working when the list of sources cannot be read", async () => {
    const { wrapper } = await show(LaunchView, () => ({ status: 500, body: { error: "down" } }))

    expect(wrapper.find("input#mine-path").exists()).toBe(true)
    expect(wrapper.findAll("button").find((candidate) => candidate.text() === "Installed source")!.attributes("disabled")).toBeDefined()
  })
})

describe("the overview page", () => {
  const status = { ...STATUS, drawer_count: 12, jobs_queued: 2 }
  const answer = (request: Recorded): Answer => ((request.url.includes("/api/status") ? { body: status } : { body: [JOB, { ...JOB, status: "failed" }, { ...JOB, status: "cancelled" }] }))

  it("shows the palace, its counts and how the recent jobs ended", async () => {
    const { wrapper } = await show(OverviewView, answer)

    expect(wrapper.text()).toContain("running")
    expect(wrapper.text()).toContain("12")
    expect(wrapper.text()).toContain("token required")
    expect(wrapper.text()).toContain("newest 3")
  })

  it("says the palace is degraded when migrations are pending, and shows why the datastore is down", async () => {
    const down = { ...status, datastore: { ...status.datastore, ok: false, error: "disk full", pending: ["003"] } }
    const { wrapper } = await show(OverviewView, (request) => (request.url.includes("/api/status") ? { body: down } : { body: [] }))

    expect(wrapper.text()).toContain("degraded")
    expect(wrapper.text()).toContain("disk full")
    expect(wrapper.text()).toContain("1 pending")
  })

  it("shows the daemon's refusal", async () => {
    const { wrapper } = await show(OverviewView, () => ({ status: 500, body: { code: "memcastle::x", error: "no palace", help: "start it" } }))

    expect(wrapper.text()).toContain("no palace")
  })
})

describe("the palace page", () => {
  const drawer = { id: "drawer:aaaa", name: "decision", content: "Use SurrealDB.", source: { kind: "note", uri: null, agent: "pi" }, tags: ["db"], provenance: { requested_by: "pi", job_id: null }, valid_from: "2026-10-01T00:00:00Z", valid_to: null, created_at: "2026-10-01T00:00:00Z", updated_at: "2026-10-01T00:00:00Z" }
  const summary = { id: drawer.id, name: "decision", chars: 14, preview: "Use SurrealDB.", source: drawer.source, created_at: drawer.created_at }
  const room = { id: "room:1", name: "design", wing_name: "castle", description: null, created_at: drawer.created_at, drawers: 1 }
  const wing = { id: "wing:1", name: "castle", description: null, created_at: drawer.created_at, rooms: 1, drawers: 1 }

  const answer = (request: Recorded): Answer => {
    const route = path(request)
    if (route.endsWith("/history")) return { body: { drawer: drawer.id, versions: [drawer, { ...drawer, id: "drawer:bbbb", valid_to: "2026-10-02T00:00:00Z" }] } }
    if (route.endsWith("/duplicates")) return { body: [{ drawer: "drawer:cccc", side: "older", kind: "near", similarity: 0.91 }] }
    if (route.endsWith("/drawers")) return { body: [summary] }
    if (route.includes("/drawers/")) return { body: drawer }
    if (route === "/api/wings/castle") return { body: { wing, rooms: [room] } }
    return { body: [wing] }
  }
  const click = async (wrapper: Awaited<ReturnType<typeof show>>["wrapper"], text: string) => {
    await wrapper.findAll("button").find((candidate) => candidate.text().startsWith(text))!.trigger("click")
    await flushPromises()
  }

  it("lists the wings, then the rooms of the chosen wing, then its drawers", async () => {
    const { wrapper } = await show(PalaceView, answer)

    expect(wrapper.text()).toContain("Choose a room")
    await click(wrapper, "castle")
    await click(wrapper, "design")

    expect(wrapper.text()).toContain("Use SurrealDB.")
    expect(wrapper.text()).toContain("14 characters")
  })

  it("opens a drawer with its history and what it resembles", async () => {
    const { wrapper } = await show(PalaceView, answer)
    await click(wrapper, "castle")
    await click(wrapper, "design")

    await click(wrapper, "Open")

    expect(document.body.textContent).toContain("this version")
    expect(document.body.textContent).toContain("91% similar")
  })

  it("still shows the drawer when its history cannot be read", async () => {
    const { wrapper } = await show(PalaceView, (request) => (path(request).endsWith("/history") || path(request).endsWith("/duplicates") ? { status: 500, body: { error: "no" } } : answer(request)))
    await click(wrapper, "castle")
    await click(wrapper, "design")

    await click(wrapper, "Open")

    expect(document.body.textContent).toContain("Use SurrealDB.")
  })

  it("says so when there are no wings, and when a room is empty", async () => {
    const empty = await show(PalaceView, () => ({ body: [] }))
    expect(empty.wrapper.text()).toContain("No wings yet")

    const { wrapper } = await show(PalaceView, (request) => (path(request).endsWith("/drawers") ? { body: [] } : answer(request)))
    await click(wrapper, "castle")
    await click(wrapper, "design")
    expect(wrapper.text()).toContain("This room is empty")
  })

  it("offers more drawers when the page is full", async () => {
    const many = Array.from({ length: 50 }, (_, index) => ({ ...summary, id: `drawer:${index}`, name: `n${index}` }))
    const { wrapper, requests } = await show(PalaceView, (request) => (path(request).endsWith("/drawers") ? { body: many } : answer(request)))
    await click(wrapper, "castle")
    await click(wrapper, "design")

    await click(wrapper, "Load more")

    expect(requests.at(-1)!.url).toContain("limit=100")
  })

  it("shows the failure of a drawer that cannot be opened", async () => {
    const { wrapper } = await show(PalaceView, (request) => (path(request).includes("/drawers/") ? { status: 404, body: { code: "memcastle::drawer::missing", error: "no such drawer", help: "list again" } } : answer(request)))
    await click(wrapper, "castle")
    await click(wrapper, "design")

    await click(wrapper, "Open")

    expect(wrapper.text()).toContain("no such drawer")
  })
})

describe("the graph page", () => {
  const nodes = [
    { id: "entity:a", name: "Castle", kind: "project", aliases: ["keep"] },
    { id: "entity:b", name: "SurrealDB", kind: "tool", aliases: [] },
  ]
  const edge = { id: "rel:1", from: "entity:a", to: "entity:b", predicate: "uses", confidence: 1, valid_from: "2026-10-01T00:00:00Z", valid_to: null, provenance: { drawer: "drawer:1", extractor: "heuristic", extracted_at: "2026-10-01T00:00:00Z" } }
  const answer = (request: Recorded): Answer => {
    const route = path(request)
    if (route === "/api/graph") return { body: { nodes, edges: [edge], truncated: false } }
    if (route.endsWith("/mentions")) return { body: [{ drawer: "drawer:1", created_at: "2026-10-01T00:00:00Z" }] }
    return { body: nodes }
  }
  const mountGraph = async (reply = answer) => {
    graph.taps.length = 0
    const shown = await show(GraphView, reply)
    return shown
  }

  it("draws the entities and the facts between them", async () => {
    await mountGraph()

    expect(graph.elements.map((element) => element.data.id)).toEqual(["entity:a", "entity:b", "rel:1"])
  })

  it("says so when the graph is empty, and when it was cut", async () => {
    const empty = await mountGraph(() => ({ body: { nodes: [], edges: [], truncated: false } }))
    expect(empty.wrapper.text()).toContain("The knowledge graph is empty")

    const cut = await mountGraph(() => ({ body: { nodes, edges: [], truncated: true } }))
    expect(cut.wrapper.text()).toContain("The graph was cut at 2 entities")
  })

  it("shows the facts and the drawers of a tapped entity", async () => {
    const { wrapper } = await mountGraph()

    graph.taps.at(-1)!({ target: { id: () => "entity:a" } })
    await flushPromises()

    expect(wrapper.text()).toContain("also keep")
    expect(wrapper.text()).toContain("read by heuristic")
    expect(wrapper.text()).toContain("drawer:1".slice(0, 8))
  })

  it("ignores a tap on something that is not in the view", async () => {
    const { wrapper } = await mountGraph()

    graph.taps.at(-1)!({ target: { id: () => "entity:gone" } })
    await flushPromises()

    expect(wrapper.text()).toContain("Select an entity")
  })

  it("finds an entity by name and centres the graph on it", async () => {
    const { wrapper, requests } = await mountGraph()

    await wrapper.find("input").setValue("castle")
    await wrapper.find("form").trigger("submit")
    await flushPromises()
    await wrapper.findAll("button.list-item")[0]!.trigger("click")
    await flushPromises()

    expect(requests.some((request) => request.url.includes("/api/entities") && request.url.includes("name=castle"))).toBe(true)
    expect(requests.at(-1)!.url).toContain("entity=entity%3Aa")
    expect(wrapper.text()).toContain("Show overview")
  })

  it("shows the failure of a search", async () => {
    const { wrapper } = await mountGraph((request) => (path(request) === "/api/entities" ? { status: 500, body: { code: "memcastle::graph::failed", error: "graph is down", help: "retry" } } : answer(request)))

    await wrapper.find("form").trigger("submit")
    await flushPromises()

    expect(wrapper.text()).toContain("graph is down")
  })
})

describe("the application shell", () => {
  async function shell(authRequired = true) {
    const { fetch } = fakeFetch((request) => ({ body: request.url.endsWith("/api/status") ? STATUS : [] }))
    const session = createSession({ fetch, storage: memoryStorage() })
    session.state.authRequired = authRequired
    const router = createAppRouter(session)
    await router.push("/")
    const wrapper = mount(AppShell, { global: { plugins: [router, ToastService, [OpenVue, { theme: { preset: Aura } }]], provide: { [SESSION as symbol]: session } } })
    await flushPromises()
    return { wrapper, session, router }
  }

  it("links every page of the dashboard from the sidebar", async () => {
    const { wrapper } = await shell()

    expect(wrapper.findAll("a.nav-link").length).toBeGreaterThanOrEqual(8)
    expect(wrapper.text()).toContain("Overview")
  })

  it("signs out and returns to the login page", async () => {
    const { wrapper, router, session } = await shell()

    await wrapper.find("button.sign-out").trigger("click")
    await flushPromises()

    expect(session.state.authenticated).toBe(false)
    expect(router.currentRoute.value.name).toBe("login")
  })

  it("offers no sign out when the daemon asks for no token", async () => {
    const { wrapper } = await shell(false)

    expect(wrapper.find("button.sign-out").exists()).toBe(false)
  })

  it("says how the live updates are doing", async () => {
    const { wrapper } = await shell()

    expect(wrapper.find("[role=status]").text()).toMatch(/Live|Connecting|Manual/)
  })
})
