// Checkpointing, without any host: the settings, the review that turns a conversation into a checkpoint payload, and the
// submission of that payload to MemCastle.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like `wake-up-core.ts`, so the two agents
// cannot drift apart on what a checkpoint item is or when a failed one is reported. It imports nothing from a host:
// the host supplies the transcript and a function that asks its own model a question, and this file does the rest.
//
// The architectural rule it serves: MemCastle never classifies. What is worth keeping, and under which destination, is
// decided here, client-side, by the agent's own model, and MemCastle only stores what it is handed.

import { MemCastleFailure, failureFromJob } from "./failures.ts"
import type { ModeLabel } from "./modes.ts"
import type { ProjectContext } from "./project-core.ts"

// --- settings --------------------------------------------------------------------------------------------------

/**
 * Whether a review blocks the agent. Unrelated to the memory mode (`full`, `read-only`, `off`), which is why the
 * setting lives under `checkpoint` and is never read as a mode:
 *   - `silent`: the review runs in the background and only a failure is shown;
 *   - `blocking`: the review is visible and the agent waits for its result.
 */
export type CheckpointMode = "silent" | "blocking"

/** A model named as the integration's own agent names it. */
export interface ModelRef {
  provider: string
  id: string
}

export interface CheckpointSettings {
  /** Whether the interval review runs. The manual checkpoint works whatever this says: asking for it is the opt-in. */
  enabled: boolean
  /** How many exchanges (a prompt and the agent's answer to it) separate two interval reviews. */
  interval: number
  mode: CheckpointMode
  /** The model that reviews the conversation, or `null` for the one the session is already using. */
  model: ModelRef | null
}

export const DEFAULT_CHECKPOINT: CheckpointSettings = { enabled: true, interval: 10, mode: "silent", model: null }

type Env = Readonly<Record<string, string | undefined>>

/** A non-blank string, or `null`. */
function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value.trim() : null
}

/** A boolean written as a boolean or as a word an environment variable would use; anything else is `null`. */
function flag(value: unknown): boolean | null {
  if (typeof value === "boolean") return value
  switch (text(value)?.toLowerCase()) {
    case "true":
    case "1":
    case "yes":
    case "on":
      return true
    case "false":
    case "0":
    case "no":
    case "off":
      return false
    default:
      return null
  }
}

/** A whole number of at least one, written as a number or a string, or `null`. */
function count(value: unknown): number | null {
  const n = typeof value === "number" || typeof value === "string" ? Number(value) : Number.NaN
  return Number.isInteger(n) && n >= 1 && value !== "" ? n : null
}

/** `provider/id` (the id may itself contain slashes), or `{ provider, id }`; anything else is `null`. */
function model(value: unknown): ModelRef | null {
  if (typeof value === "object" && value !== null) {
    const { provider, id } = value as Record<string, unknown>
    const [p, i] = [text(provider), text(id)]
    return p !== null && i !== null ? { provider: p, id: i } : null
  }
  const written = text(value)
  const slash = written?.indexOf("/") ?? -1
  if (written === null || slash < 1 || slash === written.length - 1) return null
  return { provider: written.slice(0, slash), id: written.slice(slash + 1) }
}

/**
 * Resolve the checkpoint settings: the `checkpoint` options object first, then `MEMCASTLE_CHECKPOINT*`, then the
 * defaults.
 *
 * Lenient on purpose, like wake-up and unlike the memory mode: a mistyped value falls back to its default, because the
 * worst a typo can do here is review a little more or less often. What a review may write is bounded by the session's
 * memory mode, which is strict.
 */
export function resolveCheckpoint(options: unknown, env: Env): CheckpointSettings {
  const opts = typeof options === "object" && options !== null ? (options as Record<string, unknown>) : {}
  const mode = (value: unknown) => {
    const wanted = text(value)?.toLowerCase()
    return wanted === "silent" || wanted === "blocking" ? wanted : null
  }
  return {
    enabled: flag(opts.enabled) ?? flag(env.MEMCASTLE_CHECKPOINT) ?? DEFAULT_CHECKPOINT.enabled,
    interval: count(opts.interval) ?? count(env.MEMCASTLE_CHECKPOINT_INTERVAL) ?? DEFAULT_CHECKPOINT.interval,
    mode: mode(opts.mode) ?? mode(env.MEMCASTLE_CHECKPOINT_MODE) ?? DEFAULT_CHECKPOINT.mode,
    model: model(opts.model) ?? model(env.MEMCASTLE_CHECKPOINT_MODEL),
  }
}

