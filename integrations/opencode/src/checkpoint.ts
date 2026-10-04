// Checkpoints for OpenCode sessions: an interval review, a manual one, and an emergency one before compaction.
//
// OpenCode 1 and 2 reach this through `core.ts`, each supplying a {@link ReviewHost}: how to read a session's
// conversation and how to ask a model a question. Everything else (counting, what a review reads, the payload, the job)
// is `checkpoint-core.ts`, which is the same file Pi uses.
//
// Classification happens here, client-side, never in MemCastle: the daemon stores what it is handed and decides nothing.

import {
  CheckpointReview,
  ExchangeCounter,
  type ClassificationRequest,
  type ModelRef,
  type ReviewIo,
  type Turn,
  describeOutcome,
  submitCheckpoint,
  validatePayload,
} from "./checkpoint-core.ts"
import { MemCastleFailure } from "./failures.ts"
import type { ProjectContext } from "./project-core.ts"
import type { SessionRegistry } from "./registry.ts"
import type { Settings } from "./settings.ts"
import { readSkill } from "./skill-text.ts"

/** The shared skill that says what is worth keeping, used as the reviewing model's instructions. */
export const CHECKPOINT_SKILL = "checkpoint-instructions"

/** The tool the model calls, named as the shared skills name it. */
export const CHECKPOINT_TOOL = "memcastle_checkpoint"

/** The slash command: it asks the model to call the tool, because OpenCode 1 commands are prompt templates. */
export const CHECKPOINT_COMMAND = "memcastle-checkpoint"

/** How long compaction waits for an emergency review before carrying on without it. */
export const EMERGENCY_DEADLINE_MS = 30_000

/** What the host supplies: a session's conversation, and a model to ask. */
export interface ReviewHost {
  /** The words said in OpenCode session `sessionId`, oldest first. */
  transcript(sessionId: string): Promise<readonly Turn[]>
  /**
   * Ask a model `request` and return its reply. `model` is the configured one, or `null` for the session's own.
   * A host that opens a session of its own to ask must call `claim` with its id before anything can run in it, so the
   * plugin's hooks never act on the reviewer: a review that reviewed itself would never end.
   */
  classify(
    sessionId: string,
    request: ClassificationRequest,
    model: ModelRef | null,
    signal: AbortSignal,
    claim: (reviewerSessionId: string) => void,
  ): Promise<string>
}

export type Log = (level: "debug" | "info" | "warn" | "error", message: string) => Promise<void>

/** The arguments of the `memcastle_checkpoint` tool, as OpenCode hands them over. */
export interface CheckpointArgs {
  /** Items the model classified itself. Without it, the plugin reviews the session's conversation. */
  payload?: unknown
  emergency?: boolean
  /** The user's own words on what to keep, when the plugin does the review. */
  note?: string
}

interface SessionState {
  review: CheckpointReview
  counter: ExchangeCounter
}

export interface Checkpoints {
  /** The agent finished a run in `sessionId`: one more exchange to count. */
  sessionIdle(sessionId: string): Promise<void>
  /**
   * `sessionId` is about to be compacted, which loses its transcript: submit what has not been kept, as an emergency.
   * `transcript` is the conversation when the host already holds it, so the plugin need not read it back mid-compaction.
   */
  compacting(sessionId: string | undefined, transcript?: readonly Turn[]): Promise<void>
  /** The `memcastle_checkpoint` tool. Returns what to tell the user, and throws a `MemCastleFailure` otherwise. */
  checkpoint(sessionId: string, args: CheckpointArgs): Promise<string>
  /** The OpenCode session ended: cancel its review and forget its count. */
  forget(sessionId: string): void
  forgetAll(): void
}

