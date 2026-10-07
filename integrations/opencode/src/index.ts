// The MemCastle plugin for OpenCode, for both OpenCode 1 and OpenCode 2 from one package.
//
// This file only decides *when* to talk to MemCastle; every memory operation is a call to the daemon over MCP.
// The connection, mode and failure handling, wake-up, search-before-answer, checkpointing, audit/repair and the project
// context are all in `core.ts`; this file and the two adapters only register them. Background mining is not built yet. See docs/research.md for why each hook was chosen.
//
// One default export serves both majors: OpenCode 1 calls `server()` and OpenCode 2 calls `setup()`. The two APIs are
// separate, so each has its own adapter (`v1.ts`, `v2.ts`) over the shared behaviour in `core.ts`.
// The plugin packages are imported as types only: V2's `Plugin.define` is an identity function, and a runtime import
// of a package the host does not ship would stop the plugin loading at all.
//
// Only the default export is a plugin. OpenCode 1 treats every exported function of a plugin module as a plugin, so
// helpers live in their own modules and are never re-exported from here.

import type { PluginModule as V1Module } from "@opencode-ai/plugin"
import type { Plugin as V2 } from "@opencode/plugin"
import { server } from "./v1.ts"
import { setup } from "./v2.ts"

const plugin = { id: "memcastle", server, setup }

// Compile-time guard: the shared export must stay a valid plugin for each major.
// Assigned from a variable, not a literal, so the other major's key is not an excess property.
const _v1: V1Module = plugin
const _v2: V2.Plugin = plugin
void _v1
void _v2

export default plugin
