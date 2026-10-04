// Wake-up, without any host: the settings, the wing a session asks about, the rendering of the answer, and the
// one in-flight request whose result a session start hands to the first (or a later) model request.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like the MCP session code, so the two agents
// cannot drift apart on what `source` or `mode` mean. It imports nothing from a host, which is also what lets the
// tests drive it with a fake caller and a promise they resolve by hand.

import { basename } from "node:path"
import type { ProjectContext } from "./project-core.ts"

/** `sync` makes the first model request wait for the wake-up; `async` never makes any request wait. */
export type WakeUpMode = "sync" | "async"

/**
 * Which wing the wake-up asks about. The daemon only knows a wing name or no wing at all, so what "user" and
 * "project" mean is this client's decision:
 *   - `user`: the wing checkpoints file preferences under;
 *   - `project`: the wing the project declares (`.config/memcastle.toml` or `MEMCASTLE_WING`), else a wing named after
 *     the working directory, which is what mining a directory creates by default;
 *   - `custom`: the wing the user named;
 *   - `none`: no wing, so highlights from every wing and no diary.
 */
export type WakeUpSource = "user" | "project" | "custom" | "none"

export interface WakeUpSettings {
  /** Whether a session start fetches and injects the wake-up at all. */
  enabled: boolean
  mode: WakeUpMode
  source: WakeUpSource
  /** The wing for `source: "custom"`. Always set when the source is `custom`. */
  wing: string | null
}

export const DEFAULT_WAKE_UP: WakeUpSettings = { enabled: true, mode: "async", source: "project", wing: null }

/** The wing checkpoints file a `preference` under when the checkpoint names none (`src/domain/checkpoint.rs`). */
export const USER_WING = "preferences"

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

/** The member of `allowed` that `value` names, ignoring case, or `null`. */
function oneOf<T extends string>(value: unknown, allowed: readonly T[]): T | null {
  const wanted = text(value)?.toLowerCase()
  return allowed.find((candidate) => candidate === wanted) ?? null
}

/**
 * Resolve the wake-up settings: the `wakeUp` options object first, then `MEMCASTLE_WAKE_UP*`, then the defaults.
 *
 * Lenient on purpose, unlike the memory mode: a mistyped value falls back to its default, because wake-up only ever
 * reads, and a typo must not cost the user their session. `custom` with no wing falls back to `project` for the
 * same reason, since there is no wing to ask about.
 */
export function resolveWakeUp(options: unknown, env: Env): WakeUpSettings {
  const opts = typeof options === "object" && options !== null ? (options as Record<string, unknown>) : {}
  const source = oneOf(opts.source, ["user", "project", "custom", "none"] as const)
    ?? oneOf(env.MEMCASTLE_WAKE_UP_SOURCE, ["user", "project", "custom", "none"] as const)
    ?? DEFAULT_WAKE_UP.source
  const wing = text(opts.wing) ?? text(env.MEMCASTLE_WAKE_UP_WING)
  return {
    enabled: flag(opts.enabled) ?? flag(env.MEMCASTLE_WAKE_UP) ?? DEFAULT_WAKE_UP.enabled,
    mode:
      oneOf(opts.mode, ["sync", "async"] as const)
      ?? oneOf(env.MEMCASTLE_WAKE_UP_MODE, ["sync", "async"] as const)
      ?? DEFAULT_WAKE_UP.mode,
    source: source === "custom" && wing === null ? "project" : source,
    wing: source === "custom" ? wing : null,
  }
}

/** What the daemon reads as a UUID, which it refuses as a wing name because a UUID addresses a record by id. */
const UUID = /^(urn:uuid:)?\{?[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}\}?$/i

/** Make `raw` acceptable as a wing name, or `null` when nothing usable is left (the daemon's `validate_name`). */
export function toWingName(raw: string): string | null {
  // A control character or a `/` is refused outright by the daemon; a directory name may legitimately hold either.
  const name = raw.replace(/\p{Cc}/gu, "").replaceAll("/", "-").trim()
  if (name === "") return null
  return UUID.test(name) ? `project-${name}` : name
}

/**
 * The wing to ask the daemon about for a session working in `cwd`, or `undefined` to ask about no wing.
 * `undefined` is also the answer when `cwd` has no usable name (the filesystem root), because guessing a wing
 * would load another project's context.
 *
 * `project` is the shared project context when it names a wing. Only the default `project` source follows it: `user`,
 * `custom` and `none` are the user's explicit choices for this client, and an explicit choice outranks a project file.
 */
