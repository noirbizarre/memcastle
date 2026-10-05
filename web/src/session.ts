// Who the dashboard is talking as: the daemon's token (when it needs one) and the memory mode every request carries.
//
// The login works like the database endpoint's sign-in (docs/adr/015): the user is the fixed identifier `memcastle`
// and the password is the MemCastle token. There is no account and no new credential, and nothing is checked here:
// the token is sent as `Authorization: Bearer` to `GET /api/status`, which the daemon's authentication layer answers
// like any other call, so a token that is rotated or revoked stops working on the very next request.
//
// The token lives in `sessionStorage`, never in the URL or `localStorage`: closing the tab signs out, and a page from
// another origin cannot read it.

import { inject, reactive, type InjectionKey } from "vue"
import { ApiError, MemCastleClient } from "./api/client.ts"
import type { MemoryMode } from "./api/types.ts"

export const LOGIN_USER = "memcastle"

const TOKEN_KEY = "memcastle.token"
const MODE_KEY = "memcastle.mode"

export interface SessionOptions {
  storage?: Pick<Storage, "getItem" | "setItem" | "removeItem">
  fetch?: typeof fetch
  baseUrl?: string
}

function storedMode(storage: SessionOptions["storage"]): MemoryMode {
  return storage?.getItem(MODE_KEY) === "read_only" ? "read_only" : "full"
}

export function createSession(options: SessionOptions = {}) {
  const storage = options.storage ?? (typeof sessionStorage === "undefined" ? undefined : sessionStorage)
  const state = reactive({
    token: storage?.getItem(TOKEN_KEY) ?? null,
    mode: storedMode(storage) as MemoryMode,
    /** Unknown until the first probe: whether the daemon asks for a token at all. */
    authRequired: null as boolean | null,
    authenticated: false,
    /** Why the user is looking at the login page again, when it is not their own doing. */
    notice: null as string | null,
  })
  let signingIn = false

  function forget(notice: string | null): void {
    state.token = null
    state.authenticated = false
    state.notice = notice
    storage?.removeItem(TOKEN_KEY)
  }

  const client = new MemCastleClient({
    baseUrl: options.baseUrl,
    fetch: options.fetch,
    token: () => state.token,
    mode: () => state.mode,
    // A refused token ends the session, except while the login form is itself testing one.
    onUnauthorized: () => {
      if (!signingIn) {
        state.authRequired = true
        forget("Your token is no longer accepted. It may have been revoked or replaced: sign in again.")
      }
    },
  })

  return {
    state,
    client,

    /**
     * Find out whether the daemon wants a token and whether the one held is good, with the one request every page makes
     * anyway. Answers with the status, so the first page does not ask twice.
     *
     * @throws ApiError for anything but a refusal (the daemon is unreachable, say): that is not a login problem.
     */
    async establish(): Promise<void> {
      try {
        const status = await client.status()
        state.authRequired = status.auth_enabled
        state.authenticated = true
        state.notice = null
      } catch (error) {
        if (!(error instanceof ApiError) || !error.unauthorized) throw error
        state.authRequired = true
        // A held token the daemon refused was already dropped by `onUnauthorized`.
        state.authenticated = false
      }
    },

    /** Sign in with the token as the password. A refusal is thrown and nothing is stored. */
    async signIn(user: string, token: string): Promise<void> {
      // The user is an identifier, not an account (docs/adr/015): anything else is a typo worth saying so.
      if (user.trim() !== LOGIN_USER) throw new ApiError(401, "memcastle::auth::unauthorized", "The user or password was not accepted.")
      signingIn = true
      try {
        await client.status(token.trim())
      } catch (error) {
        if (error instanceof ApiError && error.unauthorized) {
          // The same sentence as the database endpoint's, which never says which half was wrong or echoes the token.
          throw new ApiError(401, error.code, "The user or password was not accepted.")
        }
        throw error
      } finally {
        signingIn = false
      }
      state.token = token.trim()
      state.authRequired = true
      state.authenticated = true
      state.notice = null
      storage?.setItem(TOKEN_KEY, state.token)
    },

    signOut(): void {
      forget(null)
    },

    setMode(mode: MemoryMode): void {
      state.mode = mode
      storage?.setItem(MODE_KEY, mode)
    },
  }
}

export type Session = ReturnType<typeof createSession>

export const SESSION: InjectionKey<Session> = Symbol("memcastle.session")

let shared: Session | undefined

/** The page's one session, created on first use. */
export function defaultSession(): Session {
  shared ??= createSession()
  return shared
}

export function useSession(): Session {
  return inject(SESSION, defaultSession, true)
}
