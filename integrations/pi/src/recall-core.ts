// Search-before-answer, without any host: the setting that says how hard to push, and the text a client injects.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like `wake-up-core.ts`, so the two agents
// cannot drift apart on what `sometimes` or `always` mean. MemCastle itself never forces a search: the daemon has no
// such setting, and this one only decides what an integration puts in front of the model.

import type { ProjectContext } from "./project-core.ts"

/**
 * How strongly the client tells the model to search before it answers:
 *   - `off`: nothing is injected, and the model relies on the skill being discoverable (or not at all);
 *   - `sometimes`: the shared skill as written, which asks for a search when a question may depend on the past;
 *   - `always`: the shared skill plus {@link ALWAYS_LINE}, which asks for a search before every answer.
 */
export type RecallLevel = "off" | "sometimes" | "always"

export interface RecallSettings {
  level: RecallLevel
}

export const DEFAULT_RECALL: RecallSettings = { level: "sometimes" }

const LEVELS: readonly RecallLevel[] = ["off", "sometimes", "always"]

/**
 * The one line `always` adds to the shared skill.
 *
 * The skill itself says to skip the search for self-contained questions, which is the opposite of `always`. Editing the
 * shared text would change what every other client reads, so the stronger policy is stated here, once, as an override.
 */
export const ALWAYS_LINE =
  "Override for this session: search MemCastle before every answer, including questions that look self-contained. " +
  "The advice above to skip the search for such questions does not apply."

type Env = Readonly<Record<string, string | undefined>>

/** The level `value` names, ignoring case and surrounding space, or `null`. */
function level(value: unknown): RecallLevel | null {
  const wanted = typeof value === "string" ? value.trim().toLowerCase() : ""
  return LEVELS.find((candidate) => candidate === wanted) ?? null
}

/**
 * Resolve `forceMemoryRecall`: the options object first (`{ level }`, or the bare level), then
 * `MEMCASTLE_FORCE_MEMORY_RECALL`, then the default.
 *
 * Lenient on purpose, like wake-up and unlike the memory mode: a mistyped level falls back to `sometimes`, because the
 * setting only changes how much instruction the model gets and can never write to or expose a palace.
 */
export function resolveForceMemoryRecall(options: unknown, env: Env): RecallSettings {
  const fromOptions =
    typeof options === "object" && options !== null ? level((options as Record<string, unknown>).level) : level(options)
  return { level: fromOptions ?? level(env.MEMCASTLE_FORCE_MEMORY_RECALL) ?? DEFAULT_RECALL.level }
}

/**
 * The line that tells the model which part of the palace this project's memory is in, or `null` when the project names
 * none. The model passes the names on as the `wing` and `room` of its searches: nothing narrows a search for it.
 * A room is only offered for `memcastle_search`, because recall ignores a room.
 */
export function projectLine(project: ProjectContext | null): string | null {
  if (project === null || (project.wing === null && project.room === null)) return null
  const scope = [
    project.wing === null ? null : `wing \`${project.wing}\``,
    project.room === null ? null : `room \`${project.room}\``,
  ].filter((part) => part !== null)
  const passed = [
    project.wing === null ? null : `\`wing\``,
    project.room === null ? null : `\`room\` (to \`memcastle_search\` only)`,
  ].filter((part) => part !== null)
  return (
    `This project's memory is in ${scope.join(", ")}. ` +
    `Pass ${passed.join(" and ")} when you search, unless the question is clearly about something else.`
  )
}

/**
 * The text to inject for `settings`, built from the shared skill's `body`, or `null` when nothing is to be injected.
 * The body is passed through untouched: the tests hold it to the file byte for byte. A project that names a wing or a
 * room adds one line after it.
 */
export function recallInstruction(settings: RecallSettings, body: string, project: ProjectContext | null = null): string | null {
  const line = projectLine(project)
  const scoped = line === null ? body : `${body}\n\n${line}`
  switch (settings.level) {
    case "off":
      return null
    case "sometimes":
      return scoped
    case "always":
      return `${scoped}\n\n${ALWAYS_LINE}`
  }
}
