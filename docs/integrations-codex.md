# Codex integration

`memcastle integration install codex` installs the bundled Codex plugin through a local marketplace.
It uses Codex's native HTTP MCP transport at `http://127.0.0.1:8420/mcp`.
When the daemon requires authentication, export `MEMCASTLE_AUTH_TOKEN` before starting Codex.
The installer never edits `~/.codex/config.toml` and never writes a token.

The plugin exposes the shared MemCastle skills and the MCP tools supplied by the daemon.
Use `/memcastle-checkpoint` to submit explicitly classified items.

Codex does not expose a supported plugin API for session startup, transcript access, context compaction, or a plugin-owned
long-lived MCP connection.
Consequently wake-up injection, automatic and emergency checkpoints, reconnect handling, and automatic per-session
memory-mode selection are unavailable.
Use the shared skills, explicit MCP calls, and daemon-side triggers instead.
