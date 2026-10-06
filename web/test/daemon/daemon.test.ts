// The dashboard against a real daemon: the client, the login and the static files, over real HTTP.
// Needs a build of the dashboard (`bun run build`) and of the daemon (`cargo build`); `mise run web:check` does both.

import { afterAll, beforeAll, describe, expect, it } from "bun:test"
import { existsSync } from "node:fs"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { ApiError } from "../../src/api/client.ts"
import type { DaemonEvent } from "../../src/api/types.ts"
import { createEvents } from "../../src/events.ts"
import { createSession, LOGIN_USER } from "../../src/session.ts"
import { emptyAssets, TestDaemon, WORKTREE } from "./support.ts"

const TOKEN = "mc_a_token_for_the_dashboard_suite_0123456789"

describe("the dashboard's client against a daemon that needs a token", () => {
  let daemon: TestDaemon
  beforeAll(async () => {
    daemon = await TestDaemon.start({ token: TOKEN, web: true })
  })
  afterAll(async () => daemon.stop())

  it("is refused without a token and with a wrong one, and served with the right one", async () => {
    const anonymous = await daemon.client(null).status().catch((error: unknown) => error)
    const wrong = await daemon.client("not-the-token").status().catch((error: unknown) => error)
    const status = await daemon.client().status()

    expect(anonymous).toBeInstanceOf(ApiError)
    expect((anonymous as ApiError).unauthorized).toBe(true)
    expect((wrong as ApiError).code).toBe("memcastle::auth::unauthorized")
    expect(status.auth_enabled).toBe(true)
  })

  it("signs in through the same path the login page uses, and a wrong password is refused", async () => {
    const session = createSession({ baseUrl: daemon.baseUrl, storage: { getItem: () => null, setItem() {}, removeItem() {} } })
    await session.establish()
    expect(session.state).toMatchObject({ authRequired: true, authenticated: false })

    await expect(session.signIn(LOGIN_USER, "the-wrong-token")).rejects.toThrow("The user or password was not accepted.")
    await session.signIn(LOGIN_USER, TOKEN)

    expect(session.state.authenticated).toBe(true)
    expect((await session.client.config()).auth_enabled).toBe(true)
  })

  it("reads the configuration in effect, with no secret in it", async () => {
    const config = await daemon.client().config()

    expect(config.web).toEqual({ enabled: true, built: true })
    expect(config.assets.source).toBe("override")
    expect(JSON.stringify(config)).not.toContain(TOKEN)
  })

  it("pages jobs by kind and limit, and the read-only mode cannot submit one", async () => {
    const api = daemon.client()
    for (let i = 0; i < 3; i++) await api.submitJob({ type: "audit" })

    expect((await api.jobs({ kind: "audit" })).length).toBe(3)
    expect((await api.jobs({ kind: "audit", limit: 2 })).length).toBe(2)
    expect(await api.jobs({ kind: "mine" })).toEqual([])
    // An audit only reads, so it is allowed; mining files drawers, so a read-only session is refused.
    await daemon.client(TOKEN, "read_only").submitJob({ type: "audit" })
    const refused = await daemon.client(TOKEN, "read_only").submitJob({ type: "mine", path: "/tmp" }).catch((error: unknown) => error)
    expect((refused as ApiError).status).toBe(403)
  })

  it("answers an empty graph and an unknown entity with the daemon's own errors", async () => {
    const api = daemon.client()

    expect(await api.graph()).toEqual({ nodes: [], edges: [], truncated: false })
    const missing = await api.graph({ entity: "00000000-0000-0000-0000-000000000000" }).catch((error: unknown) => error)
    expect((missing as ApiError).status).toBe(404)
  })

  it("browses the hierarchy once something has been written", async () => {
    const api = daemon.client()
    await api.writeNote("docs", "plans", "Ship the dashboard")

    const wings = await api.wings()
    const detail = await api.wing("docs")
    const drawers = await api.drawers("docs", "plans")
    const drawer = await api.drawer("docs", "plans", drawers[0]!.name ?? drawers[0]!.id)

    expect(wings.map((wing) => wing.name)).toContain("docs")
    expect(detail.rooms.map((room) => room.name)).toContain("plans")
    expect(drawer.content).toBe("Ship the dashboard")
    expect((await api.search({ q: "dashboard" })).length).toBe(1)
  })
})

