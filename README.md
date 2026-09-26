<p align="center">
  <img src="docs/images/icon.svg" alt="memcastle" width="160">
</p>

<h1 align="center">memcastle</h1>

<p align="center"><strong>Local-first, always-on memory server for AI coding agents over MCP/HTTP</strong></p>

<p align="center">
  <a href="https://github.com/noirbizarre/memcastle/actions/workflows/ci.yaml">
    <img src="https://github.com/noirbizarre/memcastle/actions/workflows/ci.yaml/badge.svg" alt="CI">
  </a>
  <a href="https://codecov.io/gh/noirbizarre/memcastle">
    <img src="https://codecov.io/gh/noirbizarre/memcastle/graph/badge.svg" alt="Codecov">
  </a>
  <img src="https://img.shields.io/github/v/release/noirbizarre/memcastle" alt="Release">
  <a href="https://noirbizarre.github.io/memcastle/">
    <img src="https://img.shields.io/badge/docs-noirbizarre.github.io-blue" alt="Documentation">
  </a>
  <img src="https://img.shields.io/github/license/noirbizarre/memcastle" alt="License">
</p>

---

Running several AI coding agents (OpenCode, Claude Code, Cursor, ...) side by
side usually means each one gets its own, disconnected memory — or none at
all. MemCastle is a single daemon per project ("palace") that all of them
talk to over MCP or HTTP, so a mining run, a search, or a saved decision from
one agent is immediately visible to every other agent and to the CLI, backed
by one SurrealDB store instead of a pile of SQLite files and a separate
vector index to keep in sync.

```bash
memcastle serve &          # one daemon per palace
memcastle mine ./project   # submits a durable, resumable job — doesn't block
memcastle jobs list
memcastle search "why did we switch to GraphQL?"
```

See [the architecture doc](https://noirbizarre.github.io/memcastle/architecture/)
for the full design and what's deliberately not built yet.

## Installation

Or download a binary for your platform from the
[latest release](https://github.com/noirbizarre/memcastle/releases/latest).

## Usage

```bash
memcastle --help
```

## Documentation

<https://noirbizarre.github.io/memcastle/>

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT — see [LICENSE](LICENSE).
