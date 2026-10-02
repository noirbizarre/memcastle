# MemCastle

<figure markdown="span">
![Logo](images/icon.svg)
</figure>

Local-first, always-on memory server for AI coding agents over MCP/HTTP.

Running several AI coding agents side by side usually means each one gets its own, disconnected memory — or none at all.
MemCastle is a single daemon per palace (the memory store that one or more of your projects share)
that all of them talk to over MCP or HTTP,
so a mining run, a search or a saved decision from one agent is immediately visible to every other agent and to the CLI.
Everything lives in one SurrealDB store, embedded by default, so there is no external service to run.

```mermaid
flowchart LR
    A[OpenCode] & B[Claude Code] & C[Other MCP clients] -->|MCP| D
    CLI[memcastle CLI] -->|REST| D
    D[memcastle daemon] --> E[(Palace: SurrealDB)]
```

## Get started

1. [Install MemCastle](installation.md).
2. Follow the [Quickstart](quickstart.md): start the daemon, store and search your first memories.
3. [Connect an MCP client](mcp-clients.md) such as OpenCode or Claude Code.

## Guides

- [Running the daemon](daemon.md): start, stop, supervise and check it.
- [Authentication](authentication.md): require a bearer token from every client.
- [Database access](database-access.md): open the daemon's database to SurrealDB Studio.
- [Memory modes](memory-modes.md): make a session read-only, or turn memory off for it.
- [Storage and data](storage.md): where data lives, what is in it, how to back it up.
- [Migrations and upgrades](migrations.md): how a palace is kept up to date across releases.
- [Troubleshooting](troubleshooting.md): diagnostic codes and what to do about them.

## Reference

- [Configuration](configuration.md): paths, the config file, environment variables, flags and precedence.
- [CLI](cli.md): every command and flag.
- [MCP tools and REST API](mcp-and-api.md): the interfaces agents and scripts use.

## Project

- [Architecture](architecture.md): how MemCastle is built and why.
- [Development](development.md): building, testing and the local workflow.
- [Architecture Decisions](adr/README.md): the reasoning behind the choices that shaped it.
- [Contributing](https://github.com/noirbizarre/memcastle/blob/main/CONTRIBUTING.md): commits, releases and pull requests.