// --- counting --------------------------------------------------------------------------------------------------

/** Counts exchanges and says when an interval review is due. */
export class ExchangeCounter {
  private seen = 0

  constructor(private readonly interval: number) {}

  /** Record one finished exchange; `true` when this one completes an interval. */
  tick(): boolean {
    this.seen += 1
    if (this.seen < this.interval) return false
    this.seen = 0
    return true
  }

  /** Start counting again, after a review of any kind has covered what came before. */
  reset(): void {
    this.seen = 0
  }
}

// --- the transcript --------------------------------------------------------------------------------------------

/** One side of a conversation, reduced to the words that were said. Tool calls and their output are left out. */
export interface Turn {
  role: "user" | "assistant"
  text: string
}

/** The text in message content, which is a string or a list of blocks of which only the `text` ones are speech. */
export function textOf(content: unknown): string {
  if (typeof content === "string") return content.trim()
  if (!Array.isArray(content)) return ""
  return content
    .map((block: unknown) => {
      const { type, text: words } = (block ?? {}) as { type?: unknown; text?: unknown }
      return type === "text" && typeof words === "string" ? words : ""
    })
    .filter((words) => words.trim() !== "")
    .join("\n")
    .trim()
}

/** A turn for `role` and `content`, or `null` for a role that is not a participant or a message with no words. */
export function turnOf(role: unknown, content: unknown): Turn | null {
  if (role !== "user" && role !== "assistant") return null
  const words = textOf(content)
  return words === "" ? null : { role, text: words }
}

/** The most text a review reads. A conversation is cut from its oldest end: recent turns are what is not yet kept. */
export const MAX_TRANSCRIPT_CHARS = 24_000

/** The turns as the plain text the reviewing model reads. */
export function renderTranscript(turns: readonly Turn[], maxChars: number = MAX_TRANSCRIPT_CHARS): string {
  const lines = turns.map((turn) => `${turn.role === "user" ? "User" : "Assistant"}: ${turn.text}`)
  const kept: string[] = []
  let used = 0
  for (const line of [...lines].reverse()) {
    // Always keep the newest turn, even when it alone is over the limit: an empty review would be worse than a long one.
    if (kept.length > 0 && used + line.length > maxChars) break
    kept.push(line)
    used += line.length
  }
  const omitted = lines.length - kept.length
  const body = [...kept].reverse().join("\n\n")
  return omitted > 0 ? `(${omitted} earlier turn(s) left out)\n\n${body}` : body
}

// --- classification --------------------------------------------------------------------------------------------

/** What the reviewing model is asked, and under which instructions. */
export interface ClassificationRequest {
  system: string
  prompt: string
}

/**
 * The reply format. The shared skill explains what is worth keeping and how to word it, but it is written for an agent
 * that calls `memcastle_checkpoint` itself, so this suffix turns that into a reply the client can parse.
 */
const REPLY_FORMAT = `You are reviewing a conversation on behalf of the agent that held it, and you have no tools.
Do not call \`memcastle_checkpoint\`: the client submits what you reply with.
Reply with exactly one JSON object and nothing else: the \`payload\` described above, \`{"items": [...]}\`.
Each item has \`destination\` (\`preference\`, \`project\`, \`diary\` or \`general\`), \`content\`, and \`tags\` (a list, possibly empty), and may have \`wing\` and \`name\`.
Leave out \`source\` and \`fact\`: the client sets them.
When nothing in the conversation is worth keeping, reply \`{"items": []}\`.`

/** The destinations whose items a project's wing is the default for: its own notes and its own diary, not the user's. */
const PROJECT_WING_DESTINATIONS: ReadonlySet<string> = new Set(["project", "diary"])

/**
 * What the reviewer is told about the project's wing, or `null` when the project names none.
 * The client applies the default itself afterwards, so this only keeps the reviewer from inventing another wing.
 */
function projectWingNote(project: ProjectContext | null): string | null {
  if (project?.wing == null) return null
  return `This conversation belongs to a project whose wing is \`${project.wing}\`. Leave \`wing\` out of \`project\` and \`diary\` items: the client files them there.`
}

