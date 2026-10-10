<p align="center">
  <img src="docs/images/logo.svg" alt="MemCastle">
</p>

<p align="center"><strong>Local-first, always-on memory server for AI coding agents over MCP/HTTP</strong></p>

<p align="center">
  <a href="https://github.com/memcastle/memcastle/actions/workflows/ci.yaml">
    <img src="https://github.com/memcastle/memcastle/actions/workflows/ci.yaml/badge.svg" alt="CI">
  </a>
  <a href="https://codecov.io/gh/memcastle/memcastle">
    <img src="https://codecov.io/gh/memcastle/memcastle/graph/badge.svg" alt="Codecov">
  </a>
  <img src="https://img.shields.io/github/v/release/memcastle/memcastle" alt="Release">
  <a href="https://memcastle.github.io/">
    <img src="https://img.shields.io/badge/docs-memcastle.github.io-blue" alt="Documentation">
  </a>
  <img src="https://img.shields.io/github/license/memcastle/memcastle" alt="License">
  <!-- Kept off until the score is worth showing:
  <a href="https://m8ven.ai/mcp/memcastle/memcastle?s=readme">
    <img src="https://m8ven.ai/badge/mcp/memcastle/memcastle" alt="M8ven Score">
  </a>
  -->
</p>

---

Running several AI coding agents (OpenCode, Claude Code, Cursor, ...) side by side
usually means each one gets its own, disconnected memory — or none at all.
MemCastle is a single daemon per palace (the memory store that one or more of your projects share)
that all of them talk to over MCP or HTTP,
so a mining run, a search, or a saved decision from one agent
is immediately visible to every other agent and to the CLI.
Everything lives in one SurrealDB store, embedded by default, so there is no external service to run.

```mermaid
flowchart LR
    A[OpenCode] & B[Claude Code] & C[Other MCP clients] -->|MCP| D
    CLI[memcastle CLI] -->|REST| D
    D[memcastle daemon] --> E[(Palace: SurrealDB)]
```

## Installation

```bash
brew install noirbizarre/homebrew-tap/memcastle   # macOS
paru -S memcastle-bin                             # Arch Linux (AUR)
```

Or download a binary for your platform from the [latest release](https://github.com/memcastle/memcastle/releases/latest),
or build from source with `cargo install --path .`.
See the [installation guide](https://memcastle.github.io/installation/) for details and platform notes.

## Quickstart

Start the daemon in one terminal.
It serves `127.0.0.1:8420` and keeps its data in `~/.local/share/memcastle/default`:

```bash
memcastle serve
```

Then, from another terminal:

```bash
memcastle status                       # is it up, where, and is its datastore healthy?
memcastle mine ./project               # submits a durable, resumable job — doesn't block
memcastle job list                     # follow it
memcastle search "why did we switch to GraphQL?"
```

Connect an MCP client to `http://127.0.0.1:8420/mcp`:

```bash
claude mcp add --transport http memcastle http://127.0.0.1:8420/mcp   # Claude Code
```

For OpenCode, add a `remote` server to `opencode.json`.
The [MCP client guide](https://memcastle.github.io/mcp-clients/) has the exact configuration for both.
Stop and restart the daemon whenever you like: the palace is on disk, and it is all still there.

## Documentation

The full documentation is at <https://memcastle.github.io/>.

| To... | Read |
|---|---|
| Install and try it | [Installation](https://memcastle.github.io/installation/), [Quickstart](https://memcastle.github.io/quickstart/) |
| Use it from an agent | [Connect an MCP client](https://memcastle.github.io/mcp-clients/), [Agent integrations](https://memcastle.github.io/integrations/) (`memcastle integration install`), [Agent skills](https://memcastle.github.io/skills/), [MCP tools and REST API](https://memcastle.github.io/mcp-and-api/) |
| Run it day to day | [Running the daemon](https://memcastle.github.io/daemon/), [Authentication](https://memcastle.github.io/authentication/), [Memory modes](https://memcastle.github.io/memory-modes/), [Project configuration](https://memcastle.github.io/project-config/), [Mining sources](https://memcastle.github.io/mining-sources/), [Deduplication](https://memcastle.github.io/deduplication/), [Database access](https://memcastle.github.io/database-access/), [Troubleshooting](https://memcastle.github.io/troubleshooting/) |
| Configure it | [Configuration](https://memcastle.github.io/configuration/) (XDG paths on Linux and macOS, config file, environment, flags), [CLI reference](https://memcastle.github.io/cli/) |
| Know where data lives | [Storage and data](https://memcastle.github.io/storage/), [Migrations and upgrades](https://memcastle.github.io/migrations/) |
| Understand or change it | [Architecture](https://memcastle.github.io/architecture/), [Development](https://memcastle.github.io/development/), [Architecture Decisions](https://memcastle.github.io/adr/), [Writing](https://memcastle.github.io/writing-sources/) and [publishing](https://memcastle.github.io/publishing-sources/) sources, [Integration contract](https://memcastle.github.io/integration-contract/) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT — see [LICENSE](LICENSE).
