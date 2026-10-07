# Claude Code plugin API research

The supported Claude Code plugin layout is a `.claude-plugin/plugin.json` manifest with marketplace registration, commands, hooks, skills, and an optional `.mcp.json` native MCP declaration.
The package uses a local marketplace because Claude Code owns plugin registration and its settings files must not be edited by MemCastle.

Native MCP supports a streamable HTTP server with headers, so `.mcp.json` declares MemCastle's configured endpoint and bearer token.
The manifest declares `userConfig` for mode, endpoint, and a sensitive token, which current Claude Code can prompt for when enabling a plugin.

| Contract item | Supported mechanism | Result |
| --- | --- | --- |
| Native MCP | `.mcp.json` HTTP server | implemented |
| Session-start guidance | `SessionStart` prompt hook | implemented, best effort |
| Search reminder | `UserPromptSubmit` prompt hook and shared skill | implemented, best effort |
| Explicit checkpoint/audit/repair | plugin command Markdown | implemented |
| Persistent client and reconnect | no plugin mod runtime | gap |
| Transcript classification and interval checkpoint | no supported transcript API | gap |
| PreCompact checkpoint | no supported pre-compaction plugin hook with MCP ownership | gap |

For each gap: **Missing** is a documented long-lived plugin/mod API with per-session MCP and transcript access; **Fallback** is the explicit command or daemon-side trigger; **Effect** is that the operation is not automatic.
