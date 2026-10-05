// Load something when a page opens, and again whenever `refresh` is called (the Refresh button).
//
// There is no timer: the dashboard does not poll. A failed load keeps the last good data and sets `error`, so a blip
// does not blank the page, and `updatedAt` says how old what is on screen is.

import { onMounted, ref, shallowRef, type Ref, type ShallowRef } from "vue"
import { ApiError } from "../api/client.ts"

export interface Loaded<T> {
  data: ShallowRef<T | undefined>
  error: Ref<ApiError | null>
  loading: Ref<boolean>
  /** When the data last arrived; absent until the first answer. */
  updatedAt: Ref<Date | null>
  refresh: () => Promise<void>
}

/** An `ApiError` for any thrown value, so a view can show it without knowing where it came from. */
export function asApiError(error: unknown): ApiError {
  return error instanceof ApiError ? error : new ApiError(0, "unexpected", error instanceof Error ? error.message : String(error))
}

export function useLoad<T>(load: () => Promise<T>): Loaded<T> {
  const data = shallowRef<T>()
  const error = ref<ApiError | null>(null)
  const loading = ref(false)
  const updatedAt = ref<Date | null>(null)

  async function refresh(): Promise<void> {
    loading.value = true
    try {
      data.value = await load()
      updatedAt.value = new Date()
      error.value = null
    } catch (caught) {
      error.value = asApiError(caught)
    } finally {
      loading.value = false
    }
  }

  onMounted(refresh)
  return { data, error, loading, updatedAt, refresh }
}