export function wingFor(settings: WakeUpSettings, cwd: string, project: ProjectContext | null = null): string | undefined {
  switch (settings.source) {
    case "user":
      return USER_WING
    case "custom":
      return toWingName(settings.wing ?? "") ?? undefined
    case "project":
      return project?.wing ?? toWingName(basename(cwd)) ?? undefined
    case "none":
      return undefined
  }
}

/** The part of a drawer the briefing quotes. The daemon's drawers carry far more, none of which is injected. */
interface Entry {
  content?: unknown
}

/** The shape of `memcastle_wake_up`'s answer that this client reads. */
export interface WakeUpContext {
  diary?: Entry | null
  recent_highlights?: Entry[] | null
}

/** A drawer's content, or `null` when it has none worth injecting. */
function contentOf(entry: Entry | null | undefined): string | null {
  return typeof entry?.content === "string" && entry.content.trim() !== "" ? entry.content : null
}

/**
 * The text to inject, or `null` for an empty briefing.
 *
 * Contents are quoted verbatim, as `skills/wake-up` asks: the daemon's wording is what the user checkpointed.
 * An empty palace is a normal first session and not an error, so it injects nothing at all, not even a heading.
 */
export function renderWakeUp(context: WakeUpContext): string | null {
  const diary = contentOf(context.diary)
  const highlights = (context.recent_highlights ?? []).map(contentOf).filter((item): item is string => item !== null)
  if (diary === null && highlights.length === 0) return null

  const sections = [
    "# MemCastle wake-up",
    "What earlier sessions remembered about the user and the project. Treat it as established fact: act on it without " +
      "asking again, quote it verbatim, and let the user's present words override it.",
  ]
  if (diary !== null) sections.push(`## Latest diary entry\n\n${diary}`)
  if (highlights.length > 0) sections.push(`## Recent highlights\n\n${highlights.join("\n\n---\n\n")}`)
  return sections.join("\n\n")
}

/** The one thing wake-up needs from an MCP connection. */
export interface Caller {
  call<T = unknown>(tool: string, args?: Record<string, unknown>): Promise<T>
}

/** Ask the daemon for the briefing and render it. Throws whatever the call throws: the caller reports it. */
export async function fetchWakeUp(caller: Caller, agentIdentity: string, wing: string | undefined): Promise<string | null> {
  const context = await caller.call<WakeUpContext>("memcastle_wake_up", {
    agent_identity: agentIdentity,
    // An absent wing is a different question from a wing that does not exist, so it is left out and not sent as null.
    ...(wing === undefined ? {} : { wing }),
  })
  return renderWakeUp(context)
}

/**
 * One wake-up request, started at session start and consumed by whichever model request comes next.
 *
 * It never rejects: a failure goes to `onError` once, and the briefing is then simply absent, so a daemon that is
 * down can neither block the session nor be mistaken for an empty palace (the user was told).
 */
export class PendingWakeUp {
  private text: string | null = null
  private done = false
  private asked = false
  private readonly finished: Promise<void>

  constructor(run: () => Promise<string | null>, onError: (error: unknown) => void) {
    // `Promise.resolve().then` so that a `run` that throws before returning a promise is a failure like any other.
    this.finished = Promise.resolve()
      .then(run)
      .then(
        (rendered) => {
          this.text = rendered
        },
        (error: unknown) => {
          try {
            onError(error)
          } catch {
            // Reporting is best effort: a failing notifier must not turn a handled failure into an unhandled one.
          }
        },
      )
      .finally(() => {
        this.done = true
      })
  }

  /** Whether the request has finished, successfully or not. */
  get settled(): boolean {
    return this.done
  }

  /** Resolves once the request has finished. Never rejects. */
  get whenSettled(): Promise<void> {
    return this.finished
  }

  /**
   * The briefing, if it can be used for this model request, else `null`. It does not consume the briefing: a host
   * whose system prompt is rebuilt every request calls this on each one, and a host that persists the injection
   * keeps its own record of having delivered it.
   *
   * Only the *first* call in `sync` mode waits, and for at most `timeoutMs`. That is what "the first response waits"
   * means: a later request must not stall again because the daemon is slow, so it behaves as `async` does.
   */
  async available(mode: WakeUpMode, timeoutMs: number): Promise<string | null> {
    const first = !this.asked
    this.asked = true
    if (!this.done && first && mode === "sync") {
      let timer: ReturnType<typeof setTimeout> | undefined
      const timeout = new Promise<void>((resolve) => {
        timer = setTimeout(resolve, timeoutMs)
        // A pending timeout must not keep the agent's process alive after everything else has finished.
        timer.unref?.()
      })
      // The timer is cleared as soon as the request wins, or it would hold the event loop for the full timeout.
      await Promise.race([this.finished, timeout]).finally(() => clearTimeout(timer))
    }
    return this.done ? this.text : null
  }
}
