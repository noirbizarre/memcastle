// The language-neutral conformance fixtures, read from the one place every integration and the Rust suite share.
// They are never copied into an integration: a copy is a second source of truth that drifts.

import { readFileSync } from "node:fs"

export function fixture<T = any>(name: string): T {
  const path = new URL(`../../../../tests/fixtures/integration/${name}`, import.meta.url)
  return JSON.parse(readFileSync(path, "utf8")) as T
}

/** Replace every string equal to `{{name}}` with `replacement`, as the Rust suite does for `{{mine_dir}}`. */
export function substitute<T>(value: T, name: string, replacement: string): T {
  if (value === `{{${name}}}`) return replacement as T
  if (Array.isArray(value)) return value.map((item) => substitute(item, name, replacement)) as T
  if (typeof value === "object" && value !== null) {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, substitute(item, name, replacement)]),
    ) as T
  }
  return value
}
