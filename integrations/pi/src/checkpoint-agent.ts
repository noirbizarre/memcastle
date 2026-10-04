// Interval review: every N exchanges the extension asks its own model what in the conversation is worth keeping, and
// submits that as a checkpoint.
//
// Classification happens here, client-side, never in MemCastle: the daemon stores what it is handed and decides
// nothing. What is host-free (the settings, the review, the payload) lives in `checkpoint-core.ts`; this file only
// knows how Pi counts an exchange, reads its transcript and asks its model.
//
// One exchange is one `agent_end`: Pi runs the agent once per prompt, and `turn_end` fires for every model turn inside
// that run, which would make a tool-heavy answer count as many exchanges.

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent"
import { CheckpointReview, ExchangeCounter, type ReviewIo, type Turn, describeOutcome, textOf, turnOf } from "./checkpoint-core.ts"
import { MemCastleFailure } from "./failures.ts"
import type { McpManager } from "./mcp-manager.ts"
import { readSkill } from "./skill-text.ts"

/** The shared skill that says what is worth keeping, used as the reviewing model's instructions. */
export const CHECKPOINT_SKILL = "checkpoint-instructions"

/** Names this extension's footer status while a review is visible. */
export const CHECKPOINT_STATUS = "memcastle-checkpoint"

/** The most a review's reply may run to: a payload of a handful of items is far below it. */
const MAX_REPLY_TOKENS = 4096

/** The review state of one Pi session: its counter and its one review at a time. */
interface SessionCheckpoints {
  manager: McpManager
  review: CheckpointReview
  counter: ExchangeCounter
}

/**
 * The checkpoint state of the Pi session, tied to the manager that owns its connection.
 * A new manager is a new session, so it gets fresh state, and an old review never submits through the new connection.
 */
export class CheckpointSessions {
  private current: SessionCheckpoints | null = null

  /** The state for `manager`, created on first use. */
  for(manager: McpManager): SessionCheckpoints {
    if (this.current?.manager === manager) return this.current
    this.current?.review.abort()
    const { settings } = manager
    this.current = {
      manager,
      counter: new ExchangeCounter(settings.checkpoint.interval),
      review: new CheckpointReview({
        mode: settings.mode,
        agentIdentity: settings.agentIdentity,
        project: () => manager.project,
        session: () => manager.session,
        skill: () => readSkill(CHECKPOINT_SKILL),
      }),
    }
    return this.current
  }

  /** End the session's state, stopping a review in flight. */
  drop(): void {
    this.current?.review.abort()
    this.current = null
  }
}

/** The conversation and the model a review reads and asks, for the Pi session `ctx` belongs to. */
export function piReviewIo(ctx: ExtensionContext, manager: McpManager): ReviewIo {
  const wanted = manager.settings.checkpoint.model
  return {
    transcript: () =>
      ctx.sessionManager
        .getBranch()
        // Only what was said: tool results, summaries and injected context are not the conversation.
        .flatMap((entry): Turn[] => {
          if (entry.type !== "message") return []
          const turn = turnOf(entry.message.role, "content" in entry.message ? entry.message.content : undefined)
          return turn ? [turn] : []
        }),
    classify: async (request, signal) => {
      const model = wanted ? ctx.modelRegistry.find(wanted.provider, wanted.id) : ctx.model
      if (!model) {
        throw new MemCastleFailure(
          "invalid_input",
          wanted
            ? `The checkpoint model ${wanted.provider}/${wanted.id} is not one Pi knows.`
            : "No model is selected, so there is nothing to review the conversation with.",
          null,
          wanted ? "Fix MEMCASTLE_CHECKPOINT_MODEL, or unset it to review with the session's own model." : "Select a model, then try again.",
        )
      }
      const response = await ctx.modelRegistry.complete(
        model,
        {
          systemPrompt: request.system,
          messages: [{ role: "user", content: [{ type: "text", text: request.prompt }], timestamp: Date.now() }],
        },
        { signal, maxTokens: MAX_REPLY_TOKENS },
      )
      // A provider error comes back as a message rather than a throw, and its text would be read as a malformed reply.
      if (response.stopReason === "error") {
        throw new MemCastleFailure(
          "unexpected",
          `The checkpoint model failed: ${response.errorMessage ?? "no reason was given"}.`,
          null,
          "Nothing was saved. Try again, or choose another model with `checkpoint.model`.",
        )
      }
      return textOf(response.content)
    },
  }
}

export function registerCheckpointAgent(pi: ExtensionAPI, manager: () => McpManager | null, sessions: CheckpointSessions): void {
  // A new Pi session starts counting from zero and with nothing reviewed.
  pi.on("session_start", async () => {
    sessions.drop()
  })

  pi.on("session_shutdown", async () => {
    sessions.drop()
  })

  pi.on("agent_end", async (_event, ctx) => {
    const current = manager()
    // No manager is an `off` session or a finished one: nothing may be read or written then.
    if (!current) return
    const { checkpoint, mode } = current.settings
    // A `read-only` session cannot write, so reviewing would pay for a model call whose result must be thrown away.
    if (!checkpoint.enabled || mode !== "full") return
    const state = sessions.for(current)
    if (!state.counter.tick()) return

    const review = interval(current, state, ctx, checkpoint.mode === "blocking")
    // Silent means the agent never waits: the review runs on, and only a failure is shown.
    if (checkpoint.mode === "blocking") await review
    else void review
  })
}

/** One interval review, reporting its own failures: it runs unawaited in `silent` mode, where a throw would be lost. */
async function interval(current: McpManager, state: SessionCheckpoints, ctx: ExtensionContext, visible: boolean): Promise<void> {
  const notify = (message: string, level: "info" | "warning" | "error") => ctx.ui.notify(message, level)
  try {
    // A daemon that could not be reached has been reported by the start, and reviewing would only repeat that.
    if (!(await current.ready)) return
    if (visible) ctx.ui.setStatus(CHECKPOINT_STATUS, "MemCastle: checkpointing...")
    const outcome = await state.review.run(piReviewIo(ctx, current))
    if (visible && outcome.kind !== "busy") notify(`MemCastle: ${describeOutcome(outcome)}`, "info")
  } catch (error) {
    // The session may have ended while the model was thinking, and a failure about it is no longer anyone's to read.
    if (state.manager.session === null) return
    current.report(error, notify)
  } finally {
    if (visible) ctx.ui.setStatus(CHECKPOINT_STATUS, undefined)
  }
}
