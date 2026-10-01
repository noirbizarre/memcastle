# Development

## Prerequisites

- Rust, via `rustup`, which installs the channel pinned in `rust-toolchain.toml` (mise does not manage Rust itself).
- The remaining tools (nextest, prek, typos, ...) via `mise` — run `mise install` once.

That's it — SurrealKV, the embedded storage engine, is pure Rust,
so the storage engine itself needs no C/C++ toolchain.
(A transitive TLS dependency, `aws-lc-sys`, may use `cmake` on some targets; that is unrelated to storage.)

## Everyday tasks

```sh
mise run build      # cargo build
mise run test       # cargo nextest run (accepts nextest selectors)
mise run lint       # cargo clippy --all-targets --all-features -- -D warnings
mise run format     # cargo fmt --all
mise run guards     # the architecture guard hooks, described below
mise run check      # every lint, the guards and the tests, without modifying the tree
mise run ci         # check plus the docs build: the local equivalent of CI's lint,
                    # test and docs steps
mise cli <args>      # run memcastle from source, e.g. `mise cli status`
mise run docs       # serve this documentation locally
mise run docs:build # build it into site/
```

`prek install` (once) wires the same checks into `git commit` as pre-commit hooks —
formatting, Clippy, spelling, actionlint, markdownlint, commitlint, basic file hygiene,
and the architecture guard described below.

## Running the daemon locally

```sh
mise cli serve
# in another terminal:
mise cli status
mise cli mine ./src
mise cli jobs list
mise cli jobs demo --steps 5   # exercise the scheduler without mining anything
mise cli search "job scheduler"
mise cli stop
```

By default the palace lives under `~/.local/share/memcastle/default` and the daemon listens on `127.0.0.1` port `8420`.
Every path, environment variable, flag and the precedence between them is in [Configuration](configuration.md).
`config::Config` is the source of truth if that page ever drifts.

## Testing

