import { describe, expect, it } from "vitest"
import { createAppRouter } from "../src/router.ts"
import { createSession, LOGIN_USER } from "../src/session.ts"
import { fakeFetch, memoryStorage, STATUS, UNAUTHORIZED } from "./support/fetch.ts"

const TOKEN = "mc_the_right_token_0123456789"

function app(authRequired: boolean) {
  const { fetch } = fakeFetch((request) =>
    !authRequired || request.headers.Authorization === `Bearer ${TOKEN}` ? { body: { ...STATUS, auth_enabled: authRequired } } : UNAUTHORIZED,
  )
  const session = createSession({ fetch, storage: memoryStorage() })
  return { session, router: createAppRouter(session) }
}

describe("the route guard", () => {
  it("sends an anonymous visitor to the login and remembers where they were going", async () => {
    const { router } = app(true)

    await router.push("/jobs")

    expect(router.currentRoute.value.name).toBe("login")
    expect(router.currentRoute.value.query.redirect).toBe("/jobs")
  })

  it("lets a signed-in visitor through, and bounces them off the login page", async () => {
    const { router, session } = app(true)
    await session.signIn(LOGIN_USER, TOKEN)

    await router.push("/jobs")
    expect(router.currentRoute.value.name).toBe("jobs")

    await router.push("/login")
    expect(router.currentRoute.value.name).toBe("overview")
  })

  it("never shows a login page to a daemon that wants no token", async () => {
    const { router } = app(false)

    await router.push("/login")

    expect(router.currentRoute.value.name).toBe("overview")
  })

  it("turns an unknown path into the overview rather than a blank page", async () => {
    const { router } = app(false)

    await router.push("/no/such/page")

    expect(router.currentRoute.value.name).toBe("overview")
  })

  it("returns to the login from wherever the user is when the token is refused", async () => {
    const { router, session } = app(true)
    await session.signIn(LOGIN_USER, TOKEN)
    await router.push("/jobs")

    // What the client does when the daemon answers a request that carried the token with a 401.
    session.state.authenticated = false
    await new Promise((resolve) => setTimeout(resolve, 0))
    await router.isReady()

    expect(router.currentRoute.value.name).toBe("login")
    expect(router.currentRoute.value.query.redirect).toBe("/jobs")
  })
})
