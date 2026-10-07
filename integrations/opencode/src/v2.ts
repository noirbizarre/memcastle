// The OpenCode 2 adapter: `setup(ctx)` registers hooks on the domains that own them. Wiring only; behaviour lives in
// `core.ts`.
//
// There is no `client.app.log` in V2's plugin context, so the plugin reports through the console, which OpenCode
// captures in its own log. There is no toast either: `Toast.show` exists only on the TUI plugin context
// (`@opencode/plugin/tui`), which a server plugin cannot reach, so a failure outside the checkpoint command is logged
// at its severity and not shown on screen (types of `@opencode/plugin` 2.0.22).

import type { Plugin } from "@opencode/plugin"
import { AUDIT_COMMAND, AUDIT_TOOL, REPAIR_COMMAND, REPAIR_TOOL, auditWing } from "./audit.ts"
import { CHECKPOINT_COMMAND, CHECKPOINT_TOOL, type ReviewHost, checkpointArgs } from "./checkpoint.ts"
import { type Turn, textOf, turnOf } from "./checkpoint-core.ts"
import { createCore, type Level } from "./core.ts"
import { MemCastleFailure } from "./failures.ts"
import { sharedSkills } from "./skills.ts"

const consoleLog = async (level: Level, message: string, extra?: Record<string, unknown>) => {
  // `extra` is already redacted by `describeSettings`; an empty object adds nothing but noise.
  const line = `[memcastle] ${message}`
  const args = extra && Object.keys(extra).length > 0 ? [line, extra] : [line]
  console[level](...args)
}

/** What the plugin reads of an event: V2's union is far larger, and a malformed payload must be ignored. */
interface SessionEventData {
  sessionID?: unknown
  parentID?: unknown
  location?: { directory?: unknown }
}

/** The session id of a `session.deleted` event, or `undefined` for anything else. */
function deletedSessionId(event: { type: string; data?: unknown }): string | undefined {
  if (event.type !== "session.deleted") return undefined
  const sessionId = (event.data as SessionEventData | undefined)?.sessionID
  return typeof sessionId === "string" ? sessionId : undefined
}

/** The session, working directory and parent of a `session.created` event, or `undefined` for anything else. */
function createdSession(event: { type: string; data?: unknown }, fallbackDirectory: string) {
  if (event.type !== "session.created") return undefined
  const data = event.data as SessionEventData | undefined
  if (typeof data?.sessionID !== "string") return undefined
  return {
    sessionId: data.sessionID,
    // An event with no location is asked about the directory OpenCode started in, which is where the session runs.
    directory: typeof data.location?.directory === "string" ? data.location.directory : fallbackDirectory,
    parentId: typeof data.parentID === "string" ? data.parentID : undefined,
  }
}

/** The session id of a `session.idle` event, or `undefined` for anything else. */
function idleSessionId(event: { type: string; data?: unknown }): string | undefined {
  if (event.type !== "session.idle") return undefined
  const sessionId = (event.data as SessionEventData | undefined)?.sessionID
  return typeof sessionId === "string" ? sessionId : undefined
}

/**
 * The turns of OpenCode 2's messages. The session's own messages say `type` and a user one holds its `text`; the
 * provider-level messages the compaction hook receives say `role` and hold a list of parts. Both are read, and only
 * what a user or the assistant said is kept.
 */
export function v2Turns(messages: readonly unknown[]): Turn[] {
  return messages.flatMap((message) => {
    const { type, role, text, content } = (message ?? {}) as Record<string, unknown>
    const who = typeof role === "string" ? role : type
    const turn = turnOf(who, who === "user" && typeof text === "string" ? text : content)
    return turn ? [turn] : []
  })
}

/** How the plugin reads a session and asks a model, through OpenCode 2's context. */
function v2Host(ctx: Plugin.Context): ReviewHost {
  return {
    transcript: async (sessionId) => v2Turns(await ctx.session.context({ sessionID: sessionId } as never)),
    // `generate.text` is a one-off request with no session of its own, so there is no reviewer to claim, and it has no
    // system prompt, so the instructions go in front of the conversation.
    classify: async (_sessionId, request, model) => {
      const { text } = await ctx.generate.text({
        prompt: `${request.system}\n\n---\n\n${request.prompt}`,
        ...(model ? { model: { id: model.id, providerID: model.provider } } : {}),
      } as never)
      return text
    },
  }
}

