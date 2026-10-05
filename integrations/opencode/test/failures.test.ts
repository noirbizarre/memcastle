import { expect, test } from "bun:test"
import {
  MemCastleFailure,
  failureFromBody,
  failureFromJob,
  failureFromStatus,
  failureFromTransport,
  parseErrorBody,
} from "../src/failures.ts"
import { fixture } from "./support/fixtures.ts"

interface FailureClassFixture {
  id: string
  http_status: number | null
  code: string | null
  also_codes?: string[]
}
const classes = fixture<{ classes: FailureClassFixture[] }>("failure-classes.json").classes

test("every class in the shared fixture is one this client can produce, so the two cannot drift apart", () => {
  const produced = ["daemon_unavailable", "unauthorized", "mode_rejected", "invalid_input", "job_failed"]
  expect(classes.map((entry) => entry.id).sort()).toEqual(produced.sort())
})

test("an error body is classified by its public code, whatever its message says", () => {
  for (const entry of classes.filter((entry) => entry.code !== null)) {
    const failure = failureFromBody({ error: "reworded message", code: entry.code, help: "do this" })
    expect(failure.failureClass).toBe(entry.id as never)
    expect(failure.toUserMessage()).toContain("do this")
  }
})

test("every code the daemon answers with a 400 is invalid input over MCP too, where there is no status to go by", () => {
  const entry = classes.find((entry) => entry.id === "invalid_input")
  const codes = [entry?.code, ...(entry?.also_codes ?? [])]
  expect(codes.length).toBeGreaterThan(1)
  for (const code of codes) {
    expect(failureFromBody({ error: "refused", code: code as string, help: null }).failureClass).toBe("invalid_input")
  }
})

test("a code that is not a caller's mistake stays unexpected when no status says otherwise", () => {
  expect(failureFromBody({ error: "boom", code: "memcastle::migrate::failed", help: null }).failureClass).toBe(
    "unexpected",
  )
})

test("an HTTP status with the documented code is classified as the fixture says", () => {
  for (const entry of classes.filter((entry) => entry.http_status !== null)) {
    expect(failureFromStatus(entry.http_status as number, "").failureClass).toBe(entry.id as never)
  }
})

test("a bare 401 still tells the user a token is needed and where to set it", () => {
  const failure = failureFromStatus(401, "")
  expect(failure.failureClass).toBe("unauthorized")
  expect(failure.toUserMessage()).toContain("MEMCASTLE_AUTH_TOKEN")
})

test("a body that is not an error body is not mistaken for one", () => {
  expect(parseErrorBody("<html>bad gateway</html>")).toBeNull()
  expect(parseErrorBody('{"status":"ok"}')).toBeNull()
  expect(failureFromStatus(502, "<html>bad gateway</html>").failureClass).toBe("unexpected")
})

test("an unreachable daemon is reported with how to start it, never as an empty memory", () => {
  const failure = failureFromTransport("http://127.0.0.1:1", new Error("connection refused"))
  expect(failure.failureClass).toBe("daemon_unavailable")
  expect(failure.toUserMessage()).toContain("memcastle daemon start")
  expect(failure.toUserMessage()).toContain("http://127.0.0.1:1")
})

test("only a failed job is a failure, and it carries the job's own error and how to retry", () => {
  expect(failureFromJob({ id: "1", status: "completed" })).toBeNull()
  expect(failureFromJob({ id: "1", status: "running" })).toBeNull()
  const failure = failureFromJob({ id: "j-1", status: "failed", error: "disk full" })
  expect(failure).toBeInstanceOf(MemCastleFailure)
  expect(failure?.failureClass).toBe("job_failed")
  expect(failure?.toUserMessage()).toContain("disk full")
  expect(failure?.toUserMessage()).toContain("memcastle_job_retry")
})