/** The request that asks a model what in `turns` is worth keeping. `note` is the user's own words on a manual save. */
export function classificationRequest(
  skill: string,
  turns: readonly Turn[],
  note?: string,
  project: ProjectContext | null = null,
): ClassificationRequest {
  const asked = note?.trim()
  const projectNote = projectWingNote(project)
  return {
    system: `${skill}\n\n---\n\n${REPLY_FORMAT}${projectNote === null ? "" : `\n${projectNote}`}`,
    prompt:
      (asked ? `The user asked for this checkpoint and said: ${asked}\n\n` : "") +
      `Review this conversation and reply with the payload.\n\n<conversation>\n${renderTranscript(turns)}\n</conversation>`,
  }
}

const DESTINATIONS = ["preference", "project", "diary", "general"] as const

/** One checkpoint item as MemCastle takes it (`src/domain/checkpoint.rs`). */
export interface CheckpointItem {
  destination: (typeof DESTINATIONS)[number]
  wing: string | null
  name: string | null
  content: string
  tags: string[]
  source: { kind: "manual"; uri: null; agent: string }
  fact: null
}

export interface CheckpointPayload {
  items: CheckpointItem[]
}

/** A reply or payload that cannot become a checkpoint, with what is wrong and what to do about it. */
function unusable(problem: string, help: string): MemCastleFailure {
  return new MemCastleFailure("invalid_input", `The checkpoint was not submitted: ${problem}.`, null, help)
}

const MODEL_HELP =
  "Nothing was saved. Try again, or choose a more capable model for it with the `checkpoint.model` setting."

/**
 * Check `value` and turn it into the payload MemCastle takes, stamped with the agent identity.
 *
 * An item with no wing of its own, headed for `project` or `diary`, takes the project's wing: the client applies it
 * here, at submission, because a checkpoint is a durable job and replaying it must not depend on what a file says by
 * then. A `preference` or `general` item is the user's and is never moved into a project's wing.
 *
 * Validation happens here as well as in the daemon because the daemon refuses a whole payload for one bad item, and a
 * reply from a model is the likeliest place to find one. `source` and `fact` are never taken from the input: the source
 * is this client's to state, and a fact needs entity and relationship ids that no MCP tool lets a client obtain.
 */
export function validatePayload(
  value: unknown,
  agent: string,
  help: string = MODEL_HELP,
  project: ProjectContext | null = null,
): CheckpointPayload {
  const items = (value as { items?: unknown } | null)?.items
  if (!Array.isArray(items)) throw unusable("it has no `items` list", help)
  return {
    items: items.map((raw: unknown, index): CheckpointItem => {
      const item = (typeof raw === "object" && raw !== null ? raw : {}) as Record<string, unknown>
      const where = `item ${index + 1}`
      const destination = DESTINATIONS.find((candidate) => candidate === text(item.destination)?.toLowerCase())
      if (!destination) {
        throw unusable(`${where} has destination ${JSON.stringify(item.destination)}, which is not one of ${DESTINATIONS.join(", ")}`, help)
      }
      const content = text(item.content)
      if (content === null) throw unusable(`${where} has no content`, help)
      const tags = Array.isArray(item.tags) ? item.tags.filter((tag): tag is string => typeof tag === "string" && tag.trim() !== "") : []
      return {
        destination,
        wing: text(item.wing) ?? (PROJECT_WING_DESTINATIONS.has(destination) ? (project?.wing ?? null) : null),
        name: text(item.name),
        content,
        tags: tags.map((tag) => tag.trim()),
        source: { kind: "manual", uri: null, agent },
        fact: null,
      }
    }),
  }
}

/** The JSON a model's reply carries: bare, in a code fence, or inside prose, which models do whatever they are told. */
function jsonIn(reply: string): unknown {
  const trimmed = reply.trim()
  const candidates = [trimmed]
  const fenced = /```(?:json)?\s*([\s\S]*?)```/i.exec(trimmed)
  if (fenced?.[1]) candidates.push(fenced[1].trim())
  const [first, last] = [trimmed.indexOf("{"), trimmed.lastIndexOf("}")]
  if (first >= 0 && last > first) candidates.push(trimmed.slice(first, last + 1))
  for (const candidate of candidates) {
    try {
      return JSON.parse(candidate)
    } catch {
      // Try the next reading of the reply.
    }
  }
  throw unusable("the model's reply was not JSON", MODEL_HELP)
}

