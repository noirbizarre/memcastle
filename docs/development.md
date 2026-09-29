# Development

## Prerequisites

- Rust, via `rustup`, which installs the channel pinned in `rust-toolchain.toml` (mise does not manage Rust itself).
- The remaining tools (nextest, prek, typos, ...) via `mise` — run `mise install` once.

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
mise run guards     # the architecture guard hooks, described below
mise run check      # every lint, the guards and the tests, without modifying the tree
mise run ci         # check plus the docs build: the local equivalent of CI's lint and test steps
mise cli <args>      # run memcastle from source, e.g. `mise cli status`
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

By default the palace lives under `~/.memcastle/default` and the daemon binds `127.0.0.1:8420`.

## Configuration

Settings resolve in this order, later winning: built-in defaults, the config file, `MEMCASTLE_*` environment variables.
The config file is `~/.memcastle/config.toml` when it exists, or the path given by `--config` / `MEMCASTLE_CONFIG`.
A malformed environment override is an error naming the variable, never silently ignored.

| Setting (TOML key) | Environment variable | Default |
|---|---|---|
| `palace.path` | `MEMCASTLE_PALACE_PATH` | `~/.memcastle/default` |
| `server.bind` | `MEMCASTLE_BIND` | `127.0.0.1:8420` |
| `logging.level` | `MEMCASTLE_LOG` | `info` |
| `jobs.max_concurrency` | `MEMCASTLE_JOBS_MAX_CONCURRENCY` | `4` |
| `jobs.drain_timeout_secs` (1 to 86400) | `MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS` | `10` |
| `jobs.lease_ttl_secs` (3 to 86400) | `MEMCASTLE_JOBS_LEASE_TTL_SECS` | `30` |
| `store.mode` (`embedded` or `remote`) | none | `embedded` |

A remote store also needs `store.url`, `store.namespace`, `store.database`, `store.username` and `store.password`;
they are file-only, so keep that file out of version control.

Logging precedence, highest first: `MEMCASTLE_LOG`, `RUST_LOG`, the `-v`/`-vv` flags, then `logging.level`.
The global `--mode` flag (or `MEMCASTLE_MODE`) runs a CLI command in a memory mode, see
[the architecture](architecture.md#memory-mode-per-session-never-daemon-global).
`config::Config` is the source of truth if this table ever drifts.

## Testing

- **Unit tests** live next to the code they test (`domain::job`'s state machine, `config`'s validation,
  `store`'s migrations/persistence — the storage tests use SurrealDB's in-memory engine for speed, plus one test
  against a real SurrealKV directory to prove data survives a reconnect).
- **Integration tests** (`tests/`) run against a tempdir palace and an OS-assigned port.
  Most start the daemon in-process (`tests/common`'s `TestDaemon`); the ones that need a real process boundary
  (SurrealKV's file lock is not released within one process) spawn the `memcastle` binary instead:
  - `tests/server.rs` — health, status, graceful shutdown, retry (in-process).
  - `tests/concurrency.rs` — many simulated clients submitting jobs and reading status at once,
    proving the shared store stays consistent (in-process).
  - `tests/memory_mode.rs`, `tests/mcp_memory_mode.rs` — per-request and per-MCP-session memory modes (in-process).
  - `tests/audit.rs`, `tests/repair.rs` — the audit and repair job kinds end to end (in-process).
  - `tests/persistence.rs` — data and job state survive a daemon restart, including a SIGKILL mid-job
    and a pause or cancel requested just before it, and `memcastle restart --bind` (subprocess).
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
for a direct `store` or `jobs` import (including grouped `use crate::{store::..}` imports).
The one allowed exception is `main.rs`'s `SurrealStore` import, which `memcastle migrate` needs.
If you find yourself wanting to import `store` from one of those,
the fix is almost always to add a method to `app::AppServices` instead,
so the same capability becomes available to every interface at once.

The `single-writer` hook enforces the companion invariant, one daemon and one writer per embedded palace:
`SurrealStore::connect` may only be called from `src/server/`, `src/store/` and `src/main.rs` (for `migrate`).

Two more hooks guard the remaining invariants.
`job-status-only-via-apply` fails on any `.status =` assignment outside `src/domain/job.rs`,
so a job's status only ever changes through `Job::apply`.
`no-hand-rolled-ddl` fails on schema DDL in Rust code, because the schema is SurrealKit's (`database/schema/*.surql`).

## This repository is generated from a template

See `CONTRIBUTING.md` for the `git tpl` workflow (`mise run tpl:diff`, `mise run tpl:update`),
and `AGENTS.md` for where project-specific content goes so template updates keep merging cleanly.
