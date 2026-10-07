import { afterAll, beforeAll, expect, test } from "bun:test"
import { createAudits } from "../src/audit.ts"
import { type Caller, auditAndPlan } from "../src/audit-core.ts"
import { resolveSettings } from "../src/settings.ts"
import { TestDaemon } from "./support/daemon.ts"

const ORPHAN = { drawer_id: "d1", room: "gone" }
const AUDIT = {
  wing: null,
  orphan_drawers: [ORPHAN],
  dangling_provenance_drawers: [],
  stuck_failed_jobs: 0,
  running_jobs: 0,
  drawers_without_embedding: 0,
  total_drawers_in_scope: 1,
}

/** A daemon that holds one orphan until an applied repair removes it, and records every call. */
function scripted() {
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  let orphans = [ORPHAN]
  let n = 0
  const caller: Caller = {
    async call(tool, args = {}) {
      calls.push({ tool, args })
      if (tool === "memcastle_audit") return { id: `a${++n}`, status: "completed", result: { ...AUDIT, orphan_drawers: orphans } } as never
      if (tool === "memcastle_repair") {
        const dry = args.dry_run !== false
        const actions = orphans.map((o) => ({ action: "remove_orphan_drawer", ...o }))
        if (!dry) orphans = []
        return { id: `r${++n}`, status: "completed", result: { dry_run: dry, based_on_job: args.based_on_job, actions } } as never
      }
      return {} as never
    },
  }
  return { caller, calls }
}

function audits(mode: string, caller: Caller) {
  const settings = resolveSettings({ mode }, {})
  const sessions = { session: () => caller } as never
  return createAudits({ settings, sessions, wingOf: () => "castle" })
}

test("an audit reports the orphans and a dry-run plan and applies nothing", async () => {
  const { caller, calls } = scripted()
  const text = await audits("full", caller).audit("s1")
  expect(text).toContain("1 orphan drawer")
  expect(text).toContain("Repair plan")
  expect(calls.filter((c) => c.tool === "memcastle_repair").every((c) => c.args.dry_run === true)).toBe(true)
})

test("a repair without a plan from this session is refused and calls nothing", async () => {
  const { caller, calls } = scripted()
  await expect(audits("full", caller).repair("s1")).rejects.toThrow("no repair plan")
  expect(calls).toEqual([])
})

test("a repair applies only the plan that was shown, once, and writes the diary", async () => {
  const { caller, calls } = scripted()
  const flow = audits("full", caller)
  await flow.audit("s1")
  const text = await flow.repair("s1")
  expect(text).toContain("no orphan drawers are left")
  const applied = calls.filter((c) => c.tool === "memcastle_repair" && c.args.dry_run === false)
  expect(applied).toHaveLength(1)
  expect(applied[0]?.args.based_on_job).toBe("a1")
  expect(calls.at(-1)?.tool).toBe("memcastle_diary_write")
  // The plan is consumed: a second repair is refused rather than applied again.
  await expect(flow.repair("s1")).rejects.toThrow("no repair plan")
})

test("another session cannot apply a plan it was not shown", async () => {
  const { caller } = scripted()
  const flow = audits("full", caller)
  await flow.audit("s1")
  await expect(flow.repair("s2")).rejects.toThrow("no repair plan")
})

test("a read-only session gets the plan but can never apply it", async () => {
  const { caller, calls } = scripted()
  const flow = audits("read-only", caller)
  expect(await flow.audit("s1")).toContain("cannot be applied")
  await expect(flow.repair("s1")).rejects.toThrow("no repair plan")
  expect(calls.some((c) => c.args.dry_run === false)).toBe(false)
})

// --- a real daemon -----------------------------------------------------------------------------------------------

let daemon: TestDaemon
beforeAll(async () => void (daemon = await TestDaemon.start()))
afterAll(async () => daemon.stop())

test("against a real daemon, a fresh palace audits clean and plans nothing", async () => {
  const { audit, plan } = await auditAndPlan(daemon.session("full"))
  expect(audit.orphan_drawers).toEqual([])
  expect(plan).toBeNull()
})

test("against a real daemon, a read-only session can audit and the daemon refuses an applied repair", async () => {
  const session = daemon.session("read-only")
  const { auditJobId } = await auditAndPlan(session)
  await expect(session.call("memcastle_repair", { dry_run: false, based_on_job: auditJobId })).rejects.toThrow()
})
