import { afterEach, expect, spyOn, test } from "bun:test"
import { readFileSync } from "node:fs"
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import type { PluginInput } from "@opencode-ai/plugin"
import type { Plugin } from "@opencode/plugin"
import { type Core, createCore, type Level } from "../src/core.ts"
import plugin from "../src/index.ts"
import { ALWAYS_LINE } from "../src/recall-core.ts"
import { addSkillsPath, frontmatterOf, sharedSkills } from "../src/skills.ts"
import { skillBody } from "../src/skill-text.ts"

/** The repository's `skills/`, read here independently of the code under test. */
const REPO_SKILLS = new URL("../../../skills/", import.meta.url)
const shared = (name: string) => readFileSync(new URL(`${name}/SKILL.md`, REPO_SKILLS), "utf8")
const SEARCH = skillBody(shared("search-before-answer"))
const CHECKPOINT = skillBody(shared("checkpoint-instructions"))

const saved = { ...process.env }
afterEach(() => {
  for (const key of Object.keys(process.env)) if (!(key in saved)) delete process.env[key]
  Object.assign(process.env, saved)
})

/** A core whose wake-up is off, so a request carries the reminder and nothing else. */
async function coreWith(options: Record<string, unknown> = {}) {
  const logs: { level: Level; message: string }[] = []
  const core = (await createCore(
    { wakeUp: { enabled: false }, ...options },
    async (level, message) => void logs.push({ level, message }),
    {},
    "/work/started-here",
  )) as Core
  const request = async (sessionId: string | undefined) => {
    const system: string[] = []
    await core.systemTransform(sessionId, (text) => system.push(text))
    return system
  }
  return { core, logs, request }
}

// --- the reminder ---------------------------------------------------------------------------------------------

test("every model request carries the shared skill's body byte for byte, without its frontmatter", async () => {
  const { request } = await coreWith()
  expect(await request("ses_1")).toEqual([SEARCH])
  expect(await request("ses_1")).toEqual([SEARCH])
  expect(SEARCH).not.toContain("memcastle-version")
})

test("always adds the one override line to the unchanged shared text, and sometimes does not", async () => {
  expect(await (await coreWith({ forceMemoryRecall: { level: "always" } })).request("ses_1")).toEqual([
    `${SEARCH}\n\n${ALWAYS_LINE}`,
  ])
  expect(await (await coreWith({ forceMemoryRecall: { level: "sometimes" } })).request("ses_1")).toEqual([SEARCH])
})

test("a level of off injects nothing", async () => {
  expect(await (await coreWith({ forceMemoryRecall: { level: "off" } })).request("ses_1")).toEqual([])
})

test("the level can come from the environment variable Pi reads, so the two agents are configured alike", async () => {
  const core = (await createCore({ wakeUp: { enabled: false } }, async () => undefined, {
    MEMCASTLE_FORCE_MEMORY_RECALL: "always",
  })) as Core
  const system: string[] = []
  await core.systemTransform("ses_1", (text) => system.push(text))
  expect(system).toEqual([`${SEARCH}\n\n${ALWAYS_LINE}`])
})

