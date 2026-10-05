import { describe, expect, it } from "vitest"
import { ago, duration, jobDuration, jobTarget, shortId, utc } from "../src/format.ts"

describe("duration", () => {
  it("shows the two most significant units and none that is zero", () => {
    expect(duration(0)).toBe("0s")
    expect(duration(59)).toBe("59s")
    expect(duration(60)).toBe("1m")
    expect(duration(61)).toBe("1m 1s")
    expect(duration(3600)).toBe("1h")
    expect(duration(4920)).toBe("1h 22m")
    expect(duration(86400 * 2 + 3600 * 5)).toBe("2d 5h")
  })

  it("answers a dash for what is not a span rather than printing NaN", () => {
    expect(duration(Number.NaN)).toBe("-")
    expect(duration(-5)).toBe("-")
  })
})

describe("ago", () => {
  const now = new Date("2026-10-05T12:00:00Z")
  it("says how long ago, against the clock it is given", () => {
    expect(ago("2026-10-05T10:38:00Z", now)).toBe("1h 22m ago")
    expect(ago("2026-10-05T11:59:58Z", now)).toBe("just now")
  })
  it("says a dash for a missing or unreadable time", () => {
    expect(ago(null, now)).toBe("-")
    expect(ago("not a date", now)).toBe("-")
  })
})

describe("jobDuration", () => {
  const now = new Date("2026-10-05T12:00:10Z")
  it("is a dash until the job has started, and runs on until it completes", () => {
    expect(jobDuration({ started_at: null, completed_at: null }, now)).toBe("-")
    expect(jobDuration({ started_at: "2026-10-05T12:00:00Z", completed_at: null }, now)).toBe("10s")
    expect(jobDuration({ started_at: "2026-10-05T12:00:00Z", completed_at: "2026-10-05T12:03:01Z" }, now)).toBe("3m 1s")
  })
})

describe("small helpers", () => {
  it("formats a UTC timestamp for a person", () => {
    expect(utc("2026-10-05T11:22:51.123Z")).toBe("2026-10-05 11:22:51 UTC")
    expect(utc(null)).toBe("-")
  })
  it("shortens an id to the eight characters the CLI shows", () => {
    expect(shortId("a1bca35a-0000-4000-8000-000000000000")).toBe("a1bca35a")
  })
  it("names what a job works on", () => {
    expect(jobTarget({ type: "mine", path: "/home/me/notes" })).toBe("/home/me/notes")
    expect(jobTarget({ type: "extract", wing: "docs" })).toBe("docs")
    expect(jobTarget({ type: "audit" })).toBe("-")
  })
})
