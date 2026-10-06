import { describe, expect, it } from "vitest"
import type { OptionSpec } from "../src/api/types.ts"
import { type MineForm, mineOptions, mineRequest } from "../src/mine.ts"

const since: OptionSpec = { name: "since", description: "from this date", type: "date" }
const dir: OptionSpec = { name: "dir", description: "this directory", type: "path" }

const form = (change: Partial<MineForm> = {}): MineForm => ({ kind: "source", path: "", source: "opencode", locator: "", wing: "", full: false, options: {}, ...change })

describe("the options of a mine job", () => {
  it("sends only the ones that were filled in, trimmed", () => {
    expect(mineOptions({ since: " 2026-09 ", dir: "  " }, [since, dir])).toEqual({ since: "2026-09" })
  })
  it("sends none when nothing was filled in, so a plain mine is the request it always was", () => {
    expect(mineOptions({ since: "" }, [since])).toBeUndefined()
    expect(mineRequest(form(), [since])).not.toHaveProperty("options", expect.anything())
  })
  it("leaves out a key the source does not declare, such as one typed for the source chosen before", () => {
    expect(mineOptions({ since: "2026-09", dir: "/work/app" }, [since])).toEqual({ since: "2026-09" })
  })
})

describe("the mine request", () => {
  it("names a directory by its path and carries its options", () => {
    const request = mineRequest(form({ kind: "directory", path: " /work/app ", options: { since: "2026-09" } }), [since])

    expect(request).toEqual({ type: "mine", path: "/work/app", options: { since: "2026-09" }, wing: undefined, full: undefined })
  })
  it("names a source with its place to read, options, wing and full", () => {
    const request = mineRequest(form({ locator: "work-laptop", wing: "history", full: true, options: { dir: "/work/*" } }), [since, dir])

    expect(request).toEqual({ type: "mine", source: "opencode", locator: "work-laptop", options: { dir: "/work/*" }, wing: "history", full: true })
  })
})