test("a project that names a wing and a room adds one line telling the model to pass them to its searches", async () => {
  const root = mkdtempSync(join(tmpdir(), "memcastle-oc-recall-"))
  try {
    mkdirSync(join(root, ".config"), { recursive: true })
    writeFileSync(join(root, ".config/memcastle.toml"), '[memcastle]\nwing = "castle"\nroom = "design"\n')
    const core = (await createCore({ wakeUp: { enabled: false } }, async () => undefined, { HOME: "/nonexistent-home" }, root)) as Core
    const system: string[] = []
    await core.systemTransform("ses_1", (text) => system.push(text))
    expect(system).toHaveLength(1)
    expect(system[0]?.startsWith(`${SEARCH}\n\nThis project's memory is in wing \`castle\`, room \`design\`.`)).toBe(true)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("a subagent's request is left alone, and a request with no session id still gets the reminder", async () => {
  const { core, request } = await coreWith()
  await core.sessionCreated("ses_child", "/work/castle", "ses_parent")
  expect(await request("ses_child")).toEqual([])
  expect(await request(undefined)).toEqual([SEARCH])
})

test("the reminder does not depend on the wake-up: a failing one still leaves the reminder in place", async () => {
  const { core, logs, request } = await coreWith({ wakeUp: { enabled: true, mode: "sync" }, timeoutMs: 50 })
  // No daemon is reachable, so the wake-up fails and is reported, and the request must still get its reminder.
  process.env.MEMCASTLE_PORT = "1"
  await core.sessionCreated("ses_1", "/work/castle")
  expect(await request("ses_1")).toEqual([SEARCH])
  expect(logs.some((entry) => entry.level === "warn")).toBe(true)
})

// --- the two integrations stay the same ---------------------------------------------------------------------------

test("the Pi and OpenCode copies of the shared recall, skill-reading, wake-up, project, checkpoint, failure and mode code are identical", () => {
  // `failures.ts` and `modes.ts` are the contract-critical ones: they decide what a failure or a mode label means.
  const shared = [
    "recall-core.ts",
    "skill-text.ts",
    "checkpoint-core.ts",
    "wake-up-core.ts",
    "project-core.ts",
    "failures.ts",
    "modes.ts",
  ]
  for (const file of shared) {
    const here = readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8")
    const pi = readFileSync(new URL(`../../pi/src/${file}`, import.meta.url), "utf8")
    expect(here).toBe(pi)
  }
})

// --- native discovery ------------------------------------------------------------------------------------------

test("the shared skills are listed with their repository text, including both this integration reuses", async () => {
  const skills = await sharedSkills()
  const byName = new Map(skills.map((skill) => [skill.name, skill]))
  expect(byName.get("search-before-answer")?.content).toBe(SEARCH)
  expect(byName.get("checkpoint-instructions")?.content).toBe(CHECKPOINT)
  for (const skill of skills) {
    expect(skill.id).toBe(skill.name)
    expect(skill.path).toEndWith(`/skills/${skill.name}/SKILL.md`)
    expect(skill.description.length).toBeGreaterThan(0)
    // The id must be what OpenCode validates against the directory, so it can never drift from the file.
    expect(frontmatterOf(shared(skill.name))?.name).toBe(skill.name)
  }
})

test("a directory that is not a skill is skipped instead of failing the listing", async () => {
  const dir = mkdtempSync(join(tmpdir(), "memcastle-skills-"))
  try {
    mkdirSync(join(dir, "empty"))
    mkdirSync(join(dir, "no-frontmatter"))
    writeFileSync(join(dir, "no-frontmatter", "SKILL.md"), "# just text\n")
    mkdirSync(join(dir, "ok"))
    writeFileSync(join(dir, "ok", "SKILL.md"), "---\nname: ok\ndescription: Does: a thing. Use it.\n---\n\nBody\n")
    expect(await sharedSkills(dir)).toEqual([
      { id: "ok", name: "ok", description: "Does: a thing. Use it.", path: join(dir, "ok", "SKILL.md"), content: "Body" },
    ])
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test("the skills directory is added to OpenCode 1's configuration once, beside what the user configured", () => {
  const config: { skills?: { paths?: string[] } } = { skills: { paths: ["/mine"] } }
  addSkillsPath(config, "/repo/skills")
  addSkillsPath(config, "/repo/skills")
  expect(config.skills?.paths).toEqual(["/mine", "/repo/skills"])
  const empty: { skills?: { paths?: string[] } } = {}
  addSkillsPath(empty, "/repo/skills")
  expect(empty).toEqual({ skills: { paths: ["/repo/skills"] } })
})

test("OpenCode 1: the config hook points OpenCode at the repository's skills directory", async () => {
  const hooks = await plugin.server({ client: { app: { log: async () => ({}) } }, directory: "/work" } as unknown as PluginInput)
  const config: { skills?: { paths?: string[] } } = {}
  await hooks.config?.(config as never)
  expect(config.skills?.paths).toHaveLength(1)
  expect(config.skills?.paths?.[0]).toEndWith("/skills")
  expect(readFileSync(join(config.skills?.paths?.[0] ?? "", "search-before-answer", "SKILL.md"), "utf8")).toBe(
    shared("search-before-answer"),
  )
  await hooks.dispose?.()
})

test("OpenCode 2: both shared skills are registered natively, and one the user already installed is not shadowed", async () => {
  spyOn(console, "info").mockImplementation(() => undefined)
  const added: { name: string; content: string }[] = []
  let transform: ((editor: unknown) => void) | undefined
  const ctx = {
    options: {},
    location: { directory: "/work" },
    event: { subscribe: async function* () {} },
    session: { hook: async () => ({ dispose: async () => undefined }) },
    tool: { hook: async () => ({ dispose: async () => undefined }), transform: async () => ({ dispose: async () => undefined }) },
    command: { transform: async () => ({ dispose: async () => undefined }) },
    skill: {
      transform: async (callback: (editor: unknown) => void) => ((transform = callback), { dispose: async () => undefined }),
    },
  } as unknown as Plugin.Context
  const cleanup = await plugin.setup(ctx)
  transform?.({
    get: (id: string) => (id === "diary" ? { id } : undefined),
    add: (skill: { name: string; content: string }) => added.push(skill),
  })
  const names = added.map((skill) => skill.name)
  expect(names).toContain("search-before-answer")
  expect(names).toContain("checkpoint-instructions")
  expect(names).not.toContain("diary")
  expect(added.find((skill) => skill.name === "search-before-answer")?.content).toBe(SEARCH)
  expect(added.find((skill) => skill.name === "checkpoint-instructions")?.content).toBe(CHECKPOINT)
  await cleanup?.()
  ;(console.info as unknown as { mockRestore?: () => void }).mockRestore?.()
})

test("OpenCode 2: a host that refuses skills does not stop the plugin from loading", async () => {
  const warn = spyOn(console, "warn").mockImplementation(() => undefined)
  spyOn(console, "info").mockImplementation(() => undefined)
  const ctx = {
    options: {},
    location: { directory: "/work" },
    event: { subscribe: async function* () {} },
    session: { hook: async () => ({ dispose: async () => undefined }) },
    tool: { hook: async () => ({ dispose: async () => undefined }), transform: async () => ({ dispose: async () => undefined }) },
    command: { transform: async () => ({ dispose: async () => undefined }) },
    skill: {
      transform: async () => {
        throw new Error("skills are not available")
      },
    },
  } as unknown as Plugin.Context
  const cleanup = await plugin.setup(ctx)
  expect(typeof cleanup).toBe("function")
  expect(warn.mock.calls.flat().join(" ")).toContain("could not be registered")
  await cleanup?.()
  warn.mockRestore()
  ;(console.info as unknown as { mockRestore?: () => void }).mockRestore?.()
})

// --- OpenCode itself -------------------------------------------------------------------------------------------

// Slow (a cold OpenCode start) and needs the opencode binary, so it runs wherever that is installed and skips elsewhere.
test.skipIf(Bun.which("opencode") === null)(
  "OpenCode itself lists both shared skills once the plugin is loaded",
  async () => {
    const root = mkdtempSync(join(tmpdir(), "memcastle-opencode-"))
    try {
      // An isolated home and project, so the user's own skills and plugins cannot make the answer pass or fail.
      const project = join(root, "project")
      mkdirSync(join(project, ".opencode", "plugins"), { recursive: true })
      writeFileSync(
        join(project, ".opencode", "plugins", "memcastle.ts"),
        `export { default } from ${JSON.stringify(join(import.meta.dir, "..", "src", "index.ts"))}\n`,
      )
      Bun.spawnSync(["git", "init", "-q", project])
      const home = join(root, "home")
      const run = Bun.spawnSync(["opencode", "debug", "skill"], {
        cwd: project,
        env: {
          ...process.env,
          HOME: home,
          XDG_CONFIG_HOME: join(home, ".config"),
          XDG_DATA_HOME: join(home, ".local", "share"),
          XDG_STATE_HOME: join(home, ".state"),
          XDG_CACHE_HOME: join(home, ".cache"),
          // The plugin connects lazily, so no daemon is needed to load it; the port keeps it away from a real one.
          MEMCASTLE_PORT: "1",
        },
      })
      const listed = JSON.parse(run.stdout.toString()) as { name: string; location: string }[]
      const found = new Map(listed.map((skill) => [skill.name, skill.location]))
      // The location is the repository file itself: OpenCode read the shared skill, not a copy of it.
      expect(found.get("search-before-answer")).toEndWith("/skills/search-before-answer/SKILL.md")
      expect(found.get("checkpoint-instructions")).toEndWith("/skills/checkpoint-instructions/SKILL.md")
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  },
  120_000,
)
