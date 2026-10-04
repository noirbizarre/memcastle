// Making the shared skills visible to OpenCode's own skill mechanism, without copying them.
//
// OpenCode lists skills in its `skill` tool and loads one on demand. It finds them in `.opencode/`, `.claude/` and
// `.agents/` skill directories, and from `skills.paths` in its configuration. The repository's `skills/` directory is
// none of those, so the plugin adds it: OpenCode 1 through the `config` hook (`addSkillsPath`), OpenCode 2 through
// `ctx.skill.transform` (`sharedSkills`). Either way the files are read where they are, so `skills/` stays the one
// source and there is nothing to keep in step.
//
// Every skill in the directory is exposed, which is the same as copying the directory into a client location, as
// docs/skills.md describes.

import { readdir, readFile } from "node:fs/promises"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { SKILLS_DIR, skillBody } from "./skill-text.ts"

/** A skill as OpenCode 2's `Skill.Info` wants it, with plain strings where its type is branded. */
export interface SharedSkill {
  id: string
  name: string
  description: string
  /** Absolute path of the `SKILL.md`, so OpenCode can show where the skill came from. */
  path: string
  /** The body below the frontmatter, which is what the `skill` tool hands the model. */
  content: string
}

/**
 * The `name` and `description` of a `SKILL.md`, or `undefined` when the frontmatter has neither.
 *
 * Skill frontmatter is flat `key: value` lines (`tests/skills.rs` holds the files to that), so a line split is enough,
 * and a description may contain colons because only the first `: ` splits.
 */
export function frontmatterOf(source: string): { name: string; description: string } | undefined {
  const block = /^---\r?\n([\s\S]*?)\r?\n---/.exec(source)?.[1]
  if (block === undefined) return undefined
  const fields = new Map<string, string>()
  for (const line of block.split(/\r?\n/)) {
    const split = line.indexOf(": ")
    // An indented line belongs to the `metadata` map, which OpenCode does not need.
    if (split > 0 && !line.startsWith(" ")) fields.set(line.slice(0, split), line.slice(split + 2).trim())
  }
  const name = fields.get("name")
  const description = fields.get("description")
  return name && description ? { name, description } : undefined
}

/** Every skill under `dir` (the repository's `skills/` by default), in a stable order. */
export async function sharedSkills(dir: string = fileURLToPath(SKILLS_DIR)): Promise<SharedSkill[]> {
  const entries = await readdir(dir, { withFileTypes: true })
  const skills: SharedSkill[] = []
  for (const entry of entries.filter((candidate) => candidate.isDirectory()).sort((a, b) => a.name.localeCompare(b.name))) {
    const path = join(dir, entry.name, "SKILL.md")
    const source = await readFile(path, "utf8").catch(() => undefined)
    const meta = source === undefined ? undefined : frontmatterOf(source)
    // A directory with no readable skill file is not a skill; the repository's own tests catch a malformed one.
    if (source === undefined || meta === undefined) continue
    skills.push({ id: meta.name, name: meta.name, description: meta.description, path, content: skillBody(source) })
  }
  return skills
}

/**
 * OpenCode 1's configuration with `dir` added to `skills.paths`, in place, once.
 *
 * The plugin's `Config` type predates the `skills` key, hence the narrow shape here. An entry the user already
 * configured is left alone and never duplicated, so a reloaded plugin does not grow the list.
 */
export function addSkillsPath(config: object, dir: string): void {
  const target = config as { skills?: { paths?: string[]; urls?: string[] } }
  const skills = (target.skills ??= {})
  const paths = (skills.paths ??= [])
  if (!paths.includes(dir)) paths.push(dir)
}
