# AGENTS.md

Notes for anyone — human or otherwise — changing this repository.

## What this project is

MemCastle is **a long-running memory server, not a CLI process that happens
to expose MCP.** One daemon serves one palace; `memcastle serve`/`daemon` is
the only command that does real work locally — every other subcommand
(`status`, `search`, `mine`, `jobs ...`) is a thin HTTP client to that
daemon, so a web dashboard could do everything the CLI does by calling the
same API. Storage is unified in SurrealDB (embedded RocksDB for local dev,
remote for server deployments) — never a second datastore, never a separate
vector index file to fall out of sync. See `docs/architecture.md` for the
full rationale, including what this deliberately does *not* do yet.

## Non-negotiable invariants

Each of these should be enforced by a hook or a test. An invariant nothing
checks is a comment, and it will be violated.

1. **The CLI has no business logic MCP/HTTP can't reuse** — every subcommand
   except `serve`/`daemon` only calls `client::DaemonClient`, never `store`
   or `jobs` directly. Enforced by the `prek` architecture-guard hook (grep
   for `crate::store` outside `store`/`app`/`server`) and by `tests/cli.rs`.
2. **Job status only changes through `domain::Job::apply`** — no other code
   assigns `job.status` directly. Enforced by `domain::job`'s unit tests
   (every transition, including the rejected ones).
3. **The job queue is durable, the scheduler is only the execution
   mechanism** — a job's state survives a daemon restart. Enforced by
   `jobs::Scheduler::recover` and its exercise in `tests/server.rs`.
4. **One daemon per palace, one writer** — `store` is only ever constructed
   by `server::run`; nothing else opens the embedded RocksDB path directly.
   Enforced by the same architecture-guard hook as (1).

## Layout

```text
src/
├── main.rs     the memcastle binary: parses Cli, dispatches, nothing else
├── cli.rs      argument types only
├── lib.rs      module wiring
├── error.rs    the crate's error type
├── config/     typed configuration (defaults -> file -> env -> validate)
├── domain/     Palace/Wing/Room/Drawer/Job — pure types, no I/O
├── store/      SurrealDB connection, schema, and repository methods
├── jobs/       the scheduler: claiming, dispatch, cooperative pause/cancel
├── mining/     the mining job handler
├── search/     the search abstraction (lexical today; semantic later)
├── app/        application services — the one layer cli/mcp/api call into
├── server/     the daemon composition root + lifecycle (registry file)
├── mcp/        MCP tool surface, over HTTP
├── api/        the REST API (health/status/jobs/search)
└── client/     the CLI's HTTP client for a running daemon
```

Dependencies point inward:
`cli / mcp / api -> app -> domain (+ store/jobs/search traits) -> store`.
Nothing in `domain` knows SurrealDB exists; nothing in `cli`/`mcp`/`api` knows
`store` exists.

## Style

**Every non-obvious line carries a comment saying why.** Not what — the code
says what. Ideally naming the failure it prevents. A comment that restates the
code is worse than none.

**Errors are typed and actionable.** `thiserror` for the library, `miette` at
the binary edge. A diagnostic must carry the two things the user does not
already know: what specifically failed, and what to do about it. Diagnostic
codes are `memcastle::<module>::<kind>`, and a code is a public identifier
users grep for — renaming one is a breaking change.

**Test names are sentences.** `an_unchanged_input_produces_no_output`, not
`test_run_2`. The name should say what would be broken if it failed.

## Commits

Conventional Commits, enforced by commitlint on `commit-msg`. The type becomes a
changelog heading, so choose it as if someone will read it in release notes —
because they will.

## Releases

Driven by gh-ship. Never bump a version or push a tag by hand: `cliff.toml`
derives the version from the commit history, `prepare-release` applies it, and
`.github/ship.yml` is the contract between them. See CONTRIBUTING.md.

## Before you push

```sh
mise run ci
```

Formatting, Clippy, spelling, workflow and Markdown linting, tests and the documentation build. Same as CI.

## This repository is generated from a template

The toolchain, hooks, CI and release workflows come from
[rust.tpl](https://github.com/noirbizarre/rust.tpl) and are updated with
`git tpl update`. Files carrying template-owned content end with a
`# --- project-specific ---` marker: add below it, never above.

Changing template-owned content here fixes it in one repository. Changing it in
the template fixes it in all of them — prefer that.
