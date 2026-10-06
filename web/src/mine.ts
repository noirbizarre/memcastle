import type { JobRequest, MineOptions, OptionSpec } from "./api/types.ts"

/** What the Launch form holds for a mine job. */
export interface MineForm {
  kind: "directory" | "source"
  path: string
  source: string | null
  locator: string
  wing: string
  full: boolean
  /** One text per option name the source declares; an empty one is not sent. */
  options: Record<string, string>
}

const orUndefined = (text: string) => text.trim() || undefined

/**
 * The options to send: only the ones the source declares and the user filled in, trimmed.
 *
 * A value typed for one source and left behind after switching to another is not sent, so the daemon never refuses a
 * request for a key the person no longer sees.
 */
export function mineOptions(values: Record<string, string>, accepted: readonly OptionSpec[]): MineOptions | undefined {
  const sent: MineOptions = {}
  for (const spec of accepted) {
    const value = values[spec.name]?.trim()
    if (value) sent[spec.name] = value
  }
  return Object.keys(sent).length ? sent : undefined
}

/** The `POST /api/jobs` body for the form, in the same shape the CLI and MCP send. */
export function mineRequest(form: MineForm, accepted: readonly OptionSpec[]): JobRequest {
  const wing = orUndefined(form.wing)
  const full = form.full || undefined
  const options = mineOptions(form.options, accepted)
  return form.kind === "directory"
    ? { type: "mine", path: form.path.trim(), options, wing, full }
    : { type: "mine", source: form.source ?? "", locator: orUndefined(form.locator), options, wing, full }
}
