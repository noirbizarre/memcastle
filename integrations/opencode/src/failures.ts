// Failure classes: what went wrong, in the terms the user can act on.
//
// "A daemon that is down is not 'nothing remembered'": every failure is surfaced with its class and, when the
// daemon supplied one, its `help`. Nothing here swallows an error into an empty result.
// The classes mirror `tests/fixtures/integration/failure-classes.json`; `unexpected` is the one addition, for
// the daemon's own faults (HTTP 500 and the like) that the contract does not ask a client to tell apart.

export type FailureClass =
  | "daemon_unavailable"
  | "unauthorized"
  | "mode_rejected"
  | "invalid_input"
  | "job_failed"
  | "unexpected"

/** The error body every MemCastle failure carries, over REST and over MCP alike. */
export interface ErrorBody {
  error: string
  code: string | null
  help: string | null
}

export class MemCastleFailure extends Error {
  constructor(
    readonly failureClass: FailureClass,
    message: string,
    readonly code: string | null = null,
    readonly help: string | null = null,
  ) {
    super(message)
    this.name = "MemCastleFailure"
  }

  /** The message to show the user: the problem, then what to do about it. */
  toUserMessage(): string {
    return this.help ? `${this.message} ${this.help}` : this.message
  }
}

/** Parse an error body, or `null` when the text is not one (a proxy's HTML, a transport's own message). */
export function parseErrorBody(text: string): ErrorBody | null {
  try {
    const value: unknown = JSON.parse(text)
    if (typeof value !== "object" || value === null) return null
    const { error, code, help } = value as Partial<ErrorBody>
    if (typeof error !== "string") return null
    return { error, code: typeof code === "string" ? code : null, help: typeof help === "string" ? help : null }
  } catch {
    return null
  }
}

/**
 * The codes the daemon answers with HTTP 400 for a request the caller got wrong: `memcastle::input::invalid` and the
 * more specific ones. Over MCP a tool error carries no HTTP status, so the code is the only way to tell them apart from
 * a daemon fault. Mirrors `invalid_input` in `tests/fixtures/integration/failure-classes.json` (`code`, `also_codes`).
 */
const INVALID_INPUT_CODES: ReadonlySet<string> = new Set([
  "memcastle::input::invalid",
  "memcastle::palace::invalid_path",
  "memcastle::jobs::invalid_id",
  "memcastle::repair::invalid_based_on_job",
  "memcastle::domain::empty_label",
  "memcastle::search::semantic_unavailable",
])

/** Classify by the public diagnostic code. Codes are stable identifiers; the message text is not. */
function classOfCode(code: string | null, status: number | null): FailureClass {
  if (code === "memcastle::auth::unauthorized" || status === 401) return "unauthorized"
  if (code === "memcastle::app::mode_forbidden" || status === 403) return "mode_rejected"
  if ((code !== null && INVALID_INPUT_CODES.has(code)) || status === 400) return "invalid_input"
  return "unexpected"
}

/** A failure the daemon answered with an error body, over MCP (`status` is `null`) or HTTP. */
export function failureFromBody(body: ErrorBody, status: number | null = null): MemCastleFailure {
  return new MemCastleFailure(classOfCode(body.code, status), body.error, body.code, body.help)
}

/** An HTTP status with no usable body, such as the 401 an MCP transport reports as a bare status. */
export function failureFromStatus(status: number, text: string): MemCastleFailure {
  const body = parseErrorBody(text)
  if (body) return failureFromBody(body, status)
  const failureClass = classOfCode(null, status)
  if (failureClass === "unauthorized") {
    return new MemCastleFailure(
      failureClass,
      "MemCastle refused the request because it needs a token.",
      "memcastle::auth::unauthorized",
      "Set MEMCASTLE_AUTH_TOKEN to the daemon's token.",
    )
  }
  return new MemCastleFailure(failureClass, `MemCastle answered with HTTP ${status}.`, null, text.trim() || null)
}

/** Nothing answered at all. */
export function failureFromTransport(endpoint: string, cause: unknown): MemCastleFailure {
  const detail = cause instanceof Error ? cause.message : String(cause)
  return new MemCastleFailure(
    "daemon_unavailable",
    `MemCastle cannot be reached at ${endpoint} (${detail}).`,
    null,
    "Start it with `memcastle daemon start`, or check it with `memcastle status`.",
  )
}

/** The slice of a job that failure handling needs. */
export interface JobLike {
  id: string
  status: string
  error?: string | null
}

/** A job that reached `failed`, or `null` for any other state. The caller offers `memcastle_job_retry`. */
export function failureFromJob(job: JobLike): MemCastleFailure | null {
  if (job.status !== "failed") return null
  return new MemCastleFailure(
    "job_failed",
    `MemCastle job ${job.id} failed: ${job.error ?? "no error was recorded"}.`,
    null,
    "It can be retried with memcastle_job_retry.",
  )
}
