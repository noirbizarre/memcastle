# ADR-044: Pi 1.0 is the minimum, and the Pi integration's MCP session is built on Pi's own client library

## Status

Accepted.
Amends [ADR-022](022-integrations-are-bun-packages-tested-against-a-real-daemon.md) for the Pi integration:
OpenCode keeps `@modelcontextprotocol/sdk`.
Builds on [ADR-019](019-shared-integration-contract.md) and [ADR-034](034-agent-integration-distribution.md).

## Context

The Pi integration declared `agent = ">=0.99"` and was developed against 0.99.
Pi 1.0 is the line the extension is now built and tested on, and nothing in it carried code for an earlier Pi,
so the old floor was only a number that no one tested.

Pi 1.0 also has MCP support of its own, in three forms, and none of them replaces the extension's connection:

- `pi.registerMcpServer()` declares a server that Pi connects itself and offers to the model (or to codemode),
  so MemCastle's tools would appear twice, and the extension would not hold the connection.
- `ctx.executeTool()` can call such a tool, but it exists only on the context of a tool's `execute()`,
  and the extension's work runs in event and command handlers (`session_start`, `before_agent_start`, `agent_end`,
  `session_before_compact`, slash commands).
- Neither gives the extension the `mcp-session-id` that carries the daemon's memory mode,
  which has to be selected on every connection before anything else uses it.

Pi's MCP client is also published as a library, `@earendil-works/pi-mcp` (`McpClient`, `StreamableHttpTransport`),
which `@earendil-works/pi-coding-agent` depends on.
Pi does not supply it to extensions, which only get `pi-coding-agent`, `pi-ai`, `pi-agent-core` and `pi-tui`.

## Decision

- **Pi 1.0 is the minimum Pi version.**
  The manifest says `agent = ">=1.0"`, the peer dependency says `>=1.0.0`, and the documentation says "1.0 or later".
  No code supports an earlier Pi.
- **The extension keeps owning one MCP session** with the daemon, as before:
  the mode is selected first on every connection, a keep-alive ping holds the session, and a forgotten session (HTTP 404)
  is replaced and the call retried once.
- **That session is built on `@earendil-works/pi-mcp`,** the library Pi uses for its own MCP servers,
  instead of `@modelcontextprotocol/sdk`.
  It is a regular dependency and is bundled, because Pi does not supply it.
  `packaging/integrations/build.sh` therefore keeps only the four packages Pi supplies external,
  and `tests/integration_bundle.rs` fails if a bundle still imports `pi-mcp`.
- **No idle server-to-client stream is opened** (`openGetStream: false`): the daemon sends nothing unprompted.

## Alternatives rejected

- **Register MemCastle with `pi.registerMcpServer()`.**
  It duplicates the tools for the model, cannot be called from the handlers that need it,
  and leaves the memory mode to Pi's connection handling.
- **Keep `@modelcontextprotocol/sdk` in the Pi package.**
  It works, but it is a second MCP implementation next to the one Pi ships and tests against its own agent,
  and it is the larger dependency.

## Consequences

- The Pi package no longer depends on `@modelcontextprotocol/sdk`; OpenCode still does, so the two copies of the
  client differ in their error classes, and the shared fixtures are what keeps their behaviour the same.
- The bundle grows by Pi's client and its transport, and it must be rebuilt when `pi-mcp` changes.
  Dependabot updates it with the other Pi packages.
- A user on Pi 0.99 gets a refusal from `memcastle integration install pi`, naming the version found,
  and has to upgrade Pi first.