- **Unit tests** live next to the code they test (`domain::job`'s state machine, `config`'s validation,
  `store`'s migrations/persistence — the storage tests use SurrealDB's in-memory engine for speed, plus one test
  against a real SurrealKV directory; survival across a restart is proven at the process boundary in
  `tests/persistence.rs`, because SurrealKV's file lock is not released within one process).
- **Integration tests** (`tests/`) run against a tempdir palace and an OS-assigned port.
  Most start the daemon in-process (`tests/common`'s `TestDaemon`); the ones that need a real process boundary
  (SurrealKV's file lock is not released within one process) spawn the `memcastle` binary instead:
  - `tests/server.rs` — health, status, graceful shutdown, retry (in-process).
  - `tests/concurrency.rs` — many simulated clients submitting jobs and reading status at once,
    proving the shared store stays consistent (in-process).
  - `tests/memory_mode.rs`, `tests/mcp_memory_mode.rs` — per-request and per-MCP-session memory modes (in-process).
  - `tests/audit.rs`, `tests/repair.rs` — the audit and repair job kinds end to end (in-process).
  - `tests/persistence.rs` — data and job state survive a daemon restart, including a SIGKILL mid-job
    and a pause or cancel requested just before it, and `memcastle restart --bind --port` (subprocess).
  - `tests/mcp_smoke.rs` — the MCP surface end to end: a real MCP client over streamable HTTP against a
    `memcastle serve` subprocess, covering read and write tools, migrations before serving, a second session,
    and persistence across a restart with a reconnecting session (subprocess).
  - `tests/auth.rs` — optional bearer-token authentication over real HTTP: every route guarded, the open health probe,
    a configured secret, generate, rotate and revoke, and MCP with a token and with no credential-management tool
    (in-process).
  - `tests/auth_lifecycle.rs` — the authentication lifecycle through the real CLI: generate, restart, enable, a
    rotated and a revoked token, the daemon refusing to start with nothing to check against, and no token ever reaching
    the log, the database or any file (subprocess).
  - `tests/config_assets.rs` — the assets override is honoured and checked, and a standalone binary starts with no assets
    (subprocess).
  - `tests/dependencies.rs` — the lockfile never pulls a second storage engine into the binary (ADR-001).
  - `tests/config_bind.rs` — the listener itself: bind address and port precedence, and real bind failures
    (subprocess).
  - `tests/config_paths.rs` — the XDG config, data and state locations, resolved through the real binary
    (subprocess).
  - `tests/migrate.rs` — `memcastle migrate` and its `--check`/`--status` modes (subprocess).
  - `tests/cli.rs` — the binary's argument parsing and its behaviour with no daemon reachable (subprocess).
  - `tests/cli_daemon.rs` — CLI flags that change what the daemon is asked: `--mode`, and relative `mine` paths
    against an in-process daemon (subprocess client).

Run a subset with nextest's filter syntax, e.g.:

```sh
mise run test -- --filter-expr 'test(job)'
```

## The architecture guard

The non-negotiable invariant in `AGENTS.md` — "the CLI has no business logic MCP/HTTP can't reuse" —
is enforced by the `store-isolation` `prek` hook.
It greps `src/main.rs`, `src/cli.rs`, `src/client/`, `src/mcp/` and `src/api/`
for a direct `store` or `jobs` import (including grouped `use crate::{store::..}` imports,
even when rustfmt spreads them over several lines).
The one allowed exception is `main.rs`'s `SurrealStore` import, which `memcastle migrate` needs.
The pattern matches text, not syntax, so a diagnostic-code string such as `memcastle::jobs::not_found` trips it too:
tests should read a code from the error (`Error::body().code`) rather than spell it out in these directories.
If you find yourself wanting to import `store` from one of those,
the fix is almost always to add a method to `app::AppServices` instead,
so the same capability becomes available to every interface at once.

The `single-writer` hook enforces the companion invariant, one daemon and one writer per embedded palace:
`SurrealStore::connect` may only be called from `src/server/`, `src/store/` and `src/main.rs` (for `migrate`).

The authentication invariant has no hook, because a route or an MCP tool is not something a grep can recognise.
It is enforced by tests instead: `tests/auth.rs` walks a list of the REST routes
(and a route that does not exist) without a token, and fails if an MCP tool's name mentions credentials.

Two more hooks guard the remaining invariants.
`job-status-only-via-apply` fails on any `.status =` assignment outside `src/domain/job.rs`,
so a job's status only ever changes through `Job::apply`.
`no-hand-rolled-ddl` fails on schema DDL in Rust code, because the schema is SurrealKit's (`database/schema/*.surql`).

## Documentation

The documentation site is built by [Zensical](https://zensical.org/) from `docs/`, configured in `zensical.toml`.
`mise run docs` serves it with live reload, and `mise run docs:build` (part of `mise run ci`) builds it.
Documentation is part of a change, not a follow-up: a pull request that changes a flag, a setting, a tool
or a user-visible behaviour updates the page that describes it, in the same commit series.

- **Where things go.** Task-oriented pages (installation, quickstart, guides) and references (configuration, CLI, MCP and
  REST) are for users; `architecture.md`, this page and `adr/` are for contributors.
  Say a thing once and link to it from elsewhere instead of repeating it.
- **New pages** must be added to the `nav` in `zensical.toml`, or they are built but not reachable.
- **Describe what exists.** Document the implementation as it is, and put anything not built yet under a "not
  implemented" or "non-goals" heading rather than describing it as available.
  Run the command and paste real output when a page shows some.
- **Diagrams** use [Mermaid](https://mermaid.js.org/) in fenced `mermaid` blocks, next to the text they explain.
  GitHub and the site both render them; stick to flowcharts, sequence, state, class and entity-relationship diagrams,
  and keep them small enough to read on a phone.
- **Prose style.** One sentence per line, wrapped at a clause boundary if a sentence would pass 120 columns
  (`mise run lint:md` enforces the width).
  `mise run spell` runs `typos`; a legitimate new term goes into `typos.toml`'s `extend-words`.

## This repository is generated from a template

See `CONTRIBUTING.md` for the `git tpl` workflow (`mise run tpl:diff`, `mise run tpl:update`),
and `AGENTS.md` for where project-specific content goes so template updates keep merging cleanly.
