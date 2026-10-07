// The palace audit and repair workflow, without any host: run the read-only audit, plan a dry-run repair, apply it only
// when the caller has the user's confirmation, and record what changed in the diary.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like `checkpoint-core.ts`, so the two agents
// cannot drift apart on what is shown, what is applied or when the diary is written. It holds no audit or repair logic:
// MemCastle finds and fixes, and this file only sequences the calls and words the results.
//
// Confirmation is the host's, never this file's: Pi asks with a dialog, OpenCode with a second command the user runs.
// Every function that changes the palace is `applyRepair` and nothing else, and it refuses unless the session is `full`.

import { MemCastleFailure, failureFromJob } from "./failures.ts"
import type { ModeLabel } from "./modes.ts"

/** The slice of an MCP session the workflow needs. */
export interface Caller {
  call<T = unknown>(tool: string, args?: Record<string, unknown>): Promise<T>
}

export interface AuditReport {
  wing: string | null
  orphan_drawers: { drawer_id: string; room: string }[]
  dangling_provenance_drawers: { drawer_id: string; job_id: string }[]
  stuck_failed_jobs: number
  running_jobs: number
  drawers_without_embedding: number
  total_drawers_in_scope: number
}

export interface RepairReport {
  dry_run: boolean
  based_on_job: string | null
  actions: { action: string; drawer_id?: string; room?: string }[]
}

interface Job<T> {
  id: string
  status: string
  error?: string | null
  result?: T | null
}

const TERMINAL = new Set(["completed", "failed", "cancelled"])

/** Watch a job to its end and return its result. @throws MemCastleFailure when it failed, was cancelled or timed out. */
export async function waitForJob<T>(session: Caller, job: Job<T>, waitMs = 30_000, pollMs = 200): Promise<T> {
  const deadline = Date.now() + waitMs
  let current = job
  while (!TERMINAL.has(current.status) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, pollMs))
    current = await session.call<Job<T>>("memcastle_job_get", { id: current.id })
  }
  const failure = failureFromJob(current)
  if (failure) throw failure
  if (current.status !== "completed" || !current.result) {
    throw new MemCastleFailure(
      "job_failed",
      `MemCastle job ${current.id} did not finish (it is ${current.status}).`,
      null,
      "Check it with memcastle_job_get, and retry it with memcastle_job_retry if it was cancelled.",
    )
  }
  return current.result
}

/** Whether a session in `mode` may change the palace. A `read-only` session can audit and plan, never apply. */
export function mayApply(mode: ModeLabel): boolean {
  return mode === "full"
}

export interface Findings {
  /** The audit job, which the repair is tied to so it can only touch what the user was shown. */
  auditJobId: string
  audit: AuditReport
  /** The dry-run plan, or `null` when the audit found nothing repairable. */
  plan: RepairReport | null
}

/** Run the audit and, when it found orphans, a dry-run repair of exactly those. Changes nothing. */
export async function auditAndPlan(session: Caller, wing?: string): Promise<Findings> {
  const submitted = await session.call<Job<AuditReport>>("memcastle_audit", wing ? { wing } : {})
  const audit = await waitForJob(session, submitted)
  if (audit.orphan_drawers.length === 0) return { auditJobId: submitted.id, audit, plan: null }
  const planned = await session.call<Job<RepairReport>>("memcastle_repair", { dry_run: true, based_on_job: submitted.id })
  return { auditJobId: submitted.id, audit, plan: await waitForJob(session, planned) }
}

export interface Applied {
  repair: RepairReport
  /** A fresh audit after the repair, to show what is left. */
  after: AuditReport
}

/**
 * Apply the repair tied to `auditJobId`. The one function here that writes, so the mode is checked here and not only by
 * the caller: a host that forgot would otherwise pay a rejected call, or worse.
 */
export async function applyRepair(session: Caller, mode: ModeLabel, auditJobId: string, wing?: string): Promise<Applied> {
  if (!mayApply(mode)) {
    throw new MemCastleFailure(
      "mode_rejected",
      `This session is ${mode}, so it cannot apply a repair.`,
      "memcastle::mode::forbidden",
      "Start the session with MEMCASTLE_MODE=full to repair.",
    )
  }
  const submitted = await session.call<Job<RepairReport>>("memcastle_repair", { dry_run: false, based_on_job: auditJobId })
  const repair = await waitForJob(session, submitted)
  const again = await session.call<Job<AuditReport>>("memcastle_audit", wing ? { wing } : {})
  return { repair, after: await waitForJob(session, again) }
}

// --- wording ---------------------------------------------------------------------------------------------------

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`

/** The audit as lines for the user: every finding, then what is not repairable. */
export function describeAudit(audit: AuditReport): string {
  const lines = [`MemCastle audit${audit.wing ? ` (wing "${audit.wing}")` : ""}:`]
  const orphans = audit.orphan_drawers
  lines.push(
    orphans.length === 0
      ? "- no orphan drawers"
      : `- ${plural(orphans.length, "orphan drawer", "orphan drawers")}: ${orphans.map((o) => `${o.drawer_id} (room ${o.room})`).join(", ")}`,
  )
  const dangling = audit.dangling_provenance_drawers.length
  if (dangling > 0) lines.push(`- ${plural(dangling, "drawer", "drawers")} pointing at a job that no longer exists (report only)`)
  if (audit.stuck_failed_jobs > 0) lines.push(`- ${plural(audit.stuck_failed_jobs, "failed job", "failed jobs")} (report only)`)
  if (audit.running_jobs > 0) lines.push(`- ${plural(audit.running_jobs, "job", "jobs")} still running`)
  if (audit.drawers_without_embedding > 0) {
    lines.push(`- ${audit.drawers_without_embedding} of ${audit.total_drawers_in_scope} drawers have no embedding (report only)`)
  }
  return lines.join("\n")
}

/** The dry-run plan, one action per line. */
export function describePlan(plan: RepairReport): string {
  const lines = plan.actions.map((a) => `- ${a.action.replaceAll("_", " ")}${a.drawer_id ? ` ${a.drawer_id}` : ""}${a.room ? ` (room ${a.room})` : ""}`)
  return [`Repair plan (nothing applied yet, ${plural(plan.actions.length, "action", "actions")}):`, ...lines].join("\n")
}

/** One sentence on what an applied repair did. */
export function describeApplied(applied: Applied): string {
  const left = applied.after.orphan_drawers.length
  return `Applied ${plural(applied.repair.actions.length, "repair action", "repair actions")}; ${left === 0 ? "no orphan drawers are left" : `${plural(left, "orphan drawer", "orphan drawers")} left`}.`
}

/** The diary entry that records the before and after. */
export function diarySummary(before: AuditReport, applied: Applied): string {
  return (
    `Palace repair: ${before.orphan_drawers.length} orphan drawers before, ${applied.after.orphan_drawers.length} after; ` +
    `${applied.repair.actions.length} actions applied (${applied.repair.actions.map((a) => a.drawer_id ?? a.action).join(", ")}).`
  )
}

/** Write the summary to the diary. Called only after a confirmed apply, so a declined or read-only run leaves no trace. */
export async function writeDiary(session: Caller, agentIdentity: string, wing: string, content: string): Promise<void> {
  await session.call("memcastle_diary_write", { agent_identity: agentIdentity, wing, content })
}
