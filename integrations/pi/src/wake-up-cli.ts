// A command to run the wake-up on demand and show what a session start would inject.
//
// Not implemented yet: tracked as #22. The shape is fixed so the extension can register it already.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "./mcp-manager.ts"

export function registerWakeUpCommand(_pi: ExtensionAPI, _manager: () => McpManager | null): void {
  // Intentionally empty. A handler registered here reads `_manager()` when it fires and does nothing when it is null.
}