const TOOL_DESCRIPTION =
  "Save what is worth keeping from this conversation to MemCastle. " +
  "With no `payload`, the plugin reviews the conversation itself and saves what it finds, using `note` as a hint. " +
  "With a `payload` of classified items, it saves exactly those; `emergency` is only for context about to be lost."

const TOOL_INPUT = {
  type: "object",
  properties: {
    payload: {
      type: "object",
      description: "Classified items: { items: [{ destination, content, tags, wing?, name? }] }",
      properties: { items: { type: "array", items: { type: "object" } } },
      required: ["items"],
    },
    emergency: { type: "boolean", description: "Jump the queue; only for context that is about to be lost" },
    note: { type: "string", description: "What the user wants kept, when no payload is given" },
  },
  additionalProperties: false,
} as const

export const setup = async (ctx: Plugin.Context): Promise<(() => Promise<void>) | undefined> => {
  const directory = ctx.location.directory
  const core = await createCore(ctx.options, consoleLog, process.env, directory, v2Host(ctx))
  if (!core) return undefined

  // The stream ends when the cleanup aborts it, so a reloaded plugin never leaves a second subscriber behind.
  const controller = new AbortController()
  void (async () => {
    try {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const untyped = event as { type: string; data?: unknown }
        // session.created starts the wake-up fetch, which is what gives it a head start on the first request.
        const created = createdSession(untyped, directory)
        if (created) await core.sessionCreated(created.sessionId, created.directory, created.parentId)
        // The connection belongs to the OpenCode session, so it ends with it.
        const sessionId = deletedSessionId(untyped)
        if (sessionId) await core.sessionDeleted(sessionId)
        // The agent finished a run: one more exchange towards the next interval review (#34).
        const idle = idleSessionId(untyped)
        if (idle) await core.sessionIdle(idle)
      }
    } catch (error) {
      // An abort is the normal way for this loop to end; anything else must be reported, not thrown into the void.
      if (!controller.signal.aborted) await consoleLog("warn", `event stream ended: ${String(error)}`)
    }
  })()

  // The shared skills, registered with OpenCode's own skill mechanism and read from the repository where they live.
  // A skill the user already installed under the same name wins, so a copy in `.agents/skills` is never shadowed.
  const skills = await sharedSkills(core.skillsDir).catch(async (error) => {
    await consoleLog("warn", `the shared skills could not be read, so none is registered: ${String(error)}`)
    return []
  })
  try {
    await ctx.skill.transform((editor) => {
      // `Skill.Info` brands its strings, which a plain string from a file cannot satisfy without this cast.
      for (const skill of skills) if (!editor.get(skill.id)) editor.add(skill as unknown as Parameters<typeof editor.add>[0])
    })
  } catch (error) {
    // Skills are a convenience on top of the injected reminder: a host that refuses them must not stop the plugin.
    await consoleLog("warn", `the shared skills could not be registered: ${String(error)}`)
  }

  // `context` runs before every agent model request, which is where V1's system transform ran (#33, #36).
  await ctx.session.hook("context", (input) =>
    // The `context` hook is the agent's own request (the title model has a `title` hook), and `system` is rebuilt
    // for each one, so the briefing is added to every request, as in V1.
    core.systemTransform(input.sessionID, (text) => input.system.push({ type: "text", text })),
  )
  // V2's counterpart of V1's `experimental.session.compacting`: the transcript is about to be summarised (#34).
  // The hook already holds the messages, so the emergency review reads them from here and not back from the session.
  await ctx.session.hook("compaction", (input) => core.compacting(input.sessionID, v2Turns(input.messages)))

  // The checkpoint tool and its slash command. A host that refuses them loses only these: everything above still works.
  try {
    await ctx.tool.transform((editor) =>
      editor.add({
        name: CHECKPOINT_TOOL,
        description: TOOL_DESCRIPTION,
        input: TOOL_INPUT as never,
        execute: async (input: unknown, context: { sessionID: string }) => {
          try {
            return { content: await core.checkpoint(context.sessionID, checkpointArgs(input)) }
          } catch (error) {
            // Thrown so OpenCode marks the call as failed, with the daemon's own help in the message.
            throw new Error(error instanceof MemCastleFailure ? error.toUserMessage() : String(error))
          }
        },
      } as never),
    )
    await ctx.command.transform((editor) =>
      editor.add({
        name: CHECKPOINT_COMMAND,
        description: "Save what is worth keeping from this conversation to MemCastle now",
        execute: async (invocation) => {
          const note = textOf((invocation.prompt as { text?: unknown } | undefined)?.text)
          let answer: string
          try {
            answer = await core.checkpoint(invocation.sessionID, { note: note === "" ? undefined : note })
          } catch (error) {
            answer = error instanceof MemCastleFailure ? error.toUserMessage() : String(error)
          }
          // Shown in the session without asking the model to reply to it; the log keeps it if that is not possible.
          await ctx.session
            .synthetic({ sessionID: invocation.sessionID, text: `MemCastle: ${answer}`, resume: false } as never)
            .catch(() => consoleLog("info", `checkpoint: ${answer}`))
        },
      }),
    )
  } catch (error) {
    await consoleLog("warn", `the checkpoint tool and command could not be registered: ${String(error)}`)
  }

  // The audit and repair tools and commands (#127). The user running `/memcastle-repair` is the confirmation, and the
  // command calls the core directly, so no model decides whether to apply. The repair tool stays for a model the user
  // told to apply the plan, and it is held to the same plan.
  const fail = (error: unknown) => (error instanceof MemCastleFailure ? error.toUserMessage() : String(error))
  const show = async (sessionID: string, text: string) =>
    ctx.session.synthetic({ sessionID, text: `MemCastle: ${text}`, resume: false } as never).catch(() => consoleLog("info", text))
  try {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: AUDIT_TOOL,
        description: "Audit the MemCastle palace and plan a repair as a dry run. Read-only; show the user the report and plan.",
        input: { type: "object", properties: { wing: { type: "string", description: "Narrow the embedding counts to one wing" } }, additionalProperties: false } as never,
        execute: async (input: unknown, context: { sessionID: string }) => {
          try {
            return { content: await core.audit(context.sessionID, auditWing(input)) }
          } catch (error) {
            throw new Error(fail(error))
          }
        },
      } as never)
      editor.add({
        name: REPAIR_TOOL,
        description: "Apply the repair plan the audit just showed. Destructive: only when the user told you to apply it.",
        input: { type: "object", properties: {}, additionalProperties: false } as never,
        execute: async (_input: unknown, context: { sessionID: string }) => {
          try {
            return { content: await core.repair(context.sessionID) }
          } catch (error) {
            throw new Error(fail(error))
          }
        },
      } as never)
    })
    await ctx.command.transform((editor) => {
      editor.add({
        name: AUDIT_COMMAND,
        description: "Audit the MemCastle palace and plan a repair, without changing anything",
        execute: async (invocation) => {
          const wing = textOf((invocation.prompt as { text?: unknown } | undefined)?.text)
          let answer: string
          try {
            answer = await core.audit(invocation.sessionID, wing === "" ? undefined : wing.trim())
          } catch (error) {
            answer = fail(error)
          }
          await show(invocation.sessionID, answer)
        },
      })
      editor.add({
        name: REPAIR_COMMAND,
        description: "Apply the MemCastle repair plan that /memcastle-audit just showed",
        execute: async (invocation) => {
          let answer: string
          try {
            answer = await core.repair(invocation.sessionID)
          } catch (error) {
            answer = fail(error)
          }
          await show(invocation.sessionID, answer)
        },
      })
    })
  } catch (error) {
    await consoleLog("warn", `the audit and repair tools and commands could not be registered: ${String(error)}`)
  }

  return async () => {
    controller.abort()
    await core.dispose()
  }
}
