import { describe, expect, it } from "vitest"
import { createSession, LOGIN_USER } from "../src/session.ts"
import { fakeFetch, memoryStorage, STATUS, UNAUTHORIZED } from "./support/fetch.ts"

const TOKEN = "mc_the_right_token_0123456789"

/** A daemon that wants `TOKEN`, answering `/api/status` only to it. */
function daemonWithAuth() {
  return fakeFetch((request) => (request.headers.Authorization === `Bearer ${TOKEN}` ? { body: STATUS } : UNAUTHORIZED))
}

describe("finding out whether to sign in", () => {
  it("skips the login when the daemon asks for no token", async () => {
    const { fetch } = fakeFetch({ body: { ...STATUS, auth_enabled: false } })
    const session = createSession({ fetch, storage: memoryStorage() })

    await session.establish()

    expect(session.state).toMatchObject({ authRequired: false, authenticated: true })
  })

  it("needs the login when the daemon refuses an anonymous request", async () => {
    const { fetch } = daemonWithAuth()
    const session = createSession({ fetch, storage: memoryStorage() })

    await session.establish()

    expect(session.state).toMatchObject({ authRequired: true, authenticated: false, token: null })
  })

  it("resumes with a token kept from earlier in the tab", async () => {
    const { fetch } = daemonWithAuth()
    const session = createSession({ fetch, storage: memoryStorage({ "memcastle.token": TOKEN }) })

    await session.establish()

    expect(session.state).toMatchObject({ authRequired: true, authenticated: true })
  })

  it("drops a kept token the daemon no longer accepts, and says so", async () => {
    const { fetch } = daemonWithAuth()
    const storage = memoryStorage({ "memcastle.token": "an-old-token" })
    const session = createSession({ fetch, storage })

    await session.establish()

    expect(session.state.authenticated).toBe(false)
    expect(session.state.notice).toContain("no longer accepted")
    expect(storage.items.has("memcastle.token")).toBe(false)
  })

  it("is not a login problem when the daemon cannot be reached", async () => {
    const session = createSession({
      fetch: (async () => {
        throw new TypeError("fetch failed")
      }) as unknown as typeof fetch,
      storage: memoryStorage(),
    })

    await expect(session.establish()).rejects.toMatchObject({ code: "network" })
    expect(session.state.authRequired).toBeNull()
  })
})

describe("signing in", () => {
  it("accepts the right token for the user memcastle and keeps it for the tab only", async () => {
    const { fetch, requests } = daemonWithAuth()
    const storage = memoryStorage()
    const session = createSession({ fetch, storage })

    await session.signIn(LOGIN_USER, `  ${TOKEN}  `)

    expect(session.state).toMatchObject({ authenticated: true, token: TOKEN })
    expect(storage.items.get("memcastle.token")).toBe(TOKEN)
    expect(requests[0]?.headers.Authorization).toBe(`Bearer ${TOKEN}`)
  })

  it("refuses a wrong token with the database console's sentence and stores nothing", async () => {
    const { fetch } = daemonWithAuth()
    const storage = memoryStorage()
    const session = createSession({ fetch, storage })

    const error = await session.signIn(LOGIN_USER, "the-wrong-token").catch((caught: Error) => caught)

    expect(error).toMatchObject({ message: "The user or password was not accepted." })
    expect((error as Error).message).not.toContain("the-wrong-token")
    expect(session.state).toMatchObject({ authenticated: false, token: null, notice: null })
    expect(storage.items.size).toBe(0)
  })

  it("refuses another user without asking the daemon, and without saying which half was wrong", async () => {
    const { fetch, requests } = daemonWithAuth()
    const session = createSession({ fetch, storage: memoryStorage() })

    await expect(session.signIn("admin", TOKEN)).rejects.toMatchObject({ message: "The user or password was not accepted." })

    expect(requests).toHaveLength(0)
  })

  it("does not call a failed attempt a session that ended", async () => {
    const { fetch } = daemonWithAuth()
    const session = createSession({ fetch, storage: memoryStorage() })

    await session.signIn(LOGIN_USER, "nope").catch(() => undefined)

    expect(session.state.notice).toBeNull()
  })
})

describe("a session that ends", () => {
  it("returns to the login when the daemon stops accepting the token", async () => {
    let revoked = false
    const { fetch } = fakeFetch(() => (revoked ? UNAUTHORIZED : { body: STATUS }))
    const session = createSession({ fetch, storage: memoryStorage() })
    await session.signIn(LOGIN_USER, TOKEN)

    revoked = true
    await session.client.wings().catch(() => undefined)

    expect(session.state.authenticated).toBe(false)
    expect(session.state.token).toBeNull()
    expect(session.state.notice).toContain("revoked")
  })

  it("forgets the token on sign out", async () => {
    const { fetch } = daemonWithAuth()
    const storage = memoryStorage()
    const session = createSession({ fetch, storage })
    await session.signIn(LOGIN_USER, TOKEN)

    session.signOut()

    expect(session.state).toMatchObject({ authenticated: false, token: null })
    expect(storage.items.has("memcastle.token")).toBe(false)
  })
})

describe("the memory mode", () => {
  it("is full by default, is sent on every request and survives a reload of the tab", async () => {
    const { fetch, requests } = fakeFetch({ body: STATUS })
    const storage = memoryStorage()
    const session = createSession({ fetch, storage })
    expect(session.state.mode).toBe("full")

    session.setMode("read_only")
    await session.client.status()
    const reloaded = createSession({ fetch, storage })

    expect(requests[0]?.headers["X-MemCastle-Mode"]).toBe("read_only")
    expect(reloaded.state.mode).toBe("read_only")
  })

  it("ignores a stored value that is not a mode it offers", () => {
    const session = createSession({ storage: memoryStorage({ "memcastle.mode": "disabled" }) })

    expect(session.state.mode).toBe("full")
  })
})
