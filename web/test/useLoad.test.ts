import { flushPromises, mount } from "@vue/test-utils"
import { afterEach, describe, expect, it, vi } from "vitest"
import { defineComponent, h } from "vue"
import { ApiError } from "../src/api/client.ts"
import { useLoad } from "../src/composables/useLoad.ts"

function host<T>(load: () => Promise<T>) {
  let loaded!: ReturnType<typeof useLoad<T>>
  const wrapper = mount(
    defineComponent({
      setup() {
        loaded = useLoad(load)
        return () => h("div")
      },
    }),
  )
  return { wrapper, loaded }
}

afterEach(() => vi.useRealTimers())

describe("loading", () => {
  it("loads once when the page opens and not again by itself", async () => {
    vi.useFakeTimers()
    const load = vi.fn(async () => "fresh")

    const { loaded } = host(load)
    await flushPromises()
    await vi.advanceTimersByTimeAsync(10 * 60 * 1000)

    expect(loaded.data.value).toBe("fresh")
    expect(load).toHaveBeenCalledTimes(1)
  })

  it("loads again when asked, which is what the Refresh button does", async () => {
    let value = 1
    const { loaded } = host(async () => value)
    await flushPromises()

    value = 2
    await loaded.refresh()

    expect(loaded.data.value).toBe(2)
  })

  it("records when the data arrived, and keeps that time and the data when a later load fails", async () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date("2026-10-05T12:00:00Z"))
    let fail = false
    const { loaded } = host(async () => {
      if (fail) throw new ApiError(503, "memcastle::datastore::unavailable", "the datastore is down")
      return "good"
    })
    await flushPromises()

    fail = true
    vi.setSystemTime(new Date("2026-10-05T12:05:00Z"))
    await loaded.refresh()

    expect(loaded.data.value).toBe("good")
    expect(loaded.updatedAt.value?.toISOString()).toBe("2026-10-05T12:00:00.000Z")
    expect(loaded.error.value).toMatchObject({ status: 503 })
  })

  it("clears the error once a load succeeds again", async () => {
    let fail = true
    const { loaded } = host(async () => {
      if (fail) throw new ApiError(0, "network", "no answer")
      return 1
    })
    await flushPromises()
    expect(loaded.error.value).not.toBeNull()

    fail = false
    await loaded.refresh()

    expect(loaded.error.value).toBeNull()
  })
})
