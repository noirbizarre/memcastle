// The project a session works in, without any host: where `.config/memcastle.toml` is, what it and the environment
// say, and the one context the rest of the integration reads the wing and room from.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like `wake-up-core.ts`, so the two agents
// cannot drift apart on where a project starts or which source wins. It reads the file and the environment itself and
// hands the result to MemCastle as ordinary `wing` / `room` arguments: the daemon never sees the project file here
// (it reads it for one thing only, the default wing of a mined directory), and nothing is asked of it to resolve a project.
//
// The contract (`docs/project-config.md`) is held to the daemon's reader by `tests/fixtures/project-config/cases.json`.

import { existsSync, readFileSync, realpathSync, statSync } from "node:fs"
import { homedir } from "node:os"
import { dirname, join, resolve } from "node:path"
import { parse } from "smol-toml"
import { MemCastleFailure } from "./failures.ts"

/** The file's location under a project root. */
export const PROJECT_FILE = join(".config", "memcastle.toml")

/** What a session's project says about where its memory belongs. Every field is `null` when nothing says. */
export interface ProjectContext {
  /** The directory holding `.config/memcastle.toml`, or `null` when only the environment set a scope. */
  root: string | null
  /** The project's display name, which is also its wing when no wing is set. */
  name: string | null
  wing: string | null
  /** Scopes search only: recall, checkpoint, diary and mining name their rooms themselves. */
  room: string | null
}

type Env = Readonly<Record<string, string | undefined>>

/** A project file or environment variable that cannot be used, with what to do about it. */
export class ProjectConfigError extends MemCastleFailure {
  constructor(message: string, help: string) {
    super("invalid_input", message, null, help)
    this.name = "ProjectConfigError"
  }
}

const FILE_HELP = "MemCastle carries on without a project scope for this session; fix the file, then start a new session."
const ENV_HELP = "MemCastle carries on without a project scope for this session; fix or unset the variable, then start a new session."

/** What the daemon reads as a UUID, which it refuses as a wing or room name because a UUID addresses a record by id. */
const UUID = /^(urn:uuid:)?\{?[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}\}?$/i

/** Why `value` is not a usable wing or room name (the daemon's `validate_name`), or `null` when it is. */
function nameProblem(value: string): string | null {
  if (value === "") return "must not be empty"
  if (value.trim() !== value) return "must not start or end with whitespace"
  if (/\p{Cc}/u.test(value)) return "must not contain control characters"
  if (UUID.test(value)) return "must not look like a UUID, which is reserved for addressing by id"
  if (value.includes("/")) return "must not contain `/`"
  return null
}

/** `value` when it is a table, so a scalar where a section belongs is an error and not an empty section. */
function table(value: unknown, where: string, file: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new ProjectConfigError(`${file} is not valid: ${where} must be a table.`, FILE_HELP)
  }
  return value as Record<string, unknown>
}

