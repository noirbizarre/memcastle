// Search before answering: put the shared `search-before-answer` skill in front of the model on every turn.
//
// The text is `skills/search-before-answer/SKILL.md`, read from the repository and never copied here (docs/skills.md).
// It is appended to the system prompt in `before_agent_start`, which runs before each model call, rather than sent as
// a message: a message would stay in the session history and be repeated by every later turn, while the system prompt
// is rebuilt each time, so the instruction is always present and never accumulates. It also survives compaction, which
// a one-off message would not.
//
// MemCastle never forces this. The daemon has no such setting; `forceMemoryRecall.level` only decides what this
// extension tells the model.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "./mcp-manager.ts"
import { recallInstruction } from "./recall-core.ts"
import { readSkill } from "./skill-text.ts"

/** The shared skill this capability injects. */
export const SKILL_NAME = "search-before-answer"

export function registerSearchBeforeAnswer(
  pi: ExtensionAPI,
  manager: () => McpManager | null,
  // Injectable so a test can make the file unreadable without breaking the checkout.
  read: (name: string) => Promise<string> = readSkill,
): void {
  // A skill that cannot be read is reported once per Pi session, not on every prompt.
  let reported = false

  pi.on("session_start", async () => {
    reported = false
  })

  pi.on("before_agent_start", async (event, ctx) => {
    const current = manager()
    // No manager is an `off` session or a finished one: nothing MemCastle-derived may reach the prompt then.
    // `read-only` still searches, so it still gets the instruction.
    if (!current) return
    if (current.settings.forceMemoryRecall.level === "off") return

    let body: string
    try {
      body = await read(SKILL_NAME)
    } catch (error) {
      if (!reported) {
        reported = true
        ctx.ui.notify(
          `MemCastle could not read the ${SKILL_NAME} skill, so this session will not be reminded to search first: ` +
            `${error instanceof Error ? error.message : String(error)}. ` +
            "The extension reads skills/ from the repository checkout it was installed from.",
          "warning",
        )
      }
      return
    }
    // The session may have ended or restarted while the file was read, and its instruction must not leak into another.
    if (manager() !== current) return

    const text = recallInstruction(current.settings.forceMemoryRecall, body)
    if (text === null) return
    // `event.systemPrompt` already holds earlier handlers' changes, so this adds to them instead of replacing them.
    return { systemPrompt: `${event.systemPrompt}\n\n${text}` }
  })
}
