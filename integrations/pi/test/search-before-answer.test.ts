import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "../src/mcp-manager.ts"
import { ALWAYS_LINE, type RecallLevel, resolveForceMemoryRecall } from "../src/recall-core.ts"
import { registerSearchBeforeAnswer } from "../src/search-before-answer.ts"
import { resolveSettings } from "../src/settings.ts"
import { skillBody } from "../src/skill-text.ts"

type Handler = (event: unknown, ctx: ExtensionContext) => unknown

/** The shared skill as the repository has it, read here independently of the code under test. */
const SHARED = skillBody(readFileSync(new URL("../../../skills/search-before-answer/SKILL.md", import.meta.url), "utf8"))

function fakePi() {
  const handlers = new Map<string, Handler[]>()
  const notes: { message: string; level: string }[] = []
  const pi = {
    on(event: string, handler: Handler) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler])
      return () => undefined
    },
  } as unknown as ExtensionAPI
  const ctx = {
    cwd: "/work/memcastle",
    ui: { notify: (message: string, level: string) => notes.push({ message, level }) },
  } as unknown as ExtensionContext
  const fire = async (event: string, payload: object = {}) => {
    let answer: unknown
    for (const handler of handlers.get(event) ?? []) answer = await handler({ type: event, ...payload }, ctx)
    return answer as { systemPrompt?: string; message?: unknown } | undefined
  }
  return { pi, notes, fire, handlers }
}

/** A manager that is only its settings: this capability never calls the daemon itself. */
function manager(level: RecallLevel | undefined): McpManager {
  const settings = resolveSettings(level ? { forceMemoryRecall: { level } } : undefined, {})
  return { settings } as unknown as McpManager
}

function setup(current: McpManager | null) {
  const host = fakePi()
  let live = current
  registerSearchBeforeAnswer(host.pi, () => live)
  return { ...host, replace: (next: McpManager | null) => (live = next) }
}

const turn = { systemPrompt: "You are Pi." }

test("the shared skill's body is appended to the system prompt, byte for byte, without its frontmatter", async () => {
  const { fire } = setup(manager(undefined))
  const answer = await fire("before_agent_start", turn)
  expect(answer?.systemPrompt).toBe(`You are Pi.\n\n${SHARED}`)
  expect(SHARED).not.toContain("memcastle-version")
  expect(SHARED.startsWith("# Search before answering")).toBe(true)
})

test("the instruction is injected on every turn, and never as a message that would pile up in the history", async () => {
  const { fire } = setup(manager("sometimes"))
  for (let i = 0; i < 3; i++) {
    const answer = await fire("before_agent_start", turn)
    expect(answer?.systemPrompt).toContain(SHARED)
    expect(answer?.message).toBeUndefined()
  }
})

test("a project that names a wing adds one line after the shared text, so searches are narrowed the same way", async () => {
  const settings = resolveSettings(undefined, {})
  const scoped = { settings, project: { root: "/p", name: "castle", wing: "castle", room: null } } as unknown as McpManager
  const answer = await setup(scoped).fire("before_agent_start", turn)
  expect(answer?.systemPrompt).toBe(`You are Pi.\n\n${SHARED}\n\nThis project's memory is in wing \`castle\`. Pass \`wing\` when you search, unless the question is clearly about something else.`)
})

test("it builds on the prompt earlier handlers produced instead of replacing it", async () => {
  const { fire } = setup(manager("sometimes"))
  const answer = await fire("before_agent_start", { systemPrompt: "base\n\nadded by another extension" })
  expect(answer?.systemPrompt?.startsWith("base\n\nadded by another extension\n\n")).toBe(true)
})

test("always adds the one override line to the unchanged shared text, and sometimes does not", async () => {
  const always = await setup(manager("always")).fire("before_agent_start", turn)
  expect(always?.systemPrompt).toBe(`You are Pi.\n\n${SHARED}\n\n${ALWAYS_LINE}`)
  const sometimes = await setup(manager("sometimes")).fire("before_agent_start", turn)
  expect(sometimes?.systemPrompt).not.toContain(ALWAYS_LINE)
})

test("a level of off injects nothing", async () => {
  expect(await setup(manager("off")).fire("before_agent_start", turn)).toBeUndefined()
})

test("a session with no manager, which is how an off memory mode or a finished session looks, gets nothing", async () => {
  expect(await setup(null).fire("before_agent_start", turn)).toBeUndefined()
})

test("a session that ends while the skill is read gets nothing, and another session's text does not leak in", async () => {
  const host = fakePi()
  let live: McpManager | null = manager("sometimes")
  registerSearchBeforeAnswer(host.pi, () => live)
  const pending = host.fire("before_agent_start", turn)
  live = null // the manager is read again after the awaited file read
  expect(await pending).toBeUndefined()
})

test("a read-only session still gets the instruction because searching is allowed in it", async () => {
  const settings = resolveSettings({ mode: "read-only" }, {})
  const answer = await setup({ settings } as unknown as McpManager).fire("before_agent_start", turn)
  expect(answer?.systemPrompt).toContain(SHARED)
})

test("the extension registers no handler beyond the two it needs, and no tool or command", () => {
  const { handlers } = setup(manager(undefined))
  expect([...handlers.keys()].sort()).toEqual(["before_agent_start", "session_start"])
})

test("the level reads its option, then its environment variable, then defaults to sometimes", () => {
  expect(resolveForceMemoryRecall(undefined, {}).level).toBe("sometimes")
  expect(resolveForceMemoryRecall({ level: "always" }, {}).level).toBe("always")
  expect(resolveForceMemoryRecall("off", {}).level).toBe("off")
  expect(resolveForceMemoryRecall(undefined, { MEMCASTLE_FORCE_MEMORY_RECALL: " Always " }).level).toBe("always")
  expect(resolveForceMemoryRecall({ level: "off" }, { MEMCASTLE_FORCE_MEMORY_RECALL: "always" }).level).toBe("off")
})

test("a mistyped level falls back to sometimes rather than disabling the session", () => {
  expect(resolveForceMemoryRecall({ level: "everytime" }, {}).level).toBe("sometimes")
  expect(resolveForceMemoryRecall(undefined, { MEMCASTLE_FORCE_MEMORY_RECALL: "yes" }).level).toBe("sometimes")
  expect(resolveForceMemoryRecall(42, {}).level).toBe("sometimes")
})

test("forceMemoryRecall is resolved with the rest of the settings and is independent of the memory mode", () => {
  const settings = resolveSettings({ mode: "read-only", forceMemoryRecall: { level: "always" } }, {})
  expect(settings.mode).toBe("read-only")
  expect(settings.forceMemoryRecall.level).toBe("always")
})

test("an unreadable skill is reported once per session, the turn proceeds, and a restart reports again", async () => {
  const host = fakePi()
  registerSearchBeforeAnswer(host.pi, () => manager("sometimes"), async () => {
    throw new Error("ENOENT: no such file")
  })
  expect(await host.fire("before_agent_start", turn)).toBeUndefined()
  expect(await host.fire("before_agent_start", turn)).toBeUndefined()
  expect(host.notes).toHaveLength(1)
  expect(host.notes[0]?.level).toBe("warning")
  expect(host.notes[0]?.message).toContain("search-before-answer")
  expect(host.notes[0]?.message).toContain("ENOENT")

  await host.fire("session_start", { reason: "new" })
  await host.fire("before_agent_start", turn)
  expect(host.notes).toHaveLength(2)
})
