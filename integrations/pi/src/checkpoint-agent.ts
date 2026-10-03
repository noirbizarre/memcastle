// Interval review: the integration's own model decides what is worth keeping and submits a checkpoint.
//
// Not implemented yet: tracked as #23. The shape is fixed so the extension can register it already.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "./mcp-manager.ts"

export function registerCheckpointAgent(_pi: ExtensionAPI, _manager: () => McpManager | null): void {
  // Intentionally empty. A handler registered here reads `_manager()` when it fires and does nothing when it is null.
}
