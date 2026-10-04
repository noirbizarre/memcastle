// A Pi session for the tests: the real extension, loaded into the slice of Pi it touches.
//
// Everything the extension registers goes through `on` and `registerCommand`, so a fake that records them and fires
// them in registration order drives the real handlers. The model Pi would ask for a review is a recorder here: a
// review that reaches it is a review that was paid for, which is exactly what an `off` or `read-only` session must not do.

import type { ExtensionAPI, ExtensionCommandContext, ExtensionContext } from "@earendil-works/pi-coding-agent"
import memcastle from "../../../pi/src/extension.ts"

type Handler = (event: unknown, ctx: ExtensionContext) => unknown
type Command = { handler: (args: string, ctx: ExtensionCommandContext) => Promise<void> }

/** What a `before_agent_start` answered once every handler had spoken, merged the way Pi merges them. */
export interface Answer {
  message?: { customType: string; content: string; display: boolean }
  systemPrompt?: string
}

/** The reply a review's model gives, so a `full` session has something valid to save. */
const REVIEW_REPLY = JSON.stringify({ items: [{ destination: "project", content: "pi review output", tags: [] }] })

export function piSession(cwd = "/work/isolation") {
  const handlers = new Map<string, Handler[]>()
  const commands = new Map<string, Command>()
  const notes: { message: string; level: string }[] = []
  /** Every prompt a review sent to the session's model. */
  const asked: string[] = []
  const pi = {
    on(event: string, handler: Handler) {
      handlers.set(event, [...(handlers.get(event) ?? []), handler])
      return () => undefined
    },
    registerCommand(name: string, command: Command) {
      commands.set(name, command)
    },
  } as unknown as ExtensionAPI
  const ctx = {
    cwd,
    model: { id: "session-model" },
    ui: { notify: (message: string, level: string) => notes.push({ message, level }), setStatus: () => undefined },
    sessionManager: {
      getBranch: () => [
        { type: "message", message: { role: "user", content: "which database?" } },
        { type: "message", message: { role: "assistant", content: [{ type: "text", text: "SurrealDB." }] } },
      ],
    },
    modelRegistry: {
      find: () => undefined,
      complete: async (_model: unknown, context: { messages: { content: { text: string }[] }[] }) => {
        asked.push(context.messages[0]?.content[0]?.text ?? "")
        return { stopReason: "stop", content: [{ type: "text", text: REVIEW_REPLY }] }
      },
    },
  } as unknown as ExtensionCommandContext

  memcastle(pi)

  const fire = async (event: string, payload: object = {}): Promise<Answer | undefined> => {
    let answer: Answer | undefined
    // The system prompt a handler receives already holds the earlier handlers' additions, as it does in Pi.
    let systemPrompt = "BASE SYSTEM PROMPT"
    for (const handler of handlers.get(event) ?? []) {
      const result = (await handler({ type: event, systemPrompt, ...payload }, ctx)) as Answer | undefined
      if (result) answer = { ...answer, ...result }
      if (result?.systemPrompt) systemPrompt = result.systemPrompt
    }
    return answer
  }

  return {
    notes,
    asked,
    handlers: [...handlers.keys()].sort(),
    commands: [...commands.keys()].sort(),
    fire,
    /** Run the slash command `name`, as the user typing it would. */
    command: (name: string, args = "") => {
      const found = commands.get(name)
      if (!found) throw new Error(`Pi has no command ${name}`)
      return found.handler(args, ctx)
    },
    /** Everything the model of this session was shown that MemCastle supplied: the messages and the system prompt. */
    shown: (answer: Answer | undefined) => JSON.stringify(answer ?? null),
  }
}
