// Load something when a page opens, again when the daemon says it changed, and again whenever `refresh` is called
// (the Refresh button).
//
// There is no timer: the dashboard does not poll. With `on`, the page also listens to the daemon's change stream
// (docs/adr/041) and reads again, quietly, a moment after a change it cares about. The stream is an addition: without
// it (signed out, refused, not yet open) the page works as it always did, and the button stays. A failed load keeps
// the last good data and sets `error`, so a blip does not blank the page, and `updatedAt` says how old what is on
// screen is.

import { onBeforeUnmount, onMounted, ref, shallowRef, type Ref, type ShallowRef } from "vue"
import { ApiError } from "../api/client.ts"
import type { EventKind } from "../api/types.ts"
import { useEvents } from "../events.ts"

export interface Loaded<T> {
  data: ShallowRef<T | undefined>
  error: Ref<ApiError | null>
  loading: Ref<boolean>
  /** When the data last arrived; absent until the first answer. */
  updatedAt: Ref<Date | null>
  refresh: () => Promise<void>
}

export interface LoadOptions {
  /** The kinds of change that make this data stale. Without any, the page is read on demand only. */
  on?: readonly EventKind[]
  /** How long a burst of changes is allowed to settle before one re-read. */
  settleMs?: number
}

/** A job reports progress after every unit of work, which can be many times a second; one read per burst is plenty. */
const SETTLE_MS = 250

/** An `ApiError` for any thrown value, so a view can show it without knowing where it came from. */
export function asApiError(error: unknown): ApiError {
  return error instanceof ApiError ? error : new ApiError(0, "unexpected", error instanceof Error ? error.message : String(error))
}

export function useLoad<T>(load: () => Promise<T>, options: LoadOptions = {}): Loaded<T> {
  const data = shallowRef<T>()
  const error = ref<ApiError | null>(null)
  const loading = ref(false)
  const updatedAt = ref<Date | null>(null)

  // Reads never overlap: two in flight can answer out of order and leave the older data on screen. A request made
  // while one is running is folded into one more read straight after it.
  let reading: Promise<void> | undefined
  let again = false

  async function read(): Promise<void> {
    do {
      again = false
      try {
        data.value = await load()
        updatedAt.value = new Date()
        error.value = null
      } catch (caught) {
        error.value = asApiError(caught)
      }
    } while (again)
  }

  /** Read now, or fold into the read in flight; resolves when the data is current as of this call. */
  function ensureRead(): Promise<void> {
    if (reading) {
      again = true
    } else {
      reading = read().finally(() => {
        reading = undefined
      })
    }
    return reading
  }

  async function refresh(): Promise<void> {
    loading.value = true
    try {
      await ensureRead()
    } finally {
      loading.value = false
    }
  }

  /** The same read, without the spinner: a change the user did not ask to see should not make the page flicker. */
  function quietly(): Promise<void> {
    return ensureRead()
  }

  onMounted(refresh)

  const events = options.on?.length ? useEvents() : null
  if (events && options.on) {
    let timer: ReturnType<typeof setTimeout> | undefined
    const unsubscribe = events.subscribe(options.on, () => {
      // Trailing, not leading: the last change of a burst is the one the read must include.
      clearTimeout(timer)
      timer = setTimeout(() => void quietly(), options.settleMs ?? SETTLE_MS)
    })
    onBeforeUnmount(() => {
      clearTimeout(timer)
      unsubscribe()
    })
  }

  return { data, error, loading, updatedAt, refresh }
}
