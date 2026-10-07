# Claude Code integration

`memcastle integration install claude-code` installs the shipped Claude Code plugin through a uniquely named local
marketplace.
It uses Claude Code's plugin and native HTTP MCP mechanisms, not direct edits to Claude settings.

The plugin exposes the shared MemCastle skills, native MCP tools, session-start and prompt reminders, and
`/memcastle-checkpoint`, `/memcastle-audit`, and `/memcastle-repair` commands.
Start the daemon before starting Claude Code.

Configure the plugin's endpoint and sensitive token through Claude Code's plugin configuration.
The default endpoint is `http://127.0.0.1:8420/mcp`.

Claude Code does not currently publish a supported long-lived mod API that gives a plugin a per-session MCP connection
or transcript access.
Consequently, automatic reconnect, interval checkpoints, transcript classification, and pre-compaction checkpoints are
unavailable.
Use `/memcastle-checkpoint` for explicit saves and daemon-side triggers for mining.
