// The OpenCode 1 adapter: `server()` returns hooks by string key. Wiring only; behaviour lives in `core.ts`.
//
// V1 object entrypoints (`{ id, server }`) are supported from OpenCode 1.18.29.

import type { Hooks, Plugin } from "@opencode-ai/plugin"
import { createCore } from "./core.ts"

export const server: Plugin = async ({ client, directory }, options) => {
  // Logging goes through OpenCode so it lands in its log, and it must never be the reason a hook fails.
  const core = await createCore(
    options,
    async (level, message, extra) => {
      await client.app.log({ body: { service: "memcastle", level, message, extra } }).catch(() => undefined)
    },
    process.env,
    directory,
  )
  if (!core) return {}

  const hooks: Hooks = {
    event: async ({ event }) => {
      // session.created starts the wake-up fetch, which is what gives it a head start on the first request.
      // session.idle -> count turns for checkpoints (#34).
      if (event.type === "session.created") {
        const { id, directory: sessionDirectory, parentID } = event.properties.info
        await core.sessionCreated(id, sessionDirectory, parentID)
      }
      // The connection belongs to the OpenCode session, so it ends with it.
      if (event.type === "session.deleted") await core.sessionDeleted(event.properties.info.id)
    },
    "experimental.chat.system.transform": (input, output) =>
      core.systemTransform(input.sessionID, (text) => output.system.push(text)),
    "experimental.session.compacting": () => core.compacting(),
    "tool.execute.before": () => core.toolBefore(),
    dispose: () => core.dispose(),
  }
  return hooks
}
