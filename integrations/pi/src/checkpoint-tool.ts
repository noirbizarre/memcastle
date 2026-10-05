// The manual checkpoint: `/memcastle-checkpoint [note]` forces an out-of-cycle save of what has not been kept yet.
//
// It runs the same review as the interval one, so a manual save and an automatic one cannot disagree about what a
// checkpoint is. It works whatever `checkpoint.enabled` says, because asking for it is the opt-in, and it is always
// visible: the user is waiting for it.
//
// The emergency checkpoint before compaction is the same review submitted with `emergency`; it lives in
// `checkpoint-agent.ts`, triggered from `session_before_compact`.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import { describeOutcome } from "./checkpoint-core.ts"
import { START_HINT } from "./failures.ts"
import { CHECKPOINT_STATUS, type CheckpointSessions, piReviewIo } from "./checkpoint-agent.ts"
import type { McpManager } from "./mcp-manager.ts"

export const CHECKPOINT_COMMAND = "memcastle-checkpoint"

export function registerCheckpointTool(pi: ExtensionAPI, manager: () => McpManager | null, sessions: CheckpointSessions): void {
  pi.registerCommand(CHECKPOINT_COMMAND, {
    description: "Save what is worth keeping from this conversation to MemCastle now; any text after the command is a hint",
    handler: async (args, ctx) => {
      const notify = (message: string, level: "info" | "warning" | "error") => ctx.ui.notify(message, level)
      const current = manager()
      if (!current) {
        // An `off` session has no manager. Saying so is not MemCastle context, it is the reason for the silence.
        notify("MemCastle is not active in this session.", "info")
        return
      }
      // The start has already told the user why it failed; here they only need to know why nothing happened.
      if (!(await current.ready)) {
        notify(`MemCastle is not connected, so nothing was saved. ${START_HINT}`, "warning")
        return
      }
      const state = sessions.for(current)
      ctx.ui.setStatus(CHECKPOINT_STATUS, "MemCastle: checkpointing...")
      try {
        const outcome = await state.review.run(piReviewIo(ctx, current), { note: args })
        // What was just reviewed must not count towards the next interval too.
        if (outcome.kind !== "busy") state.counter.reset()
        notify(`MemCastle: ${describeOutcome(outcome)}`, "info")
      } catch (error) {
        current.report(error, notify)
      } finally {
        ctx.ui.setStatus(CHECKPOINT_STATUS, undefined)
      }
    },
  })
}