/** Refuse any key outside `allowed`, so a typo, or a secret someone pasted in, is an error and never ignored. */
function onlyKeys(section: Record<string, unknown>, allowed: readonly string[], where: string, file: string): void {
  for (const key of Object.keys(section)) {
    if (!allowed.includes(key)) {
      throw new ProjectConfigError(
        `${file} is not valid: unknown key \`${key}\` in ${where}, which accepts ${allowed.length === 0 ? "no keys yet" : allowed.map((a) => `\`${a}\``).join(", ")}.`,
        FILE_HELP,
      )
    }
  }
}

/** A name field of the file, validated, or `null` when absent. */
function fileName(section: Record<string, unknown>, key: string, field: string, file: string): string | null {
  const value = section[key]
  if (value === undefined) return null
  const problem = typeof value === "string" ? nameProblem(value) : "must be a string"
  if (problem !== null) throw new ProjectConfigError(`${file}: ${field} is not usable: it ${problem}.`, FILE_HELP)
  return value as string
}

/** What `.config/memcastle.toml` at `file` declares. Throws {@link ProjectConfigError} for anything the contract refuses. */
export function parseProjectFile(text: string, file: string): { name: string | null; wing: string | null; room: string | null } {
  let parsed: Record<string, unknown>
  try {
    parsed = parse(text)
  } catch (error) {
    throw new ProjectConfigError(`${file} is not valid: ${error instanceof Error ? error.message : String(error)}`, FILE_HELP)
  }
  onlyKeys(parsed, ["project", "memcastle", "mining"], "the file", file)
  const project = table(parsed.project ?? {}, "[project]", file)
  const scope = table(parsed.memcastle ?? {}, "[memcastle]", file)
  const mining = table(parsed.mining ?? {}, "[mining]", file)
  onlyKeys(project, ["name"], "[project]", file)
  onlyKeys(scope, ["wing", "room"], "[memcastle]", file)
  // The reserved table: accepted empty now, so that it can gain keys later without a breaking change.
  onlyKeys(mining, [], "[mining]", file)
  const name = fileName(project, "name", "[project] name", file)
  const wing = fileName(scope, "wing", "[memcastle] wing", file)
  return { name, wing: wing ?? name, room: fileName(scope, "room", "[memcastle] room", file) }
}

/** A path, canonical when it exists, so a symlinked `$HOME` is still recognised as the directory the walk reaches. */
function canonical(path: string): string {
  try {
    return realpathSync(path)
  } catch {
    return resolve(path)
  }
}

/**
 * The project file that governs `start`, or `null`.
 *
 * Walks toward the root and takes the nearest file, so a nested project uses its own and inherits nothing. It never
 * reads `$HOME/.config/memcastle.toml` (that would claim every directory under the home directory) and never goes
 * above the enclosing git root or `$HOME`, so a project cannot be given an unrelated parent's scope.
 */
export function findProjectFile(start: string, home: string | null): string | null {
  let dir = canonical(start)
  const stop = home === null ? null : canonical(home)
  for (;;) {
    if (dir === stop) return null
    const candidate = join(dir, PROJECT_FILE)
    try {
      if (statSync(candidate).isFile()) return candidate
    } catch {
      // No file here: keep walking.
    }
    // `.git` is a directory in a checkout and a file in a worktree or submodule: either marks a project's edge.
    if (existsSync(join(dir, ".git"))) return null
    const parent = dirname(dir)
    if (parent === dir) return null
    dir = parent
  }
}

/** The override `name` carries, validated, or `null` when it is unset or blank. */
function fromEnv(env: Env, name: string): string | null {
  const raw = env[name]?.trim()
  if (raw === undefined || raw === "") return null
  const problem = nameProblem(raw)
  if (problem !== null) throw new ProjectConfigError(`${name}=${JSON.stringify(raw)} is not usable: it ${problem}.`, ENV_HELP)
  return raw
}

/**
 * The project context for a session working in `dir`, or `null` when neither a project file nor the environment
 * names a scope.
 *
 * Per field the order is `MEMCASTLE_WING` / `MEMCASTLE_ROOM`, then the file, then nothing: the environment is for CI,
 * wrappers and temporary overrides, and the file is the project's persistent intent. Neither is an authorization
 * boundary: the daemon decides what a request may touch.
 */
export function resolveProject(dir: string, env: Env = process.env, home: string | null = env.HOME ?? homedir()): ProjectContext | null {
  const wing = fromEnv(env, "MEMCASTLE_WING")
  const room = fromEnv(env, "MEMCASTLE_ROOM")
  const file = findProjectFile(dir, home)
  const declared = file === null ? null : parseProjectFile(readFileSync(file, "utf8"), file)
  if (declared === null && wing === null && room === null) return null
  return {
    root: file === null ? null : dirname(dirname(file)),
    name: declared?.name ?? null,
    wing: wing ?? declared?.wing ?? null,
    room: room ?? declared?.room ?? null,
  }
}

/**
 * The projects of a process's sessions, resolved once per directory (OpenCode serves sessions from several).
 *
 * A project that cannot be read is reported once to `onError` and then behaves as no project at all: a typo in a file
 * must cost a session its scope, never the session itself, and the user is told rather than left guessing.
 */
export class ProjectScopes {
  private readonly known = new Map<string, ProjectContext | null>()

  constructor(
    private readonly env: Env,
    private readonly onError: (error: unknown) => void,
    private readonly home: string | null = env.HOME ?? homedir(),
  ) {}

  /** The context for `dir`, or `null`. Never throws. */
  for(dir: string): ProjectContext | null {
    if (this.known.has(dir)) return this.known.get(dir) ?? null
    let context: ProjectContext | null = null
    try {
      context = resolveProject(dir, this.env, this.home)
    } catch (error) {
      try {
        this.onError(error)
      } catch {
        // Reporting is best effort: a failing notifier must not turn a handled failure into an unhandled one.
      }
    }
    this.known.set(dir, context)
    return context
  }
}