describe("the change stream against a real daemon", () => {
  let daemon: TestDaemon
  beforeAll(async () => {
    daemon = await TestDaemon.start({ token: TOKEN })
  })
  afterAll(async () => daemon.stop())

  /** Resolves with the events heard until `done` says so, rejecting if the daemon says nothing for too long. */
  function until(events: ReturnType<typeof createEvents>, done: (heard: DaemonEvent[]) => boolean): Promise<DaemonEvent[]> {
    return new Promise((resolve, reject) => {
      const heard: DaemonEvent[] = []
      const timer = setTimeout(() => reject(new Error(`no matching events in time; heard ${JSON.stringify(heard)}`)), 30_000)
      events.subscribe(["job", "drawer", "wing", "room", "entity"], (event) => {
        heard.push(event)
        if (done(heard)) {
          clearTimeout(timer)
          resolve(heard)
        }
      })
    })
  }

  it("pushes a job's progress and its end, with nothing polling for them", async () => {
    const api = daemon.client()
    const events = createEvents(api)
    const directory = await mkdtemp(join(tmpdir(), "memcastle-web-sse-"))
    try {
      for (const name of ["a", "b", "c"]) await writeFile(join(directory, `${name}.md`), `# ${name}\n\nsome words about ${name}`)
      events.start()
      // The stream is open once the daemon has said so, which is also when it first tells a page to read.
      await until(events, (heard) => heard.some((event) => event.kind === "resync"))
      const settled = until(events, (heard) => heard.some((event) => event.kind === "job" && event.status === "completed"))

      const job = await api.submitJob({ type: "mine", path: directory, wing: "sse" })
      const heard = (await settled).filter((event) => event.kind === "job" && event.id === job.id)

      expect(heard.map((event) => event.status)).toContain("queued")
      expect(heard.map((event) => event.status)).toContain("completed")
      // Progress is a write to a running job: more than one `running` event means the page would have seen it advance.
      expect(heard.filter((event) => event.status === "running").length).toBeGreaterThan(1)
      // What an event leaves out, a read finds.
      expect((await api.job(job.id)).status).toBe("completed")
      expect(JSON.stringify(heard)).not.toContain(directory)
    } finally {
      events.stop()
      await rm(directory, { recursive: true, force: true })
    }
  })

  it("announces a new note's wing and drawer, by id", async () => {
    const api = daemon.client()
    const events = createEvents(api)
    events.start()
    await until(events, (heard) => heard.some((event) => event.kind === "resync"))
    const heard = until(events, (all) => all.some((event) => event.kind === "drawer" && event.action === "created"))

    await api.writeNote("notes-wing", "inbox", "a thought nobody should see in an event")

    const seen = await heard
    expect(seen.map((event) => event.kind)).toContain("wing")
    expect(JSON.stringify(seen)).not.toContain("a thought")
    events.stop()
  })

  it("refuses the stream without a token, as it refuses every route", async () => {
    const events = createEvents(daemon.client(null))
    events.start()
    await new Promise((resolve) => setTimeout(resolve, 500))

    expect(events.state.status).toBe("unavailable")
    events.stop()
  })
})

describe("the static files", () => {
  it("serve the real build from the worktree, to a browser that has no token yet", async () => {
    // A build is a precondition, not a skip: the point is that the shipped files are what the daemon serves.
    expect(existsSync(join(WORKTREE, "web/dist/index.html"))).toBe(true)
    const daemon = await TestDaemon.start({ token: TOKEN, web: true })
    try {
      const index = await fetch(`${daemon.baseUrl}/ui/`)
      const html = await index.text()
      const script = /src="(\/ui\/assets\/[^"]+\.js)"/.exec(html)?.[1]
      const asset = script ? await fetch(`${daemon.baseUrl}${script}`) : undefined
      const api = await fetch(`${daemon.baseUrl}/api/status`)

      expect(index.status).toBe(200)
      expect(html).toContain('<div id="app">')
      expect(asset?.status).toBe(200)
      expect(asset?.headers.get("content-type")).toContain("javascript")
      // And the data behind it is still guarded.
      expect(api.status).toBe(401)
    } finally {
      await daemon.stop()
    }
  })

  it("are not served at all when the dashboard is not enabled", async () => {
    const daemon = await TestDaemon.start()
    try {
      expect((await fetch(`${daemon.baseUrl}/ui/`)).status).toBe(404)
    } finally {
      await daemon.stop()
    }
  })

  it("answer a page that names the remedy when it is enabled but not installed", async () => {
    const assets = await emptyAssets()
    const daemon = await TestDaemon.start({ web: true, assets })
    try {
      const response = await fetch(`${daemon.baseUrl}/ui/`)
      const page = await response.text()

      expect(response.status).toBe(503)
      expect(page).toContain("mise run web:build")
      expect((await daemon.client(null).config()).web).toEqual({ enabled: true, built: false })
    } finally {
      await daemon.stop()
      await rm(assets, { recursive: true, force: true })
    }
  })
})
