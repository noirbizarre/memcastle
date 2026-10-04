// An OpenCode process for the tests: the real V1 plugin, loaded with the slice of OpenCode's client it touches.
//
// What the plugin registers is what OpenCode can call, so the returned hooks are the whole surface an `off` plugin
// must leave empty. The client's model is a recorder: a review that reaches it is a review that was paid for.

import type { PluginInput } from "@opencode-ai/plugin"
import plugin from "../../../opencode/src/index.ts"

const REVIEW_REPLY = JSON.stringify({ items: [{ destination: "project", content: "opencode review output", tags: [] }] })

export async function openCodePlugin(options: Record<string, unknown>, directory = "/work/isolation") {
  /** Every prompt a review sent to a model. */
  const asked: string[] = []
  const logs: { level: string; message: string }[] = []
  const client = {
    app: {
      log: async ({ body }: { body: { level: string; message: string } }) => {
        logs.push(body)
        return {}
      },
    },
    session: {
      messages: async () => ({
        data: [
          { info: { role: "user", model: { providerID: "anthropic", modelID: "sonnet" } }, parts: [{ type: "text", text: "which database?" }] },
          { info: { role: "assistant" }, parts: [{ type: "text", text: "SurrealDB." }] },
        ],
      }),
      create: async () => ({ data: { id: "ses_reviewer" } }),
      prompt: async ({ body }: { body: { parts: { text: string }[] } }) => {
        asked.push(body.parts[0]?.text ?? "")
        return { data: { parts: [{ type: "text", text: REVIEW_REPLY }] } }
      },
      delete: async () => ({}),
    },
  } as unknown as PluginInput["client"]

  const hooks = await plugin.server({ client, directory } as unknown as PluginInput, options)

  return {
    hooks,
    asked,
    logs,
    /** OpenCode announced a new session in `directory`. */
    created: (sessionID: string) =>
      hooks.event?.({ event: { type: "session.created", properties: { info: { id: sessionID, directory } } } as never }),
    /** The agent finished a run, which the interval review counts. */
    idle: (sessionID: string) => hooks.event?.({ event: { type: "session.idle", properties: { sessionID } } as never }),
    /** The system prompt of one model request, as OpenCode would build it, after every hook added what it wanted. */
    systemPrompt: async (sessionID: string): Promise<string[]> => {
      const output = { system: ["BASE SYSTEM PROMPT"] }
      await hooks["experimental.chat.system.transform"]?.({ sessionID } as never, output as never)
      return output.system
    },
    /** The configuration OpenCode hands to the `config` hook, after the plugin has had its say. */
    configured: async () => {
      const config: { command?: Record<string, unknown>; skills?: { paths?: string[] } } = {}
      await hooks.config?.(config as never)
      return config
    },
    /** Call the plugin's tool `name`, as the model would; `null` when the plugin registered no such tool. */
    callTool: async (name: string, args: Record<string, unknown>, sessionID: string): Promise<string | null> => {
      const tool = hooks.tool?.[name]
      if (!tool) return null
      const result = await tool.execute(args as never, { sessionID } as never)
      // OpenCode 1 accepts a plain string or a result object, and a tool's answer is its text either way.
      return typeof result === "string" ? result : result.output
    },
    /** The host summarises the conversation. */
    compact: (sessionID: string) => hooks["experimental.session.compacting"]?.({ sessionID }, { context: [] }),
  }
}
