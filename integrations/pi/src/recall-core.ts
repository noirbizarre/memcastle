// Search-before-answer, without any host: the setting that says how hard to push, and the text a client injects.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like `wake-up-core.ts`, so the two agents
// cannot drift apart on what `sometimes` or `always` mean. MemCastle itself never forces a search: the daemon has no
// such setting, and this one only decides what an integration puts in front of the model.

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
 * The text to inject for `settings`, built from the shared skill's `body`, or `null` when nothing is to be injected.
 * The body is passed through untouched: the tests hold it to the file byte for byte.
 */
export function recallInstruction(settings: RecallSettings, body: string): string | null {
  switch (settings.level) {
    case "off":
      return null
    case "sometimes":
      return body
    case "always":
      return `${body}\n\n${ALWAYS_LINE}`
  }
}
