// The project context: discovery, precedence, and what each consumer does with it.
//
// This file is the same in `integrations/pi` and `integrations/opencode`, like the code it tests. The resolver is held to
// the daemon's reader of the same file by the shared fixtures, which is what makes the two one contract.

import { afterEach, expect, test } from "bun:test"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { MemCastleFailure } from "../src/failures.ts"
import {
  ProjectConfigError,
  ProjectScopes,
  findProjectFile,
  parseProjectFile,
  resolveProject,
  type ProjectContext,
} from "../src/project-core.ts"
import { classificationRequest, parseClassification, validatePayload } from "../src/checkpoint-core.ts"
import { projectLine, recallInstruction } from "../src/recall-core.ts"
import { DEFAULT_WAKE_UP, wingFor } from "../src/wake-up-core.ts"

interface Case {
  name: string
  files: Record<string, string>
  home?: string
  start: string
  env?: Record<string, string>
  expect?: { name: string | null; wing: string | null; room: string | null } | null
  error?: string
}

const cases: Case[] = JSON.parse(
  readFileSync(new URL("../../../tests/fixtures/project-config/cases.json", import.meta.url), "utf8"),
).cases

const made: string[] = []
afterEach(() => {
  for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true })
})

/** A fresh root with `files` written under it. */
function tree(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "memcastle-project-"))
  made.push(root)
  for (const [path, content] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true })
    writeFileSync(join(root, path), content)
  }
  return root
}

const project = (overrides: Partial<ProjectContext> = {}): ProjectContext => ({ root: null, name: null, wing: null, room: null, ...overrides })

// --- the shared fixtures ---------------------------------------------------------------------------------------

for (const item of cases) {
  test(`fixture: ${item.name}`, () => {
    const root = tree(item.files)
    const home = item.home === undefined ? null : join(root, item.home)
    const run = () => resolveProject(join(root, item.start), item.env ?? {}, home)
    if (item.error !== undefined) {
      expect(run).toThrow(ProjectConfigError)
      expect(run).toThrow(item.error)
      return
    }
    const got = run()
    if (item.expect === null || item.expect === undefined) {
      expect(got).toBeNull()
      return
    }
    expect({ name: got?.name, wing: got?.wing, room: got?.room }).toEqual(item.expect)
  })
}

test("the fixtures were all replayed and cover the environment and every failure", () => {
  expect(cases.length).toBeGreaterThan(20)
  expect(cases.some((c) => c.env !== undefined)).toBe(true)
  expect(cases.some((c) => c.error !== undefined)).toBe(true)
})

// --- discovery and errors --------------------------------------------------------------------------------------

test("the project root is the directory that holds .config, not the directory the session started in", () => {
  const root = tree({ ".config/memcastle.toml": "[memcastle]\nwing = \"w\"\n", "a/b/x": "" })
  expect(resolveProject(join(root, "a/b"), {}, null)?.root).toBe(root)
  expect(findProjectFile(join(root, "a/b"), null)).toBe(join(root, ".config/memcastle.toml"))
})

test("a directory named .config/memcastle.toml is not a project file", () => {
  const root = tree({ ".config/memcastle.toml/inner": "" })
  expect(resolveProject(root, {}, null)).toBeNull()
})

test("a broken file is a failure of class invalid_input that says what to do and names the file", () => {
  const root = tree({ ".config/memcastle.toml": "[memcastle]\ntoken = \"hunter2\"\n" })
  try {
    resolveProject(root, {}, null)
    throw new Error("expected a failure")
  } catch (error) {
    expect(error).toBeInstanceOf(MemCastleFailure)
    expect((error as MemCastleFailure).failureClass).toBe("invalid_input")
    const message = (error as MemCastleFailure).toUserMessage()
    expect(message).toContain(join(root, ".config/memcastle.toml"))
    expect(message).toContain("without a project scope")
  }
})

test("parsing a file never echoes a secret it refused", () => {
  expect(() => parseProjectFile("[memcastle]\ntoken = \"hunter2\"\n", "f")).toThrow(/unknown key `token`/)
  try {
    parseProjectFile("[memcastle]\ntoken = \"hunter2\"\n", "f")
  } catch (error) {
    expect(String(error)).not.toContain("hunter2")
  }
})

test("a scalar where a table belongs is refused", () => {
  expect(() => parseProjectFile("memcastle = 3\n", "f")).toThrow(/must be a table/)
  expect(() => parseProjectFile("[memcastle]\nwing = 3\n", "f")).toThrow(/must be a string/)
})

// --- the per-directory cache -----------------------------------------------------------------------------------

