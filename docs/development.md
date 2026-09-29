# Development

## Prerequisites

- The toolchain pinned in `rust-toolchain.toml`, installed via `mise` — run `mise install` once.

That's it — SurrealKV, the embedded storage engine, is pure Rust,
so unlike the RocksDB backend this project used before #47,
the storage engine itself needs no C/C++ toolchain.
(A transitive TLS dependency, `aws-lc-sys`, may use `cmake` on some targets; that is unrelated to storage.)

## Everyday tasks

```sh
mise run build      # cargo build
mise run test       # cargo nextest run (accepts nextest selectors)
mise run lint       # cargo clippy --all-targets --all-features -- -D warnings
mise run format     # cargo fmt --all
mise run check      # everything CI runs except coverage/docs
mise run ci         # everything CI runs, locally
mise cli <args>      # run memcastle from source, e.g. `mise cli status`
```

`prek install` (once) wires the same checks into `git commit` as pre-commit hooks —
formatting, Clippy, spelling, and the architecture guard described below.

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

By default the palace lives under `~/.memcastle/default` and the daemon binds `127.0.0.1:8420`.
Override either with `MEMCASTLE_PALACE_PATH` / `MEMCASTLE_BIND`, or a config file —
see `config::Config` for the full list of settings and their environment-variable overrides.

## Testing

- **Unit tests** live next to the code they test (`domain::job`'s state machine, `config`'s validation,
  `store`'s migrations/persistence — the storage tests use SurrealDB's in-memory engine for speed, plus one test
  against a real SurrealKV directory to prove data survives a reconnect).
- **Integration tests** (`tests/`) start a real daemon in-process against a tempdir palace and an OS-assigned port:
  - `tests/server.rs` — health, status, graceful shutdown.
  - `tests/concurrency.rs` — many simulated clients submitting jobs and reading status at once,
    proving the shared store stays consistent.
  - `tests/cli.rs` — the binary's argument parsing and its behaviour with no daemon reachable.

Run a subset with nextest's filter syntax, e.g.:

```sh
mise run test -- --filter-expr 'test(job)'
```

## The architecture guard

The non-negotiable invariant in `AGENTS.md` — "the CLI has no business logic MCP/HTTP can't reuse" —
is enforced by the `store-isolation` `prek` hook.
It greps `src/main.rs`, `src/cli.rs`, `src/client/`, `src/mcp/` and `src/api/`
for a direct `store` or `jobs` import (including grouped `use crate::{store::..}` imports).
The one allowed exception is `main.rs`'s `SurrealStore` import, which `memcastle migrate` needs.
If you find yourself wanting to import `store` from one of those,
the fix is almost always to add a method to `app::AppServices` instead,
so the same capability becomes available to every interface at once.

The `single-writer` hook enforces the companion invariant, one daemon and one writer per palace:
`SurrealStore::connect` may only be called from `src/server/`, `src/store/` and `src/main.rs` (for `migrate`).

## This repository is generated from a template

See `AGENTS.md` for the `git tpl` workflow (`mise run tpl:diff`, `mise run tpl:update`)
and where project-specific content goes so template updates keep merging cleanly.
