// An `off` session cannot receive MemCastle-derived context from either integration, through any path.
//
// The daemon would refuse an `off` session's reads, but it cannot stop a client from injecting what it fetched, or
// from loading a skill that carries it (docs/integration-contract.md). So this is proved on the client: a palace holds
// a marker, and every way each integration can put material in front of the model is driven in `off` and in `full`.
//
// A test that only watched `off` would pass for an integration that does nothing at all. The `full` run of the same
// driver is the control: it must surface material, or the driver proves nothing about `off`.
//
// The list of paths is `tests/fixtures/integration/off-isolation.json`, and each integration must drive every path
// that names it, so a path added to the fixture fails here until it is driven.

import { afterAll, afterEach, beforeAll, expect, test } from "bun:test"
import { submitCheckpoint } from "../../pi/src/checkpoint-core.ts"
import { TestDaemon } from "./support/daemon.ts"
import { configure, restoreEnv } from "./support/env.ts"
import { fixture } from "./support/fixtures.ts"
import { as, requestsBy, startRecording, stopRecording, unattributed } from "./support/network.ts"
import { openCodePlugin } from "./support/opencode.ts"
import { piSession } from "./support/pi.ts"

interface Path {
  id: string
  applies_to: string[]
}
const isolation = fixture<{ marker: { token: string; payload: object }; paths: Path[] }>("off-isolation.json")
const TOKEN = isolation.marker.token

/** What an integration says when it is off: a reason for the silence, not MemCastle's content. */
const INACTIVE = "MemCastle is not active in this session."

let daemon: TestDaemon
beforeAll(async () => {
  daemon = await TestDaemon.start()
  // A full session seeds the palace, before recording starts: the seeding is not an actor under test.
  const seeder = daemon.session("full")
  await submitCheckpoint(seeder, isolation.marker.payload as never)
  await seeder.close()
  startRecording()
})
afterAll(async () => {
  stopRecording()
  await daemon.stop()
})
afterEach(restoreEnv)

type Mode = "full" | "off"
/** The MemCastle-derived material one path put in front of the model or the user, or `[]` when it put none. */
type Driver = (mode: Mode, actor: string) => Promise<string[]>

// --- Pi ---------------------------------------------------------------------------------------------------------

/**
 * Run `body` in a Pi session in `mode`, once the connection is as ready as it will be, and end the session after.
 * Ending it matters: a left-open connection keeps pinging the daemon under the next test's recording.
 */
async function withPi<T>(
  mode: Mode,
  knobs: { eagerCheckpoint?: boolean; recall?: boolean },
  body: (session: ReturnType<typeof piSession>, first: Awaited<ReturnType<ReturnType<typeof piSession>["fire"]>>) => Promise<T>,
): Promise<T> {
  configure(daemon, { mode, recall: false, ...knobs })
  const session = piSession()
  try {
    await session.fire("session_start", { reason: "startup" })
    // The first request waits for the (sync) wake-up, which waits for the connection: after it, a command is not racing.
    return await body(session, await session.fire("before_agent_start", { prompt: "hello" }))
  } finally {
    await session.fire("session_shutdown", { reason: "quit" })
  }
}

const withoutNotice = (notes: { message: string }[]) => notes.map((note) => note.message).filter((message) => message !== INACTIVE)

const pi: Record<string, Driver> = {
  "wake-up-injection": (mode, actor) =>
    as(actor, () => withPi(mode, {}, async (_session, first) => (first?.message ? [first.message.content] : []))),
  "wake-up-command": (mode, actor) =>
    as(actor, () =>
      withPi(mode, {}, async (session) => {
        await session.command("memcastle-wake-up")
        return withoutNotice(session.notes)
      }),
    ),
  "search-before-answer": (mode, actor) =>
    as(actor, () =>
      withPi(mode, { recall: true }, async (session) => {
        const answer = await session.fire("before_agent_start", { prompt: "what did we decide?" })
        return answer?.systemPrompt ? [answer.systemPrompt] : []
      }),
    ),
  "native-skills": (mode, actor) =>
    as(actor, () =>
      withPi(mode, {}, async (session) => {
        const offered = (await session.fire("resources_discover", { reason: "startup" })) as { skillPaths?: string[] } | undefined
        return offered?.skillPaths ?? []
      }),
    ),
  "interval-checkpoint": (mode, actor) =>
    as(actor, () =>
      withPi(mode, { eagerCheckpoint: true }, async (session) => {
        await session.fire("agent_end")
        return session.asked
      }),
    ),
  "manual-checkpoint": (mode, actor) =>
    as(actor, () =>
      withPi(mode, {}, async (session) => {
        await session.command("memcastle-checkpoint")
        return [...session.asked, ...withoutNotice(session.notes)]
      }),
    ),
  "emergency-checkpoint": (mode, actor) =>
    as(actor, () =>
      withPi(mode, {}, async (session) => {
        await session.fire("session_before_compact", { reason: "threshold" })
        return session.asked
      }),
    ),
}

