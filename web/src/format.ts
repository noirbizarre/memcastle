// Small presentation helpers. Pure, so they are tested without a DOM.

/** `1h 22m`-style span for `seconds`: the two most significant units, and none that is zero. */
export function duration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "-"
  const whole = Math.floor(seconds)
  const days = Math.floor(whole / 86400)
  const hours = Math.floor((whole % 86400) / 3600)
  const minutes = Math.floor((whole % 3600) / 60)
  const secs = whole % 60
  if (days) return hours ? `${days}d ${hours}h` : `${days}d`
  if (hours) return minutes ? `${hours}h ${minutes}m` : `${hours}h`
  if (minutes) return secs ? `${minutes}m ${secs}s` : `${minutes}m`
  return `${secs}s`
}

/** `1h 22m ago`, or `-` for a time that is not there. `now` is a parameter so a test is not racing the clock. */
export function ago(iso: string | null | undefined, now: Date = new Date()): string {
  if (!iso) return "-"
  const seconds = (now.getTime() - new Date(iso).getTime()) / 1000
  if (Number.isNaN(seconds)) return "-"
  return seconds < 5 ? "just now" : `${duration(seconds)} ago`
}

/** How long a job took (or has been running): `-` until it has started. */
export function jobDuration(job: { started_at: string | null; completed_at: string | null }, now: Date = new Date()): string {
  if (!job.started_at) return "-"
  const end = job.completed_at ? new Date(job.completed_at) : now
  return duration((end.getTime() - new Date(job.started_at).getTime()) / 1000)
}

/** A UTC timestamp for a person: `2026-10-05 11:22:51 UTC`. */
export function utc(iso: string | null | undefined): string {
  if (!iso) return "-"
  const date = new Date(iso)
  return Number.isNaN(date.getTime()) ? "-" : `${date.toISOString().slice(0, 19).replace("T", " ")} UTC`
}

/** The first eight characters of an id, which is what the CLI shows. */
export function shortId(id: string): string {
  return id.slice(0, 8)
}

/** What a job is for, in a line: its type and the one parameter that says what it works on. */
export function jobTarget(kind: { type: string; [field: string]: unknown }): string {
  const named = kind.path ?? kind.locator ?? kind.source ?? kind.wing
  return typeof named === "string" && named ? named : "-"
}
