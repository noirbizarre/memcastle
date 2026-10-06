import { describe, expect, it } from "vitest"
import { SseParser } from "../src/api/sse.ts"

describe("the server-sent events parser", () => {
  it("reads a named frame and its data", () => {
    expect(new SseParser().push('event: job\ndata: {"id":"1"}\n\n')).toEqual([{ event: "job", data: '{"id":"1"}' }])
  })

  it("waits for the rest of a frame that is split across chunks", () => {
    const parser = new SseParser()
    expect(parser.push("event: jo")).toEqual([])
    expect(parser.push('b\ndata: {"id"')).toEqual([])
    expect(parser.push(':"1"}\n')).toEqual([])
    expect(parser.push("\n")).toEqual([{ event: "job", data: '{"id":"1"}' }])
  })

  it("returns every frame a chunk completes, in order", () => {
    const frames = new SseParser().push("event: a\ndata: 1\n\nevent: b\ndata: 2\n\nevent: c\ndata: 3")
    expect(frames.map((frame) => frame.event)).toEqual(["a", "b"])
  })

  it("ignores the keep-alive comments that say the stream is alive", () => {
    expect(new SseParser().push(": keep-alive\n\n")).toEqual([])
  })

  it("accepts carriage-return line endings", () => {
    expect(new SseParser().push("event: drawer\r\ndata: {}\r\n\r\n")).toEqual([{ event: "drawer", data: "{}" }])
  })

  it("joins the lines of a multi-line data field with newlines", () => {
    expect(new SseParser().push("data: one\ndata: two\n\n")).toEqual([{ event: "message", data: "one\ntwo" }])
  })

  it("keeps a value's own leading spaces beyond the one the syntax takes", () => {
    expect(new SseParser().push("data:  two spaces\n\n")[0]?.data).toBe(" two spaces")
  })
})