/** The payload a model's `reply` describes. Throws a `MemCastleFailure` of class `invalid_input` when it is unusable. */
export function parseClassification(reply: string, agent: string, project: ProjectContext | null = null): CheckpointPayload {
  return validatePayload(jsonIn(reply), agent, MODEL_HELP, project)
}

// --- submission ------------------------------------------------------------------------------------------------

/** The slice of an MCP session a checkpoint needs. */
export interface Caller {
  call<T = unknown>(tool: string, args?: Record<string, unknown>): Promise<T>
}

/** The slice of a job that a checkpoint reports on. */
export interface CheckpointJob {
  id: string
  status: string
  error?: string | null
  result?: { items?: number; duplicates?: number } | null
}

const TERMINAL = new Set(["completed", "failed", "cancelled"])

export interface SubmitOptions {
  /** Jumps the queue (priority Critical rather than High): only for context that is about to be lost. */
  emergency?: boolean
  /** Poll the job until it ends, so a failure is known. Without it the queued job is returned at once. */
  wait?: boolean
  /** How long to poll before giving up and returning the job as it is. */
  waitMs?: number
  pollMs?: number
}

/**
 * Submit `payload` as a checkpoint job and, with `wait`, watch it to its end.
 *
 * @throws MemCastleFailure when the daemon refuses the payload, or when the job failed or was cancelled.
 *   The job of a failure can be retried with `memcastle_job_retry`, which the message says.
 */
export async function submitCheckpoint(
  session: Caller,
  payload: CheckpointPayload,
  options: SubmitOptions = {},
): Promise<CheckpointJob> {
  const { emergency = false, wait = true, waitMs = 30_000, pollMs = 200 } = options
  let job = await session.call<CheckpointJob>("memcastle_checkpoint", { payload, emergency })
  const deadline = Date.now() + waitMs
  while (wait && !TERMINAL.has(job.status) && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, pollMs))
    job = await session.call<CheckpointJob>("memcastle_job_get", { id: job.id })
  }
  const failure = failureFromJob(job)
  if (failure) throw failure
  if (job.status === "cancelled") {
    throw new MemCastleFailure(
      "job_failed",
      `MemCastle job ${job.id} was cancelled before it saved the checkpoint.`,
      null,
      "It can be retried with memcastle_job_retry.",
    )
  }
  return job
}

// --- the review ------------------------------------------------------------------------------------------------

/** What a review did. A failure is thrown, never returned: it is not a kind of outcome. */
export type ReviewOutcome =
  | { kind: "nothing"; reason: "no new exchanges" | "nothing worth keeping" }
  | { kind: "busy" }
  | { kind: "queued"; items: number; job: CheckpointJob }
  | { kind: "saved"; items: number; duplicates: number; job: CheckpointJob }

/** What the host supplies for one review: the conversation, and a way to ask its own model a question. */
export interface ReviewIo {
  /** A snapshot, not a live list: the conversation grows while the model thinks, and those turns are not yet reviewed. */
  transcript(): Promise<readonly Turn[]> | readonly Turn[]
  classify(request: ClassificationRequest, signal: AbortSignal): Promise<string>
}

export interface ReviewOptions {
  /** Context that is about to be lost: submitted at Critical priority, and not waited for. */
  emergency?: boolean
  /** The user's own words on a manual checkpoint. */
  note?: string
  /** Watch the job to its end. Defaults to yes, except for an emergency, which must not hold up what it precedes. */
  wait?: boolean
}

/** A sentence for the user about what a review did. */
export function describeOutcome(outcome: ReviewOutcome): string {
  switch (outcome.kind) {
    case "busy":
      return "A checkpoint is already being written."
    case "nothing":
      return outcome.reason === "no new exchanges"
        ? "Nothing new to checkpoint since the last one."
        : "Reviewed the conversation: nothing worth keeping."
    case "queued":
      return `Queued a checkpoint of ${outcome.items} item(s) (job ${outcome.job.id}).`
    case "saved": {
      const duplicates = outcome.duplicates > 0 ? `, ${outcome.duplicates} already known` : ""
      return `Checkpointed ${outcome.items} item(s)${duplicates}.`
    }
  }
}

