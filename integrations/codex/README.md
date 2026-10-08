# MemCastle for Codex

`memcastle integration install codex` registers the bundled Codex plugin through a local marketplace.
The plugin configures MemCastle as an HTTP MCP server at `http://127.0.0.1:8420/mcp` and reads an optional bearer token from `MEMCASTLE_AUTH_TOKEN`.
It never edits Codex's `config.toml` and never writes a token.

The plugin makes the five shared MemCastle skills available through Codex's native skill discovery.
Use the MCP tools for recall and `/memcastle-checkpoint` for an explicit classified save.

## Contract gaps

- **Missing**: Codex plugins do not expose a supported per-session lifecycle or transcript API.
- **Fallback**: Use the shared skills and the explicit checkpoint command; use daemon-side triggers for mining.
- **Effect**: Wake-up injection, automatic classification, reconnect handling, interval checkpoints, and pre-compaction checkpoints are unavailable.

- **Missing**: Codex does not offer the integration a per-session memory-mode hook.
- **Fallback**: Disable the plugin when a session must be free of MemCastle material.
- **Effect**: Codex's native MCP client owns the connection and its session state.
