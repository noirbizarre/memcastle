import { flushPromises } from "@vue/test-utils"
import { afterEach, describe, expect, it, vi } from "vitest"
import { MemCastleClient } from "../src/api/client.ts"
import type { DaemonEvent } from "../src/api/types.ts"
import { createEvents } from "../src/events.ts"
import { fakeStream } from "./support/fetch.ts"

function setup(options: { token?: string | null; mode?: "full" | "read_only" } = {}) {
  const stream = fakeStream()
  const unauthorized = vi.fn()
  const client = new MemCastleClient({
    fetch: stream.fetch,
    token: () => (options.token === undefined ? "secret" : options.token),
    mode: () => options.mode ?? "full",
    onUnauthorized: unauthorized,
  })
  // A reconnect needs no real time in a test.
  const events = createEvents(client, { sleep: async () => undefined })
  const heard: DaemonEvent[] = []
  return { stream, events, heard, unauthorized, listen: (...kinds: DaemonEvent["kind"][]) => events.subscribe(kinds, (event) => heard.push(event)) }
}

afterEach(() => vi.useRealTimers())

describe("the change stream", () => {
  it("asks with the token and the memory mode, as every other request does", async () => {
    const { stream, events } = setup({ mode: "read_only" })

    events.start()
    await flushPromises()

    expect(stream.requests).toHaveLength(1)
    expect(stream.requests[0]?.url).toBe("/api/events")
    expect(stream.requests[0]?.headers).toMatchObject({ Authorization: "Bearer secret", "X-MemCastle-Mode": "read_only", Accept: "text/event-stream" })
    // The token goes in a header and never in the address.
    expect(stream.requests[0]?.url).not.toContain("secret")
    events.stop()
  })

  it("is live once the daemon says it has opened it", async () => {
    const { stream, events } = setup()

    events.start()
    await flushPromises()
    expect(events.state.status).toBe("connecting")
    stream.open()
    await flushPromises()

    expect(events.state.status).toBe("live")
    events.stop()
    expect(events.state.status).toBe("off")
  })

  it("hands a listener only the kinds it asked for", async () => {
    const { stream, events, heard, listen } = setup()
    listen("job")

    events.start()
    await flushPromises()
    stream.open()
    stream.emit({ kind: "drawer", action: "created", id: "d1" })
    stream.emit({ kind: "job", action: "updated", id: "j1", status: "running" })
    await flushPromises()

    expect(heard.filter((event) => event.kind !== "resync")).toEqual([{ kind: "job", action: "updated", id: "j1", status: "running" }])
    events.stop()
  })

  it("tells every listener to read again whenever the stream opens, and when the daemon says events were missed", async () => {
    const { stream, events, heard, listen } = setup()
    listen("job")

    events.start()
    await flushPromises()
    stream.open()
    await flushPromises()
    expect(heard.map((event) => event.kind)).toEqual(["resync"])

    stream.emit({ kind: "resync", action: "updated" })
    await flushPromises()
    expect(heard.map((event) => event.kind)).toEqual(["resync", "resync"])
    events.stop()
  })

  it("opens the stream again after the daemon closes it, and tells listeners to read what they missed", async () => {
    const { stream, events, heard, listen } = setup()
    listen("job")
    events.start()
    await flushPromises()
    stream.open()
    await flushPromises()

    stream.close()
    await flushPromises()
    expect(stream.opened).toBe(2)
    expect(events.state.status).toBe("connecting")
    stream.open()
    await flushPromises()

    expect(events.state.status).toBe("live")
    expect(heard.map((event) => event.kind)).toEqual(["resync", "resync"])
    events.stop()
  })

  it("keeps trying while the daemon does not answer", async () => {
    const stream = fakeStream()
    let attempts = 0
    const failing = (async (...args: Parameters<typeof fetch>) => {
      attempts += 1
      if (attempts < 3) throw new TypeError("connection refused")
      return stream.fetch(...args)
    }) as typeof fetch
    const events = createEvents(new MemCastleClient({ fetch: failing, token: () => null, mode: () => "full" }), { sleep: async () => undefined })

    events.start()
    await flushPromises()
    stream.open()
    await flushPromises()

    expect(attempts).toBe(3)
    expect(events.state.status).toBe("live")
    events.stop()
  })

  it("gives up on a refusal instead of asking again, and leaves the pages on manual refresh", async () => {
    const { stream, events } = setup()
    stream.refuse(403, { code: "memcastle::mode::forbidden", error: "not permitted" })

    events.start()
    await flushPromises()

    expect(events.state.status).toBe("unavailable")
    expect(stream.requests).toHaveLength(1)
  })

  it("ends the session when the token is refused, like any other request", async () => {
    const { stream, events, unauthorized } = setup()
    stream.refuse(401, { code: "memcastle::auth::unauthorized", error: "no" })

    events.start()
    await flushPromises()

    expect(unauthorized).toHaveBeenCalledOnce()
    expect(events.state.status).toBe("unavailable")
  })

  it("stops listening when asked to unsubscribe", async () => {
    const { stream, events, heard } = setup()
    const unsubscribe = events.subscribe(["job"], (event) => heard.push(event))
    events.start()
    await flushPromises()
    stream.open()
    await flushPromises()

    unsubscribe()
    stream.emit({ kind: "job", action: "updated", id: "j1" })
    await flushPromises()

    expect(heard.map((event) => event.kind)).toEqual(["resync"])
    events.stop()
  })

  it("does not reconnect once it has been stopped", async () => {
    const { stream, events } = setup()
    events.start()
    await flushPromises()
    stream.open()
    await flushPromises()

    events.stop()
    await flushPromises()

    expect(stream.opened).toBe(1)
  })
})
