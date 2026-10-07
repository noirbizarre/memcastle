import { expect, test } from "bun:test"
import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent"
import { AUDIT_COMMAND, registerAuditCommand } from "../src/audit-command.ts"
import type { McpManager } from "../src/mcp-manager.ts"
import { resolveSettings } from "../src/settings.ts"

const ORPHAN = { drawer_id: "d1", room: "gone" }

/** A daemon holding one orphan until an applied repair removes it; records every call. */
function scripted() {
  const calls: { tool: string; args: Record<string, unknown> }[] = []
  let orphans = [ORPHAN]
  let n = 0
  const session = {
    async call(tool: string, args: Record<string, unknown> = {}) {
      calls.push({ tool, args })
      if (tool === "memcastle_audit") {
        const result = { wing: null, orphan_drawers: orphans, dangling_provenance_drawers: [], stuck_failed_jobs: 0, running_jobs: 0, drawers_without_embedding: 0, total_drawers_in_scope: 1 }
        return { id: `a${++n}`, status: "completed", result }
      }
      if (tool === "memcastle_repair") {
        const dry = args.dry_run !== false
        const actions = orphans.map((o) => ({ action: "remove_orphan_drawer", ...o }))
        if (!dry) orphans = []
        return { id: `r${++n}`, status: "completed", result: { dry_run: dry, based_on_job: args.based_on_job, actions } }
      }
      return {}
    },
  }
  return { session, calls }
}

function run(mode: string, confirmed: boolean) {
  const { session, calls } = scripted()
  const notes: string[] = []
  let asked = 0
  const manager = {
    settings: resolveSettings({ mode }, {}),
    project: null,
    session,
    ready: Promise.resolve(true),
    report: (error: unknown) => notes.push(String(error)),
  } as unknown as McpManager
  let handler!: (args: string, ctx: ExtensionCommandContext) => Promise<void>
  const pi = { registerCommand: (name: string, command: { handler: typeof handler }) => name === AUDIT_COMMAND && (handler = command.handler) } as unknown as ExtensionAPI
  registerAuditCommand(pi, () => manager)
  const ctx = {
    cwd: "/work/castle",
    ui: {
      notify: (message: string) => notes.push(message),
      setStatus: () => undefined,
      confirm: async () => (asked++, confirmed),
    },
  } as unknown as ExtensionCommandContext
  return { go: () => handler("", ctx), calls, notes, asked: () => asked }
}

const applied = (calls: { args: Record<string, unknown> }[]) => calls.filter((c) => c.args.dry_run === false)

test("a declined confirmation applies nothing and writes no diary entry", async () => {
  const t = run("full", false)
  await t.go()
  expect(t.asked()).toBe(1)
  expect(applied(t.calls)).toEqual([])
  expect(t.calls.some((c) => c.tool === "memcastle_diary_write")).toBe(false)
  expect(t.notes.join("\n")).toContain("declined")
})

test("a confirmed repair applies the shown plan and records a diary summary", async () => {
  const t = run("full", true)
  await t.go()
  expect(applied(t.calls)).toHaveLength(1)
  expect(t.calls.at(-1)?.tool).toBe("memcastle_diary_write")
  expect(t.notes.join("\n")).toContain("no orphan drawers are left")
})

test("a read-only session shows the plan, never asks and never applies", async () => {
  const t = run("read-only", true)
  await t.go()
  expect(t.asked()).toBe(0)
  expect(applied(t.calls)).toEqual([])
  expect(t.notes.join("\n")).toContain("not applied")
})
