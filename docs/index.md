# memcastle

Local-first, always-on memory server for AI coding agents over MCP/HTTP

Running several AI coding agents side by side usually means each one gets its own, disconnected memory — or none at all.
MemCastle is a single daemon per project ("palace") that all of them talk to over MCP or HTTP,
so a mining run, a search, or a saved decision from one agent is immediately visible to every other agent and to the CLI.

See [Architecture](architecture.md) for the full design, and [Development](development.md) for the local workflow.

## Installation

Or download a binary for your platform from the [latest release](https://github.com/noirbizarre/memcastle/releases/latest).

## Usage

```bash
memcastle serve &
memcastle status
memcastle mine ./project
memcastle jobs list
memcastle search "why did we switch to GraphQL?"
```