test("a project is resolved once per directory and a broken one is reported once", () => {
  const good = tree({ ".config/memcastle.toml": "[memcastle]\nwing = \"w\"\n" })
  const bad = tree({ ".config/memcastle.toml": "nonsense = 1\n" })
  const errors: unknown[] = []
  const scopes = new ProjectScopes({}, (error) => errors.push(error), null)
  expect(scopes.for(good)?.wing).toBe("w")
  expect(scopes.for(bad)).toBeNull()
  expect(scopes.for(bad)).toBeNull()
  expect(errors).toHaveLength(1)
  // The cache is by directory: editing the file does not change an answer a session already has.
  writeFileSync(join(good, ".config/memcastle.toml"), "[memcastle]\nwing = \"other\"\n")
  expect(scopes.for(good)?.wing).toBe("w")
})

test("a reporter that throws does not turn a handled failure into an unhandled one", () => {
  const bad = tree({ ".config/memcastle.toml": "nonsense = 1\n" })
  const scopes = new ProjectScopes({}, () => {
    throw new Error("notifier is down")
  }, null)
  expect(scopes.for(bad)).toBeNull()
})

test("an invalid environment variable fails every directory, and is reported and ignored", () => {
  const errors: unknown[] = []
  const scopes = new ProjectScopes({ MEMCASTLE_WING: "a/b" }, (error) => errors.push(error), null)
  expect(scopes.for(tree({}))).toBeNull()
  expect(String(errors[0])).toContain("MEMCASTLE_WING")
})

// --- consumers -------------------------------------------------------------------------------------------------

test("wake-up asks about the project's wing in the default source, and about nothing else in the others", () => {
  const p = project({ wing: "castle" })
  expect(wingFor(DEFAULT_WAKE_UP, "/work/dirname", p)).toBe("castle")
  expect(wingFor(DEFAULT_WAKE_UP, "/work/dirname", null)).toBe("dirname")
  expect(wingFor(DEFAULT_WAKE_UP, "/work/dirname", project({ room: "r" }))).toBe("dirname")
  // An explicit client choice outranks the project file.
  expect(wingFor({ ...DEFAULT_WAKE_UP, source: "user" }, "/work/dirname", p)).toBe("preferences")
  expect(wingFor({ ...DEFAULT_WAKE_UP, source: "custom", wing: "mine" }, "/work/dirname", p)).toBe("mine")
  expect(wingFor({ ...DEFAULT_WAKE_UP, source: "none" }, "/work/dirname", p)).toBeUndefined()
})

test("the recall instruction names the project's wing and room, and only the room for search", () => {
  const line = projectLine(project({ wing: "castle", room: "design" }))
  expect(line).toContain("wing `castle`")
  expect(line).toContain("room `design`")
  expect(line).toContain("`memcastle_search` only")
  expect(projectLine(project({ wing: "castle" }))).not.toContain("room")
  expect(projectLine(project())).toBeNull()
  expect(projectLine(null)).toBeNull()
})

test("the recall instruction is the skill untouched when the project names no scope", () => {
  expect(recallInstruction({ level: "sometimes" }, "BODY", null)).toBe("BODY")
  expect(recallInstruction({ level: "sometimes" }, "BODY", project())).toBe("BODY")
  expect(recallInstruction({ level: "off" }, "BODY", project({ wing: "w" }))).toBeNull()
  const scoped = recallInstruction({ level: "always" }, "BODY", project({ wing: "w" }))
  expect(scoped?.startsWith("BODY\n\nThis project's memory is in wing `w`.")).toBe(true)
  expect(scoped).toContain("Override for this session")
})

test("project and diary items with no wing take the project's wing, and the user's own items do not", () => {
  const payload = validatePayload(
    {
      items: [
        { destination: "project", content: "a" },
        { destination: "diary", content: "b" },
        { destination: "preference", content: "c" },
        { destination: "general", content: "d" },
        { destination: "project", content: "e", wing: "chosen" },
      ],
    },
    "agent",
    undefined,
    project({ wing: "castle" }),
  )
  expect(payload.items.map((item) => item.wing)).toEqual(["castle", "castle", null, null, "chosen"])
})

test("without a project wing a checkpoint is exactly what it was before", () => {
  const items = [{ destination: "project", content: "a" }]
  expect(validatePayload({ items }, "agent").items[0]?.wing).toBeNull()
  expect(validatePayload({ items }, "agent", undefined, project({ room: "r" })).items[0]?.wing).toBeNull()
})

test("a reviewer's reply is filed under the project's wing and the reviewer is told about it", () => {
  const p = project({ wing: "castle" })
  const payload = parseClassification('{"items":[{"destination":"project","content":"x","tags":[]}]}', "agent", p)
  expect(payload.items[0]?.wing).toBe("castle")
  expect(classificationRequest("SKILL", [], "note", p).system).toContain("`castle`")
  expect(classificationRequest("SKILL", [], "note").system).not.toContain("belongs to a project")
})
