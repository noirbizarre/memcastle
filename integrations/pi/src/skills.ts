// Making the bundled skills visible to Pi's own skill mechanism, without copying them.
//
// Pi lists the skills it finds in its own directories (`~/.pi/agent/skills`, `~/.agents/skills`, ...) and, for a package,
// the ones the package manifest declares. The skills `memcastle integration install pi` puts in `skills/` beside this
// bundle are neither, and declaring them in the package's `package.json` would expose them in every session, including an
// `off` one, which must behave as if MemCastle did not exist (docs/integration-contract.md). So the extension offers them
// itself, from `resources_discover`, and offers none when the session is `off` (or its mode is unusable): the same
// decision OpenCode's `skills.ts` makes with `skills.paths`.
//
// The files are read where they are, so `skills/` stays the one source. A skill of the same name that the user already
// has wins: Pi keeps the first skill it loads under a name, and resources an extension adds come after Pi's own.

import { existsSync } from "node:fs"
import { fileURLToPath } from "node:url"
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import { InvalidModeError } from "./modes.ts"
import { resolveSettings } from "./settings.ts"
import { SKILLS_DIR } from "./skill-text.ts"

/**
 * The skill directories to offer Pi for a session, in the mode the settings choose.
 *
 * Empty for an `off` session and for a mode that does not parse, which is the fail-closed answer `extension.ts` already
 * reported to the user at `session_start`.
 */
export function skillPaths(
  dir: string = fileURLToPath(SKILLS_DIR),
  env: Record<string, string | undefined> = process.env,
): string[] {
  let mode: string
  try {
    mode = resolveSettings(undefined, env).mode
  } catch (error) {
    if (error instanceof InvalidModeError) return []
    throw error
  }
  // Nothing is exposed by an `off` session, and a directory that is not there (a checkout that moved) is not offered, so
  // Pi has nothing to complain about either.
  if (mode === "off" || !existsSync(dir)) return []
  return [dir]
}

export function registerSkills(pi: ExtensionAPI): void {
  pi.on("resources_discover", async () => {
    const paths = skillPaths()
    return paths.length === 0 ? undefined : { skillPaths: paths }
  })
}
