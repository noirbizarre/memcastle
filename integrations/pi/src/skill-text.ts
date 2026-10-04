// Reads a shared skill from the repository's `skills/` directory, so an integration never keeps its own copy of the text.
//
// `skills/<name>/SKILL.md` is the one source (docs/skills.md); this module only decides how that text reaches the model.
// The OpenCode integration carries an identical copy of this file, as it does of `wake-up-core.ts`, because the two
// packages share nothing at runtime (ADR-022).

import { readFile } from "node:fs/promises"

/** The repository's `skills/` directory, relative to this file, the way the tests reach `tests/fixtures`. */
export const SKILLS_DIR = new URL("../../../skills/", import.meta.url)

/**
 * The text of a `SKILL.md` below its frontmatter, trimmed.
 *
 * Only the body is injected: the frontmatter is metadata for a client that discovers skills, and repeating it in
 * every prompt would spend tokens on text the model has no use for.
 */
export function skillBody(source: string): string {
  const match = /^---\r?\n[\s\S]*?\r?\n---\r?\n?/.exec(source)
  return (match ? source.slice(match[0].length) : source).trim()
}

// A skill is read once per process: the file is part of the checkout and does not change under a running agent, and a
// per-turn read would add disk I/O to every prompt.
const cache = new Map<string, Promise<string>>()

/**
 * The body of the shared skill `name`.
 *
 * Rejects when the file is missing, which happens when the package is installed away from the repository checkout.
 * The caller reports that once and carries on without the skill: a missing instruction must not stop a session.
 */
export function readSkill(name: string): Promise<string> {
  let body = cache.get(name)
  if (!body) {
    body = readFile(new URL(`${name}/SKILL.md`, SKILLS_DIR), "utf8").then(skillBody)
    // A failed read is not cached, or fixing the install would need a restart to take effect.
    body.catch(() => cache.delete(name))
    cache.set(name, body)
  }
  return body
}
