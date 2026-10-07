// The manual palace audit: `/memcastle-audit [wing]` runs the read-only audit, shows what it found, plans the repair as a
// dry run, and applies it only after one explicit confirmation from the user.
//
// The audit and the repair are MemCastle's; this file only decides when to ask. A repair cannot be applied to part of
// a plan (the daemon has no per-item filter), so the one confirmation covers the whole dry-run plan the user was shown.
// A decline, a `read-only` session and a failed audit all end without having changed anything or written a diary entry.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import { applyRepair, auditAndPlan, describeApplied, describeAudit, describePlan, diarySummary, mayApply, writeDiary } from "./audit-core.ts"
import { START_HINT } from "./failures.ts"
import type { McpManager } from "./mcp-manager.ts"
import { wingFor } from "./wake-up-core.ts"

export const AUDIT_COMMAND = "memcastle-audit"
export const AUDIT_STATUS = "memcastle-audit"

export function registerAuditCommand(pi: ExtensionAPI, manager: () => McpManager | null): void {
  pi.registerCommand(AUDIT_COMMAND, {
    description: "Audit the MemCastle palace and, after you confirm, repair what the audit found; an optional wing narrows the audit",
    handler: async (args, ctx) => {
      const notify = (message: string, level: "info" | "warning" | "error") => ctx.ui.notify(message, level)
      const current = manager()
      if (!current) {
        // An `off` session has no manager: saying so is the reason for the silence, not MemCastle context.
        notify("MemCastle is not active in this session.", "info")
        return
      }
      const session = current.session
      if (!session || !(await current.ready)) {
        notify(`MemCastle is not connected, so nothing was audited. ${START_HINT}`, "warning")
        return
      }
      const { settings } = current
      const scope = args.trim() === "" ? undefined : args.trim()
      ctx.ui.setStatus(AUDIT_STATUS, "MemCastle: auditing...")
      try {
        const { auditJobId, audit, plan } = await auditAndPlan(session, scope)
        notify(describeAudit(audit), "info")
        if (!plan || plan.actions.length === 0) return
        notify(describePlan(plan), "info")
        if (!mayApply(settings.mode)) {
          // Offering what the daemon would refuse is worse than saying why it is not offered.
          notify(`This session is ${settings.mode}, so the repair was planned and not applied.`, "info")
          return
        }
        // The default is to decline: nothing is applied unless the user says so.
        const confirmed = await ctx.ui.confirm(
          "Apply the MemCastle repair?",
          `${describePlan(plan)}\n\nThis removes ${plan.actions.length} orphan drawer(s) and cannot be undone.`,
        )
        if (!confirmed) {
          notify("MemCastle: repair declined, nothing was changed.", "info")
          return
        }
        ctx.ui.setStatus(AUDIT_STATUS, "MemCastle: repairing...")
        const applied = await applyRepair(session, settings.mode, auditJobId, scope)
        notify(`MemCastle: ${describeApplied(applied)}`, "info")
        const wing = wingFor(settings.wakeUp, ctx.cwd, current.project) ?? "diary"
        await writeDiary(session, settings.agentIdentity, wing, diarySummary(audit, applied))
      } catch (error) {
        current.report(error, notify)
      } finally {
        ctx.ui.setStatus(AUDIT_STATUS, undefined)
      }
    },
  })
}
