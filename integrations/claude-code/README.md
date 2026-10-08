# MemCastle for Claude Code

This package is a Claude Code plugin marketplace entry.
It connects only through MemCastle's native HTTP MCP endpoint and never reads session history, storage, jobs, or database-admin routes.

Install it with `memcastle integration install claude-code`.
The installer copies it to `XDG_DATA_HOME/memcastle/agents/claude-code`, then asks Claude Code to add that exact directory as the `memcastle-local` marketplace and install `memcastle@memcastle-local` at user scope.
It never parses, changes, or deletes `~/.claude/settings*.json`.
`memcastle integration remove claude-code` asks Claude Code to uninstall that exact plugin and remove that marketplace before deleting the installed copy.

## Configuration

Claude Code prompts for the plugin's `memcastle_mode`, `memcastle_url`, and sensitive `memcastle_token` user configuration.
The native MCP declaration consumes those values directly; its default endpoint is `http://127.0.0.1:8420/mcp`.
No token is copied into this package or MemCastle's installation receipt.

## Lifecycle coverage

| Capability | Status |
| --- | --- |
| Native HTTP MCP | implemented by `.mcp.json` |
| Session-start mode and wake-up guidance | implemented by `SessionStart` prompt hook |
| Search-before-answer reminder | implemented by `UserPromptSubmit` prompt hook and shared skill |
| Explicit checkpoint, audit, repair | implemented as plugin commands |
| Per-session persistent MCP client, reconnect, interval checkpoints, pre-compaction checkpoint | gap |

Claude Code's documented plugin API exposes declarative commands, hooks, skills, and native MCP servers, not a supported long-lived TypeScript mod runtime with MCP-session ownership or transcript access.
The plugin therefore does not fake session registry, reconnect, transcript classification, interval checkpoints, or a pre-compaction checkpoint.

- **Missing**: a supported plugin runtime that owns a native MCP connection and the Claude conversation transcript per session.
- **Fallback**: use `/memcastle-checkpoint` for an explicit save and configure daemon-side triggers for mining.
- **Effect**: mode selection and wake-up are model-directed hook instructions, not enforced connection state, and no automatic emergency or interval checkpoint is submitted.

Do not add a second manually configured `memcastle` MCP server while using this plugin; it would expose duplicate tools.
