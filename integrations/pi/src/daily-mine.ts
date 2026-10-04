// Background mining, once per day, on the extension's own schedule: the daemon has no scheduler.
//
// Not implemented yet: tracked as #26. The shape is fixed so the extension can register it already.
//
// When it is, it only decides *when*: it submits `memcastle_mine` with `{ source: "pi" }` and nothing else. Reading Pi's
// session files is the `pi` mining source's job (`sources/pi/`, docs/adr/028), done by the daemon, so this file never
// opens a session and never names where they live.

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent"
import type { McpManager } from "./mcp-manager.ts"

export function registerDailyMine(_pi: ExtensionAPI, _manager: () => McpManager | null): void {
  // Intentionally empty. A handler registered here reads `_manager()` when it fires and does nothing when it is null.
}
