// The OpenCode 1 adapter: `server()` returns hooks by string key. Wiring only; behaviour lives in `core.ts`.
//
// V1 object entrypoints (`{ id, server }`) are supported from OpenCode 1.18.29.

import type { Hooks, Plugin, PluginInput } from "@opencode-ai/plugin"
import {
  CHECKPOINT_TOOL,
  type ReviewHost,
  addCheckpointCommand,
  checkpointArgs,
} from "./checkpoint.ts"
import { type Turn, textOf, turnOf } from "./checkpoint-core.ts"
import { type Notify, createCore } from "./core.ts"
import { MemCastleFailure } from "./failures.ts"
import { addSkillsPath } from "./skills.ts"

type Client = PluginInput["client"]

/** What a failed SDK call carries, so it can be turned into a failure the user can read. */
function sdkFailure(what: string, error: unknown): MemCastleFailure {
  const detail = typeof error === "object" && error !== null ? JSON.stringify(error) : String(error)
  return new MemCastleFailure("unexpected", `OpenCode could not ${what}: ${detail}.`, null, "Nothing was saved. Try again.")
}

/** How the plugin reads a session and asks a model, through OpenCode 1's client. */
export function v1Host(client: Client): ReviewHost {
  const messages = async (sessionId: string) => {
    const result = await client.session.messages({ path: { id: sessionId } })
    if (result.error || !result.data) throw sdkFailure("read the conversation", result.error ?? "no data")
    return result.data
  }
  return {
    transcript: async (sessionId): Promise<Turn[]> =>
      (await messages(sessionId)).flatMap(({ info, parts }) => {
        // A synthetic or ignored part was added by OpenCode or a plugin, not said by either side.
        const said = parts.flatMap((part) => (part.type === "text" && !part.synthetic && !part.ignored ? [{ type: "text", text: part.text }] : []))
        const turn = turnOf(info.role, said)
        return turn ? [turn] : []
      }),
    classify: async (sessionId, request, model, signal, claim) => {
      // The session's own model, as the last user message chose it, unless the user configured one for reviews.
      const chosen =
        model !== null
          ? { providerID: model.provider, modelID: model.id }
          : (await messages(sessionId)).flatMap(({ info }) => (info.role === "user" ? [info.model] : [])).at(-1)
      const created = await client.session.create({ body: { parentID: sessionId, title: "MemCastle checkpoint review" } })
      const reviewer = created.data?.id
      if (created.error || !reviewer) throw sdkFailure("open a session to review the conversation in", created.error ?? "no id")
      // Before the first prompt, so no hook of this plugin ever treats the reviewer as a conversation of the user's.
      claim(reviewer)
      try {
        const reply = await client.session.prompt({
          path: { id: reviewer },
          body: {
            ...(chosen ? { model: chosen } : {}),
            system: request.system,
            // The reviewer replies with text. It must not be able to write the checkpoint itself and skip the validation.
            tools: { [CHECKPOINT_TOOL]: false },
            parts: [{ type: "text", text: request.prompt }],
          },
          signal,
        })
        if (reply.error || !reply.data) throw sdkFailure("ask the model to review the conversation", reply.error ?? "no reply")
        return textOf(reply.data.parts.flatMap((part) => (part.type === "text" ? [{ type: "text", text: part.text }] : [])))
      } finally {
        // A reviewer left behind would show up in the user's session list.
        await client.session.delete({ path: { id: reviewer } }).catch(() => undefined)
      }
    },
  }
}

/**
 * The `memcastle_checkpoint` tool, or nothing when OpenCode cannot supply the `tool` helper.
 *
 * Imported when needed rather than at load: the plugin packages are types-only everywhere else, so a host that does
 * not ship `@opencode-ai/plugin` still loads the plugin and only loses this tool, which the log says.
 */
async function checkpointTool(
  core: NonNullable<Awaited<ReturnType<typeof createCore>>>,
  log: (message: string) => Promise<void>,
): Promise<Hooks["tool"]> {
  let helper: typeof import("@opencode-ai/plugin").tool
  try {
    helper = (await import("@opencode-ai/plugin")).tool
  } catch (error) {
    await log(`the ${CHECKPOINT_TOOL} tool is not registered, because @opencode-ai/plugin could not be loaded: ${String(error)}`)
    return undefined
  }
  const z = helper.schema
  return {
    [CHECKPOINT_TOOL]: helper({
      description:
        "Save what is worth keeping from this conversation to MemCastle. " +
        "With no `payload`, the plugin reviews the conversation itself and saves what it finds, using `note` as a hint. " +
        "With a `payload` of classified items, it saves exactly those; `emergency` is only for context about to be lost.",
      args: {
        payload: z
          .object({ items: z.array(z.record(z.string(), z.unknown())) })
          .optional()
          .describe("Classified items: { items: [{ destination, content, tags, wing?, name? }] }"),
        emergency: z.boolean().optional().describe("Jump the queue; only for context that is about to be lost"),
        note: z.string().optional().describe("What the user wants kept, when no payload is given"),
      },
      // A failure is thrown so OpenCode marks the call as failed, with the daemon's own help in the message.
      execute: async (args, context) => {
        try {
          return await core.checkpoint(context.sessionID, checkpointArgs(args))
        } catch (error) {
          throw new Error(error instanceof MemCastleFailure ? error.toUserMessage() : String(error))
        }
      },
    }),
  }
}

export const server: Plugin = async ({ client, directory }, options) => {
  // Logging goes through OpenCode so it lands in its log, and it must never be the reason a hook fails.
  const log = async (level: "debug" | "info" | "warn" | "error", message: string, extra?: Record<string, unknown>) => {
    await client.app.log({ body: { service: "memcastle", level, message, extra } }).catch(() => undefined)
  }
  // A toast in the TUI, so a failure is seen when it happens and not found later in a log. Under `opencode run` or a
  // server there is no TUI to show it, which is why the log line is always written as well.
  const notify: Notify = async (severity, message) => {
    await client.tui.showToast({ body: { title: "MemCastle", message, variant: severity } })
  }
  const core = await createCore(options, log, process.env, directory, v1Host(client), notify)
  if (!core) return {}

  const hooks: Hooks = {
    // Makes the shared skills visible to OpenCode's own `skill` tool, read from the repository where they live,
    // and adds `/memcastle-checkpoint`, which asks the model to call the checkpoint tool.
    config: async (config) => {
      addSkillsPath(config, core.skillsDir)
      addCheckpointCommand(config)
    },
    event: async ({ event }) => {
      // session.created starts the wake-up fetch, which is what gives it a head start on the first request.
      if (event.type === "session.created") {
        const { id, directory: sessionDirectory, parentID } = event.properties.info
        await core.sessionCreated(id, sessionDirectory, parentID)
      }
      // The agent finished a run: one more exchange towards the next interval review.
      if (event.type === "session.idle") await core.sessionIdle(event.properties.sessionID)
      // The connection belongs to the OpenCode session, so it ends with it.
      if (event.type === "session.deleted") await core.sessionDeleted(event.properties.info.id)
    },
    "experimental.chat.system.transform": (input, output) =>
      core.systemTransform(input.sessionID, (text) => output.system.push(text)),
    "experimental.session.compacting": (input) => core.compacting(input.sessionID),
    dispose: () => core.dispose(),
  }
  const tool = await checkpointTool(core, (message) => log("warn", message))
  if (tool) hooks.tool = tool
  return hooks
}
