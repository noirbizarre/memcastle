// The OpenCode 1 adapter: `server()` returns hooks by string key. Wiring only; behaviour lives in `core.ts`.
//
// V1 object entrypoints (`{ id, server }`) are supported from OpenCode 1.18.29.

import type { Hooks, Plugin } from "@opencode-ai/plugin"
import { createCore } from "./core.ts"

export const server: Plugin = async ({ client }, options) => {
  // Logging goes through OpenCode so it lands in its log, and it must never be the reason a hook fails.
  const core = await createCore(options, async (level, message, extra) => {
    await client.app.log({ body: { service: "memcastle", level, message, extra } }).catch(() => undefined)
  })
  if (!core) return {}

  const hooks: Hooks = {
    event: async ({ event }) => {
      // The connection belongs to the OpenCode session, so it ends with it.
      // session.created -> start the wake-up fetch (#33). session.idle -> count turns for checkpoints (#34).
      if (event.type === "session.deleted") await core.sessionDeleted(event.properties.info.id)
    },
    "experimental.chat.system.transform": () => core.systemTransform(),
    "experimental.session.compacting": () => core.compacting(),
    "tool.execute.before": () => core.toolBefore(),
    dispose: () => core.dispose(),
  }
  return hooks
}
