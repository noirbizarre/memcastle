// The daemon's change notices, shared by every page (docs/adr/041).
//
// One stream per signed-in dashboard, opened by the shell and read by `useLoad`: a page asks to hear about the kinds
// of change that affect what it shows, and re-reads when one arrives. The stream carries identifiers, never content,
// so what a page shows still comes through the routes that apply the memory mode.
//
// It is an improvement and never a dependency: a page loads without it and keeps its Refresh button, so a daemon that
// refuses the stream (no access in this mode), a proxy that buffers it, or a palace another daemon also writes to
// (whose changes this daemon cannot see) all leave the dashboard exactly as it was.

import { inject, reactive, type InjectionKey } from "vue"
import { ApiError, type MemCastleClient } from "./api/client.ts"
import type { DaemonEvent, EventKind } from "./api/types.ts"

/**
 * - `off`: not asked for (signed out, or not started).
 * - `connecting`: opening the stream, or opening it again after a break.
 * - `live`: changes arrive as they happen.
 * - `unavailable`: the daemon refused the stream; pages stay on manual refresh.
 */
export type EventsStatus = "off" | "connecting" | "live" | "unavailable"

export type Listener = (event: DaemonEvent) => void

/** Wait this long before opening the stream again after it broke, doubling up to the ceiling. */
const FIRST_RETRY_MS = 1_000
const LONGEST_RETRY_MS = 30_000

export interface EventsOptions {
  /** Wait before reconnecting. Replaceable, so a test needs no real time. */
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>
}

function wait(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(done, ms)
    function done() {
      clearTimeout(timer)
      signal.removeEventListener("abort", done)
      resolve()
    }
    signal.addEventListener("abort", done)
  })
}

export function createEvents(client: MemCastleClient, options: EventsOptions = {}) {
  const sleep = options.sleep ?? wait
  const state = reactive({ status: "off" as EventsStatus })
  const listeners = new Map<Listener, ReadonlySet<EventKind>>()
  let running: AbortController | undefined

  function emit(event: DaemonEvent): void {
    for (const [listener, kinds] of listeners) {
      // A `resync` is for everyone: events were missed, so whatever a page shows may be stale.
      if (event.kind === "resync" || kinds.has(event.kind)) listener(event)
    }
  }

  async function run(signal: AbortSignal): Promise<void> {
    let retry = FIRST_RETRY_MS
    while (!signal.aborted) {
      state.status = "connecting"
      try {
        for await (const event of client.events(signal)) {
          if (event.kind === "open") {
            state.status = "live"
            retry = FIRST_RETRY_MS
            // Everything that changed before the stream opened (or while it was down) went unheard, so every page
            // reads once more now. This is also what makes a reconnect safe.
            emit({ kind: "resync", action: "updated" })
          } else {
            emit(event as DaemonEvent)
          }
        }
      } catch (caught) {
        if (signal.aborted) return
        // A refusal is not going to change by asking again: the token is gone (the session says so itself), or the
        // memory mode does not allow reading. Anything else (no answer, a dropped connection) is worth another try.
        if (caught instanceof ApiError && (caught.status === 401 || caught.status === 403)) {
          state.status = "unavailable"
          return
        }
      }
      if (signal.aborted) return
      // The daemon closed the stream, or it broke: back to connecting, after a pause that grows while it keeps failing.
      state.status = "connecting"
      await sleep(retry, signal)
      retry = Math.min(retry * 2, LONGEST_RETRY_MS)
    }
  }

  return {
    state,

    /** Open the stream (again, if it is open: the mode or the token it was opened with may have changed). */
    start(): void {
      this.stop()
      running = new AbortController()
      void run(running.signal)
    },

    stop(): void {
      running?.abort()
      running = undefined
      state.status = "off"
    },

    /** Hear about `kinds` of change (and every `resync`) until the returned function is called. */
    subscribe(kinds: readonly EventKind[], listener: Listener): () => void {
      listeners.set(listener, new Set(kinds))
      return () => void listeners.delete(listener)
    },
  }
}

export type Events = ReturnType<typeof createEvents>

export const EVENTS: InjectionKey<Events> = Symbol("memcastle.events")

/** The shell's stream, or nothing outside it (the login page, a test): pages then load on demand only. */
export function useEvents(): Events | null {
  return inject(EVENTS, null)
}
