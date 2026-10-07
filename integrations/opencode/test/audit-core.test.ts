import { expect, test } from "bun:test"
import {
  type AuditReport,
  type Caller,
  applyRepair,
  auditAndPlan,
  describeApplied,
  describeAudit,
  describePlan,
  diarySummary,
  mayApply,
  waitForJob,
  writeDiary,
} from "../src/audit-core.ts"
import { MemCastleFailure } from "../src/failures.ts"

const CLEAN: AuditReport = {
  wing: null,
  orphan_drawers: [],
  dangling_provenance_drawers: [],
  stuck_failed_jobs: 0,
  running_jobs: 0,
  drawers_without_embedding: 0,
  total_drawers_in_scope: 4,
}
const DIRTY: AuditReport = {
  wing: "castle",
  orphan_drawers: [{ drawer_id: "d1", room: "gone" }, { drawer_id: "d2", room: "gone" }],
  dangling_provenance_drawers: [{ drawer_id: "d3", job_id: "j9" }],
  stuck_failed_jobs: 2,
  running_jobs: 1,
  drawers_without_embedding: 3,
  total_drawers_in_scope: 9,
}

/** A session that answers each call from a queue, and records what it was asked. */
function queued(...answers: unknown[]) {
  const calls: { tool: string; args: Record<string, unknown> | undefined }[] = []
  const caller: Caller = {
    async call(tool, args) {
      calls.push({ tool, args })
      return answers.shift() as never
    },
  }
  return { caller, calls }
}

test("a job that is still running is polled until it completes", async () => {
  const { caller, calls } = queued({ id: "j1", status: "running" }, { id: "j1", status: "completed", result: { ok: true } })
  const result = await waitForJob(caller, { id: "j1", status: "queued" }, 1000, 1)
  expect(result).toEqual({ ok: true })
  expect(calls.map((call) => call.tool)).toEqual(["memcastle_job_get", "memcastle_job_get"])
})

test("a job that fails is reported with its error, not as an empty result", async () => {
  await expect(waitForJob(queued().caller, { id: "j1", status: "failed", error: "the store is locked" })).rejects.toBeInstanceOf(MemCastleFailure)
})

test("a job that never finishes in time is reported as not finished and says how to retry", async () => {
  const { caller } = queued({ id: "j1", status: "running" })
  const failure = await waitForJob(caller, { id: "j1", status: "running" }, 0, 1).catch((error: unknown) => error)
  expect((failure as MemCastleFailure).message).toContain("did not finish")
  expect((failure as MemCastleFailure).help).toContain("memcastle_job_retry")
})

test("a completed job with no result is not mistaken for success", async () => {
  await expect(waitForJob(queued().caller, { id: "j1", status: "completed" })).rejects.toThrow("did not finish")
})

test("only a full session may apply a repair", () => {
  expect(mayApply("full")).toBe(true)
  expect(mayApply("read-only")).toBe(false)
})

test("an audit that finds nothing plans no repair and asks for none", async () => {
  const { caller, calls } = queued({ id: "a1", status: "completed", result: CLEAN })
  const findings = await auditAndPlan(caller)
  expect(findings.plan).toBeNull()
  expect(calls).toHaveLength(1)
  expect(calls[0]?.args).toEqual({})
})

test("an audit narrowed to a wing passes the wing and plans a dry run tied to that audit", async () => {
  const plan = { dry_run: true, based_on_job: "a1", actions: [] }
  const { caller, calls } = queued({ id: "a1", status: "completed", result: DIRTY }, { id: "r1", status: "completed", result: plan })
  const findings = await auditAndPlan(caller, "castle")
  expect(calls[0]?.args).toEqual({ wing: "castle" })
  expect(calls[1]?.args).toEqual({ dry_run: true, based_on_job: "a1" })
  expect(findings.plan).toEqual(plan)
})

test("a read-only session cannot apply a repair and calls nothing", async () => {
  const { caller, calls } = queued()
  const failure = await applyRepair(caller, "read-only", "a1").catch((error: unknown) => error)
  expect((failure as MemCastleFailure).failureClass).toBe("mode_rejected")
  expect(calls).toEqual([])
})

test("an applied repair is followed by a fresh audit of the same wing", async () => {
  const repair = { dry_run: false, based_on_job: "a1", actions: [{ action: "remove_orphan_drawer", drawer_id: "d1" }] }
  const { caller, calls } = queued({ id: "r1", status: "completed", result: repair }, { id: "a2", status: "completed", result: CLEAN })
  const applied = await applyRepair(caller, "full", "a1", "castle")
  expect(calls[0]?.args).toEqual({ dry_run: false, based_on_job: "a1" })
  expect(calls[1]?.args).toEqual({ wing: "castle" })
  expect(applied.after).toEqual(CLEAN)
})

test("the audit is worded finding by finding, and a clean one says so", () => {
  expect(describeAudit(CLEAN)).toContain("no orphan drawers")
  const text = describeAudit(DIRTY)
  expect(text).toContain('wing "castle"')
  expect(text).toContain("2 orphan drawers: d1 (room gone), d2 (room gone)")
  expect(text).toContain("1 drawer pointing at a job that no longer exists")
  expect(text).toContain("2 failed jobs")
  expect(text).toContain("1 job still running")
  expect(text).toContain("3 of 9 drawers have no embedding")
})

test("the plan, the result and the diary entry name what was done", () => {
  const repair = { dry_run: false, based_on_job: "a1", actions: [{ action: "remove_orphan_drawer", drawer_id: "d1", room: "gone" }, { action: "noop" }] }
  expect(describePlan(repair)).toContain("remove orphan drawer d1 (room gone)")
  expect(describePlan(repair)).toContain("2 actions")
  const applied = { repair, after: { ...DIRTY, orphan_drawers: [{ drawer_id: "d2", room: "gone" }] } }
  expect(describeApplied(applied)).toContain("1 orphan drawer left")
  expect(describeApplied({ repair, after: CLEAN })).toContain("no orphan drawers are left")
  expect(diarySummary(DIRTY, applied)).toContain("2 orphan drawers before, 1 after")
})

test("the diary entry is written as the agent, in the wing", async () => {
  const { caller, calls } = queued({})
  await writeDiary(caller, "pi", "castle", "repaired")
  expect(calls[0]).toEqual({ tool: "memcastle_diary_write", args: { agent_identity: "pi", wing: "castle", content: "repaired" } })
})
