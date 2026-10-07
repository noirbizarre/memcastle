// The palace audit and repair for OpenCode sessions, as two steps the user takes one after the other.
//
// OpenCode has no dialog a plugin can ask a yes/no question with, so the confirmation is the second command: the audit
// command reports and plans (dry run) and changes nothing, and only the repair command applies, and only the plan the
// session was just shown. The closest native equivalent of Pi's confirm dialog, and no weaker: a tool argument is never
// the guard (a model can pass one itself), the guard is a plan this session holds, which nothing but an audit creates
// and an applied repair consumes.
//
// What is audited and repaired is MemCastle's; the sequencing and wording are `audit-core.ts`, the file Pi uses.

import {
  type Findings,
  applyRepair,
  auditAndPlan,
  describeApplied,
  describeAudit,
  describePlan,
  diarySummary,
  mayApply,
  writeDiary,
} from "./audit-core.ts"
import { MemCastleFailure } from "./failures.ts"
import type { SessionRegistry } from "./registry.ts"
import type { Settings } from "./settings.ts"

/** The tools the model calls, named apart from the daemon's own `memcastle_audit` and `memcastle_repair`. */
export const AUDIT_TOOL = "memcastle_palace_audit"
export const REPAIR_TOOL = "memcastle_palace_repair"

/** The slash commands: prompt templates in OpenCode 1, so each asks the model to call its tool. */
export const AUDIT_COMMAND = "memcastle-audit"
export const REPAIR_COMMAND = "memcastle-repair"

export interface Audits {
  /** Audit the palace and plan the repair as a dry run. Changes nothing, and remembers the plan for `repair`. */
  audit(sessionId: string, wing?: string): Promise<string>
  /** Apply the plan this session was last shown, once. @throws MemCastleFailure when there is none, or the mode forbids. */
  repair(sessionId: string): Promise<string>
  forget(sessionId: string): void
}

interface Pending extends Findings {
  wing: string | undefined
}

export function createAudits(options: {
  settings: Settings
  sessions: SessionRegistry
  /** The wing the diary entry goes to for a session, or `undefined` for none (then `diary`). */
  wingOf?: (sessionId: string) => string | undefined
}): Audits {
  const { settings, sessions } = options
  const pending = new Map<string, Pending>()

  return {
    async audit(sessionId, wing) {
      // A new audit replaces any earlier plan: what the user is about to confirm is what they were just shown.
      pending.delete(sessionId)
      const findings = await auditAndPlan(sessions.session(sessionId), wing)
      const text = [describeAudit(findings.audit)]
      if (!findings.plan || findings.plan.actions.length === 0) return text.join("\n")
      text.push(describePlan(findings.plan))
      if (!mayApply(settings.mode)) {
        text.push(`This session is ${settings.mode}, so the repair was planned and cannot be applied.`)
        return text.join("\n")
      }
      pending.set(sessionId, { ...findings, wing })
      text.push(`Nothing was changed. Ask the user whether to apply this plan; they confirm by running /${REPAIR_COMMAND}.`)
      return text.join("\n")
    },

    async repair(sessionId) {
      const plan = pending.get(sessionId)
      if (!plan) {
        throw new MemCastleFailure(
          "invalid_input",
          "There is no repair plan to apply in this session.",
          null,
          `Run /${AUDIT_COMMAND} first, read its plan, then run /${REPAIR_COMMAND}.`,
        )
      }
      const session = sessions.session(sessionId)
      // Consumed before the call: a repair that fails half way must not be silently re-applied by a second command.
      pending.delete(sessionId)
      const applied = await applyRepair(session, settings.mode, plan.auditJobId, plan.wing)
      const lines = [describeApplied(applied)]
      try {
        await writeDiary(session, settings.agentIdentity, options.wingOf?.(sessionId) ?? "diary", diarySummary(plan.audit, applied))
      } catch (error) {
        // The repair is done and must be reported as done: only the note about it failed.
        lines.push(`The diary entry could not be written: ${error instanceof MemCastleFailure ? error.toUserMessage() : String(error)}`)
      }
      return lines.join("\n")
    },

    forget: (sessionId) => void pending.delete(sessionId),
  }
}

/** Slash command definitions for OpenCode 1's `config.command`. */
export const AUDIT_COMMAND_DEFINITIONS = {
  [AUDIT_COMMAND]: {
    description: "Audit the MemCastle palace and plan a repair, without changing anything",
    template:
      `Call the \`${AUDIT_TOOL}\` tool once. If the text after this sentence is not empty, pass it as the \`wing\` argument. ` +
      `Show the user the report and plan exactly as the tool returned them, say that nothing has been changed, ` +
      `and tell them to run /${REPAIR_COMMAND} to apply it. Do not call \`${REPAIR_TOOL}\` yourself.\n\n$ARGUMENTS`,
  },
  [REPAIR_COMMAND]: {
    description: "Apply the MemCastle repair plan that /memcastle-audit just showed",
    template:
      `The user ran this command to confirm the repair plan they were shown. Call the \`${REPAIR_TOOL}\` tool once, ` +
      "then tell the user what it answered, in one or two sentences, and stop.",
  },
}

/** Add both slash commands to OpenCode 1's configuration, in place, unless the user already defined one by that name. */
export function addAuditCommands(config: object): void {
  const target = config as { command?: Record<string, unknown> }
  const commands = (target.command ??= {})
  for (const [name, definition] of Object.entries(AUDIT_COMMAND_DEFINITIONS)) commands[name] ??= { ...definition }
}

/** The audit tool's `wing` from whatever a host hands over, which OpenCode 2 types as `unknown`. */
export function auditWing(input: unknown): string | undefined {
  const wing = (typeof input === "object" && input !== null ? (input as Record<string, unknown>).wing : undefined) ?? undefined
  return typeof wing === "string" && wing.trim() !== "" ? wing.trim() : undefined
}
