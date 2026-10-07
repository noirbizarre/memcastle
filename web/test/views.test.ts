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
import DiaryView from "../src/views/DiaryView.vue"
import MaintenanceView from "../src/views/MaintenanceView.vue"
import SearchView from "../src/views/SearchView.vue"
import SettingsView from "../src/views/SettingsView.vue"
import { fakeFetch, memoryStorage, type Recorded } from "./support/fetch.ts"

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