/**
 * The reviews of one session: at most one at a time, each covering only what the last one did not.
 *
 * It holds no host object. The session, the skill and the host's own model reach it through the constructor and
 * {@link ReviewIo}, which is what lets the tests drive it with a fake caller and a reply they write by hand.
 */
export class CheckpointReview {
  /** How many turns the last successful review covered, so the next one reads only what came after. */
  private reviewed = 0
  private running: Promise<unknown> | null = null
  private controller: AbortController | null = null

  constructor(
    private readonly context: {
      mode: ModeLabel
      agentIdentity: string
      /** The project the session works in, read when a review runs; `null` or absent when it names no scope. */
      project?: () => ProjectContext | null
      /** The connection, or `null` when there is none. Read when a review submits, not when it starts. */
      session: () => Caller | null
      /** The body of the shared `checkpoint-instructions` skill. */
      skill: () => Promise<string>
      waitMs?: number
      pollMs?: number
    },
  ) {}

  /** Whether a review is in progress. */
  get busy(): boolean {
    return this.running !== null
  }

  /** Cancel the review in progress, if any: the session it belongs to is over. */
  abort(): void {
    this.controller?.abort()
  }

  /**
   * Review the conversation since the last review and submit what is worth keeping.
   *
   * @throws MemCastleFailure for every failure: a refusal by the memory mode, an unusable reply, a daemon that cannot
   *   be reached or a job that failed. Nothing is swallowed into "nothing worth keeping".
   */
  async run(io: ReviewIo, options: ReviewOptions = {}): Promise<ReviewOutcome> {
    // Refused before any model call is paid for. The daemon would refuse the write too, but only after the review.
    if (this.context.mode !== "full") {
      throw new MemCastleFailure(
        "mode_rejected",
        `This session is ${this.context.mode}, so it cannot write a checkpoint.`,
        "memcastle::app::mode_forbidden",
        "Start the session with MEMCASTLE_MODE=full to checkpoint.",
      )
    }
    const previous = this.running
    if (previous) {
      // An emergency is the last chance to keep this context, so it waits its turn rather than being dropped as a duplicate.
      if (!options.emergency) return { kind: "busy" }
      await previous.catch(() => undefined)
    }
    const review = this.review(io, options)
    this.running = review
    try {
      return await review
    } finally {
      // Only the review that is still the current one clears the flag, or an emergency's end would hide a later review.
      if (this.running === review) {
        this.running = null
        this.controller = null
      }
    }
  }

  private async review(io: ReviewIo, options: ReviewOptions): Promise<ReviewOutcome> {
    const all = await io.transcript()
    // A transcript shorter than what was reviewed was replaced (compaction, a new branch), so it is read from its start.
    const from = this.reviewed > all.length ? 0 : this.reviewed
    const turns = all.slice(from)
    if (turns.length === 0 && !options.note?.trim()) return { kind: "nothing", reason: "no new exchanges" }

    const controller = new AbortController()
    this.controller = controller
    const project = this.context.project?.() ?? null
    const reply = await io.classify(classificationRequest(await this.context.skill(), turns, options.note, project), controller.signal)
    const payload = parseClassification(reply, this.context.agentIdentity, project)
    if (payload.items.length === 0) {
      this.reviewed = all.length
      return { kind: "nothing", reason: "nothing worth keeping" }
    }

    // Read now, not when the review started: the session may have been replaced while the model was thinking.
    const session = this.context.session()
    if (!session) throw unusable("MemCastle is not connected", "Start it with `memcastle daemon start`.")
    const wait = options.wait ?? !options.emergency
    const job = await submitCheckpoint(session, payload, {
      emergency: options.emergency,
      wait,
      waitMs: this.context.waitMs,
      pollMs: this.context.pollMs,
    })
    // Advanced once the daemon has the items, so a review that fails before this point is retried over the same turns.
    this.reviewed = all.length
    if (job.status === "completed") {
      return { kind: "saved", items: job.result?.items ?? payload.items.length, duplicates: job.result?.duplicates ?? 0, job }
    }
    return { kind: "queued", items: payload.items.length, job }
  }
}
