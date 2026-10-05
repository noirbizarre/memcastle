import { mount, flushPromises } from "@vue/test-utils"
import Aura from "@openvue/themes/aura"
import OpenVue from "openvue/config"
import { describe, expect, it, vi } from "vitest"
import { createAppRouter } from "../src/router.ts"
import { createSession, SESSION } from "../src/session.ts"
import LoginView from "../src/views/LoginView.vue"
import { fakeFetch, memoryStorage, STATUS, UNAUTHORIZED } from "./support/fetch.ts"

const TOKEN = "mc_the_right_token_0123456789"

async function login() {
  const { fetch } = fakeFetch((request) => (request.headers.Authorization === `Bearer ${TOKEN}` ? { body: STATUS } : UNAUTHORIZED))
  const session = createSession({ fetch, storage: memoryStorage() })
  const router = createAppRouter(session)
  await router.push("/login?redirect=/jobs")
  const wrapper = mount(LoginView, { global: { plugins: [router, [OpenVue, { theme: { preset: Aura } }]], provide: { [SESSION as symbol]: session } } })
  return { wrapper, router, session }
}

async function submit(wrapper: Awaited<ReturnType<typeof login>>["wrapper"], password: string) {
  await wrapper.find("input#password").setValue(password)
  await wrapper.find("form").trigger("submit")
  await flushPromises()
}

describe("the login page", () => {
  it("offers the user memcastle and a password field, like the database console", async () => {
    const { wrapper } = await login()

    expect((wrapper.find("input#user").element as HTMLInputElement).value).toBe("memcastle")
    expect(wrapper.find("input#password").attributes("type")).toBe("password")
    expect(wrapper.find("input#password").attributes("autocomplete")).toBe("current-password")
  })

  it("signs in with the right token and returns to the page that was asked for", async () => {
    const { wrapper, router, session } = await login()

    await submit(wrapper, TOKEN)

    expect(session.state.authenticated).toBe(true)
    // The view is a lazily loaded chunk, so the navigation finishes a moment after the form does.
    await vi.waitFor(() => expect(router.currentRoute.value.name).toBe("jobs"))
  })

  it("refuses a wrong token, stays on the page and never shows what was typed", async () => {
    const { wrapper, router, session } = await login()

    await submit(wrapper, "the-wrong-token")

    expect(wrapper.text()).toContain("The user or password was not accepted.")
    expect(wrapper.text()).not.toContain("the-wrong-token")
    expect(session.state.authenticated).toBe(false)
    expect(router.currentRoute.value.name).toBe("login")
  })

  it("ignores a redirect that leaves the dashboard", async () => {
    const { fetch } = fakeFetch({ body: STATUS })
    const session = createSession({ fetch, storage: memoryStorage() })
    const router = createAppRouter(session)
    session.state.authRequired = true
    await router.push("/login?redirect=//evil.example/")
    const wrapper = mount(LoginView, { global: { plugins: [router, [OpenVue, { theme: { preset: Aura } }]], provide: { [SESSION as symbol]: session } } })

    await wrapper.find("input#password").setValue("anything")
    await wrapper.find("form").trigger("submit")
    await flushPromises()

    await vi.waitFor(() => expect(router.currentRoute.value.name).toBe("overview"))
    expect(router.currentRoute.value.fullPath).not.toContain("evil")
  })
})