export function createCheckpoints(options: {
  settings: Settings
  sessions: SessionRegistry
  /** Sessions that are not the user's conversation: subagents, and the reviewers this plugin opens. */
  children: Set<string>
  host: ReviewHost | undefined
  log: Log
  report: (name: string, error: unknown) => Promise<void>
  /** The project a session works in, or `null`: the wing its `project` and `diary` items default to. */
  projectOf?: (sessionId: string) => ProjectContext | null
  deadlineMs?: number
}): Checkpoints {
  const { settings, sessions, children, host, log, report } = options
  const projectOf = options.projectOf ?? (() => null)
  const deadlineMs = options.deadlineMs ?? EMERGENCY_DEADLINE_MS
  const states = new Map<string, SessionState>()

  const stateOf = (sessionId: string): SessionState => {
    let state = states.get(sessionId)
    if (!state) {
      state = {
        counter: new ExchangeCounter(settings.checkpoint.interval),
        review: new CheckpointReview({
          mode: settings.mode,
          agentIdentity: settings.agentIdentity,
          project: () => projectOf(sessionId),
          // Read when a review submits, so a connection replaced in the meantime is the one used.
          session: () => sessions.session(sessionId),
          skill: () => readSkill(CHECKPOINT_SKILL),
        }),
      }
      states.set(sessionId, state)
    }
    return state
  }

  const ioFor = (sessionId: string, transcript?: readonly Turn[]): ReviewIo => {
    if (!host) throw unavailable()
    return {
      transcript: () => transcript ?? host.transcript(sessionId),
      classify: (request, signal) =>
        host.classify(sessionId, request, settings.checkpoint.model, signal, (reviewer) => children.add(reviewer)),
    }
  }

  /** A review that reports its own failures, because it may run where nobody awaits it. */
  const reviewAndReport = async (name: string, sessionId: string, emergency: boolean, transcript?: readonly Turn[]) => {
    try {
      const outcome = await stateOf(sessionId).review.run(ioFor(sessionId, transcript), { emergency })
      if (outcome.kind !== "busy" && outcome.kind !== "nothing") await log("info", `${name}: ${describeOutcome(outcome)}`)
    } catch (error) {
      // A session that ended while the model was thinking has no one left to tell.
      if (states.has(sessionId)) await report(name, error)
    }
  }

  return {
    async sessionIdle(sessionId) {
      const { checkpoint, mode } = settings
      // A `read-only` session cannot write, so a review would pay for a model call whose result must be thrown away.
      if (!host || !checkpoint.enabled || mode !== "full" || children.has(sessionId)) return
      const state = stateOf(sessionId)
      if (!state.counter.tick()) return
      const review = reviewAndReport("checkpoint", sessionId, false)
      // Silent: the plugin never waits. There is no agent to hold up at idle, so blocking only means this hook does not
      // return until the review has ended, and its result is logged where a failure would be.
      if (checkpoint.mode === "blocking") await review
      else void review
    },

    async compacting(sessionId, transcript) {
      if (!host || sessionId === undefined || settings.mode !== "full" || children.has(sessionId)) return
      stateOf(sessionId).counter.reset()
      const review = reviewAndReport("emergency checkpoint", sessionId, true, transcript)
      // The compaction must not wait on a slow model for ever: the job is Critical priority once queued, and a review
      // still in flight when the deadline passes carries on and reports for itself.
      let timer: ReturnType<typeof setTimeout> | undefined
      const deadline = new Promise<void>((resolve) => (timer = setTimeout(resolve, deadlineMs)))
      await Promise.race([review, deadline])
      clearTimeout(timer)
    },

    async checkpoint(sessionId, args) {
      if (children.has(sessionId)) {
        // The reviewer is told it has no tools, and this is the backstop if it calls one anyway.
        throw new MemCastleFailure(
          "invalid_input",
          "A checkpoint reviewer cannot call this tool.",
          null,
          "The plugin submits what the reviewer replies with.",
        )
      }
      // Refused here, for a payload the model wrote as for a review the plugin would run: a `read-only` session never
      // attempts a write. The daemon would refuse it too, but only after a rejected call a client that already knows the
      // mode has no reason to make, and a review would first have paid for a model call whose result is thrown away.
      if (settings.mode !== "full") {
        throw new MemCastleFailure(
          "mode_rejected",
          `This session is ${settings.mode}, so it cannot write a checkpoint.`,
          "memcastle::app::mode_forbidden",
          "Start the session with MEMCASTLE_MODE=full to checkpoint.",
        )
      }
      const state = stateOf(sessionId)
      if (args.payload !== undefined) {
        // The model classified it itself, so the plugin only checks it and submits it.
        const payload = validatePayload(
          args.payload,
          settings.agentIdentity,
          "Fix the payload and call the tool again.",
          projectOf(sessionId),
        )
        if (payload.items.length === 0) return describeOutcome({ kind: "nothing", reason: "nothing worth keeping" })
        const job = await submitCheckpoint(sessions.session(sessionId), payload, {
          emergency: args.emergency === true,
          wait: args.emergency !== true,
        })
        state.counter.reset()
        const items = payload.items.length
        return describeOutcome(
          job.status === "completed"
            ? { kind: "saved", items: job.result?.items ?? items, duplicates: job.result?.duplicates ?? 0, job }
            : { kind: "queued", items, job },
        )
      }
      const outcome = await state.review.run(ioFor(sessionId), { note: args.note, emergency: args.emergency === true })
      if (outcome.kind !== "busy") state.counter.reset()
      return describeOutcome(outcome)
    },

    forget(sessionId) {
      states.get(sessionId)?.review.abort()
      states.delete(sessionId)
    },

    forgetAll() {
      for (const id of [...states.keys()]) this.forget(id)
    },
  }
}

/** Raised when the host cannot read a conversation or ask a model, which is the case for a bare `createCore`. */
function unavailable(): MemCastleFailure {
  return new MemCastleFailure(
    "unexpected",
    "This host gives the plugin no way to review a conversation, so nothing was checkpointed.",
    null,
    "Pass the checkpoint payload to the tool yourself, or report this as a bug.",
  )
}

/** The slash command's definition for OpenCode 1's `config.command`, which is a prompt template. */
export const CHECKPOINT_COMMAND_DEFINITION = {
  description: "Save what is worth keeping from this conversation to MemCastle now",
  template:
    `Call the \`${CHECKPOINT_TOOL}\` tool once, with no \`payload\` argument. ` +
    "If the text after this sentence is not empty, pass it as the `note` argument. " +
    "Then tell the user what the tool answered, in one sentence, and stop.\n\n$ARGUMENTS",
}

/** Add the slash command to OpenCode 1's configuration, in place, unless the user already defined one by that name. */
export function addCheckpointCommand(config: object): void {
  const target = config as { command?: Record<string, unknown> }
  const commands = (target.command ??= {})
  commands[CHECKPOINT_COMMAND] ??= { ...CHECKPOINT_COMMAND_DEFINITION }
}

/** The tool's arguments from whatever a host hands over, which OpenCode 2 types as `unknown`. */
export function checkpointArgs(input: unknown): CheckpointArgs {
  const value = (typeof input === "object" && input !== null ? input : {}) as Record<string, unknown>
  return {
    payload: value.payload ?? undefined,
    emergency: value.emergency === true,
    note: typeof value.note === "string" ? value.note : undefined,
  }
}