// --- OpenCode ---------------------------------------------------------------------------------------------------

async function loadedOpenCode(mode: Mode, knobs: { eagerCheckpoint?: boolean; recall?: boolean; wakeUp?: boolean } = {}) {
  configure(daemon, { mode, recall: false, wakeUp: false, ...knobs })
  const opencode = await openCodePlugin({})
  await opencode.created("ses_isolation")
  return opencode
}

/** What a system prompt holds beyond OpenCode's own text. */
const addedBy = (system: string[]) => system.slice(1)

const opencode: Record<string, Driver> = {
  "wake-up-injection": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode, { wakeUp: true })
      try {
        return addedBy(await plugin.systemPrompt("ses_isolation"))
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
  "search-before-answer": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode, { recall: true })
      try {
        return addedBy(await plugin.systemPrompt("ses_isolation"))
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
  "interval-checkpoint": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode, { eagerCheckpoint: true })
      try {
        await plugin.idle("ses_isolation")
        return plugin.asked
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
  "manual-checkpoint": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode)
      try {
        const answer = await plugin.callTool("memcastle_checkpoint", {}, "ses_isolation")
        return [...plugin.asked, ...(answer === null ? [] : [answer])]
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
  "emergency-checkpoint": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode, { eagerCheckpoint: false })
      try {
        await plugin.compact("ses_isolation")
        return plugin.asked
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
  "native-skills": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode)
      try {
        return (await plugin.configured()).skills?.paths ?? []
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
  "model-facing-tools": (mode, actor) =>
    as(actor, async () => {
      const plugin = await loadedOpenCode(mode)
      try {
        const config = await plugin.configured()
        return [...Object.keys(plugin.hooks.tool ?? {}), ...Object.keys(config.command ?? {})]
      } finally {
        await plugin.hooks.dispose?.()
      }
    }),
}

const drivers: Record<string, Record<string, Driver>> = { pi, opencode }

// --- the proof --------------------------------------------------------------------------------------------------

test("every path the fixture names is driven for each integration it applies to, and no driver lacks a path", () => {
  for (const [name, table] of Object.entries(drivers)) {
    const named = isolation.paths.filter((path) => path.applies_to.includes(name)).map((path) => path.id)
    expect(Object.keys(table).sort(), `the ${name} drivers`).toEqual(named.sort())
  }
})

for (const [name, table] of Object.entries(drivers)) {
  for (const path of isolation.paths.filter((candidate) => candidate.applies_to.includes(name))) {
    test(`${name}: an off session gets nothing from ${path.id}, and a full one does, so the driver is not vacuous`, async () => {
      const driver = table[path.id]!

      const full = await driver("full", `${name}-${path.id}-full`)
      expect(full.length, `${name} ${path.id} in full mode must surface something, or this test proves nothing`).toBeGreaterThan(0)

      const actor = `${name}-${path.id}-off`
      const surfaced = await driver("off", actor)
      expect(surfaced, `${name} ${path.id} in off mode`).toEqual([])
      expect(JSON.stringify(surfaced)).not.toContain(TOKEN)
      // The wire is the stronger witness: no health check, no handshake, no rejected call, nothing to refuse.
      expect(requestsBy(actor), `${name} ${path.id} in off mode must not touch the daemon`).toEqual([])
      expect(unattributed()).toEqual([])
    })
  }
}

test("the wake-up of a full session carries the marker, so an off session's silence is not an empty palace", async () => {
  const wakeUp = await pi["wake-up-injection"]!("full", "pi-marker-control")
  expect(JSON.stringify(wakeUp)).toContain(TOKEN)
  const openCodeWakeUp = await opencode["wake-up-injection"]!("full", "opencode-marker-control")
  expect(JSON.stringify(openCodeWakeUp)).toContain(TOKEN)
})
