import { flushPromises, mount } from "@vue/test-utils"
import { afterEach, describe, expect, it, vi } from "vitest"
import { defineComponent, h } from "vue"
import { ApiError, MemCastleClient } from "../src/api/client.ts"
import type { EventKind } from "../src/api/types.ts"
import { useLoad } from "../src/composables/useLoad.ts"
import { createEvents, EVENTS } from "../src/events.ts"
import { fakeStream } from "./support/fetch.ts"

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

/** A page under a shell whose change stream is a fake the test writes to. */
function livePage<T>(load: () => Promise<T>, on: readonly EventKind[]) {
  const stream = fakeStream()
  const events = createEvents(new MemCastleClient({ fetch: stream.fetch, token: () => null, mode: () => "full" }), { sleep: async () => undefined })
  let loaded!: ReturnType<typeof useLoad<T>>
  const wrapper = mount(
    defineComponent({
      setup() {
        loaded = useLoad(load, { on })
        return () => h("div")
      },
    }),
    { global: { provide: { [EVENTS as symbol]: events } } },
  )
  return { stream, events, loaded, wrapper }
}

/** Long enough for the page to let a burst of changes settle, and much shorter than anyone would poll. */
const SETTLED = 300

async function liveAndLoaded<T>(page: ReturnType<typeof livePage<T>>) {
  await flushPromises()
  page.events.start()
  await flushPromises()
  page.stream.open()
  await flushPromises()
  // The stream opening makes every page read once more; the tests below start from the settled state.
  await vi.advanceTimersByTimeAsync(SETTLED)
}

describe("live updates", () => {
  it("reads again when a change it cares about arrives, with no timer and no button", async () => {
    vi.useFakeTimers()
    let progress = 1
    const load = vi.fn(async () => progress)
    const page = livePage(load, ["job"])
    await liveAndLoaded(page)
    expect(page.loaded.data.value).toBe(1)
    const reads = load.mock.calls.length

    progress = 2
    page.stream.emit({ kind: "job", action: "updated", id: "j1", status: "running" })
    await vi.advanceTimersByTimeAsync(SETTLED)

    expect(page.loaded.data.value).toBe(2)
    expect(load).toHaveBeenCalledTimes(reads + 1)
    page.events.stop()
  })

  it("does not poll between changes", async () => {
    vi.useFakeTimers()
    const load = vi.fn(async () => "x")
    const page = livePage(load, ["job"])
    await liveAndLoaded(page)
    const reads = load.mock.calls.length

    await vi.advanceTimersByTimeAsync(10 * 60 * 1000)

    expect(load).toHaveBeenCalledTimes(reads)
    page.events.stop()
  })

  it("reads once for a burst of changes, after the last of them", async () => {
    vi.useFakeTimers()
    let progress = 0
    const load = vi.fn(async () => progress)
    const page = livePage(load, ["job"])
    await liveAndLoaded(page)
    const reads = load.mock.calls.length

    for (let step = 1; step <= 20; step++) {
      progress = step
      page.stream.emit({ kind: "job", action: "updated", id: "j1", status: "running" })
      await vi.advanceTimersByTimeAsync(20)
    }
    await vi.advanceTimersByTimeAsync(SETTLED)

    expect(load).toHaveBeenCalledTimes(reads + 1)
    expect(page.loaded.data.value).toBe(20)
    page.events.stop()
  })

  it("ignores the kinds of change it does not show", async () => {
    vi.useFakeTimers()
    const load = vi.fn(async () => "x")
    const page = livePage(load, ["job"])
    await liveAndLoaded(page)
    const reads = load.mock.calls.length

    page.stream.emit({ kind: "drawer", action: "created", id: "d1" })
    await vi.advanceTimersByTimeAsync(SETTLED)

    expect(load).toHaveBeenCalledTimes(reads)
    page.events.stop()
  })

  it("updates without the spinner, which is for what the user asked for", async () => {
    vi.useFakeTimers()
    const page = livePage(async () => "x", ["job"])
    await liveAndLoaded(page)

    page.stream.emit({ kind: "job", action: "updated", id: "j1" })
    await vi.advanceTimersByTimeAsync(SETTLED - 10)
    const loadingWhileWaiting = page.loaded.loading.value
    await vi.advanceTimersByTimeAsync(20)

    expect(loadingWhileWaiting).toBe(false)
    expect(page.loaded.loading.value).toBe(false)
    page.events.stop()
  })

  it("reads again after the stream reconnects, since changes made while it was down were never heard", async () => {
    vi.useFakeTimers()
    let value = "before"
    const load = vi.fn(async () => value)
    const page = livePage(load, ["job"])
    await liveAndLoaded(page)

    page.stream.close()
    await flushPromises()
    value = "after"
    page.stream.open()
    await vi.advanceTimersByTimeAsync(SETTLED)

    expect(page.loaded.data.value).toBe("after")
    page.events.stop()
  })

  it("stops listening when the page goes away", async () => {
    vi.useFakeTimers()
    const load = vi.fn(async () => "x")
    const page = livePage(load, ["job"])
    await liveAndLoaded(page)
    const reads = load.mock.calls.length

    page.wrapper.unmount()
    page.stream.emit({ kind: "job", action: "updated", id: "j1" })
    await vi.advanceTimersByTimeAsync(SETTLED)

    expect(load).toHaveBeenCalledTimes(reads)
    page.events.stop()
  })

  it("never lets two reads overlap, so an older answer cannot replace a newer one", async () => {
    vi.useFakeTimers()
    const answers: Array<(value: string) => void> = []
    const load = vi.fn(() => new Promise<string>((resolve) => answers.push(resolve)))
    const { loaded } = host(load)
    await flushPromises()

    const first = loaded.refresh()
    const second = loaded.refresh()
    expect(load).toHaveBeenCalledTimes(1)
    answers[0]!("old")
    await flushPromises()
    expect(load).toHaveBeenCalledTimes(2)
    answers[1]!("new")
    await Promise.all([first, second])

    expect(loaded.data.value).toBe("new")
  })

  it("keeps the Refresh button working when there is no stream at all", async () => {
    let value = 1
    const { loaded } = host(async () => value)
    await flushPromises()

    value = 2
    await loaded.refresh()

    expect(loaded.data.value).toBe(2)
  })
})
