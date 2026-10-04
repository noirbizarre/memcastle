// How a test points an integration at its daemon, and sets the knobs both integrations read from the environment.
//
// Pi reads only the environment, when its session starts; OpenCode reads plugin options first and the environment
// second, when the plugin loads. Setting the same variables for both is therefore enough, as long as each actor is
// loaded right after its own settings are applied (the loaders below are synchronous with the assignment).

import type { ModeLabel } from "../../../pi/src/modes.ts"
import type { TestDaemon } from "./daemon.ts"

export interface Knobs {
  mode: ModeLabel
  /** Review after every exchange, waiting for it: a test needs the outcome, not a background race. */
  eagerCheckpoint?: boolean
  /** Whether the first model request carries the wake-up. */
  wakeUp?: boolean
  /** Whether requests carry the search-before-answer instruction. */
  recall?: boolean
}

const saved = { ...process.env }

/** Put the environment back to what the developer had, so a test never leaks a mode into the next one. */
export function restoreEnv(): void {
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
}

/** Apply `knobs` for the integration about to be loaded, against `daemon`. */
export function configure(daemon: TestDaemon, knobs: Knobs): void {
  restoreEnv()
  Object.assign(process.env, {
    // Where the daemon's registry file lives, which is how a user's integration finds a daemon too.
    MEMCASTLE_PALACE_PATH: daemon.palacePath,
    HOME: daemon.clientEnv.HOME,
    XDG_STATE_HOME: daemon.clientEnv.XDG_STATE_HOME,
    MEMCASTLE_MODE: knobs.mode,
    // Sync, so the first request carries the briefing and a test does not race the fetch; no wing, so the highlights
    // of every wing come back and a test does not depend on the directory's name.
    MEMCASTLE_WAKE_UP: String(knobs.wakeUp ?? true),
    MEMCASTLE_WAKE_UP_MODE: "sync",
    MEMCASTLE_WAKE_UP_SOURCE: "none",
    MEMCASTLE_FORCE_MEMORY_RECALL: knobs.recall === false ? "off" : "sometimes",
    MEMCASTLE_CHECKPOINT: String(knobs.eagerCheckpoint ?? false),
    MEMCASTLE_CHECKPOINT_INTERVAL: "1",
    MEMCASTLE_CHECKPOINT_MODE: "blocking",
  })
  if (daemon.token) process.env.MEMCASTLE_AUTH_TOKEN = daemon.token
}
