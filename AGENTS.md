# AGENTS.md

Notes for anyone — human or otherwise — changing this repository.

## What this project is

MemCastle is **a long-running memory server, not a CLI process that happens to expose MCP.**
One daemon serves one palace; `memcastle serve` is the only command that does real work locally
(`daemon start` just spawns it in the background) —
every other subcommand (`status`, `search`, `mine`, `job ...`) is a thin HTTP client to that daemon,
so a web dashboard could do everything the CLI does by calling the same API.
The one exception is `memcastle migrate`, which touches storage directly because it must work before a daemon
exists (see invariant 1).
Storage is unified in SurrealDB (embedded SurrealKV for local dev, remote for server deployments) —
never a second datastore, never a separate vector index file to fall out of sync.
See `docs/architecture.md` for the full rationale, including what this deliberately does *not* do yet.

## Non-negotiable invariants

Each of these should be enforced by a hook or a test.
An invariant nothing checks is a comment, and it will be violated.

1. **The CLI has no business logic MCP/HTTP can't reuse** —
   every subcommand except `serve`/`migrate` only calls `client::DaemonClient`, never `store` or `jobs`
   directly (`completions` calls neither: it prints a script locally).
   (`daemon start` and `daemon restart` also manage the daemon *process* — they read the registry file
   via `server::lifecycle` and spawn `serve` detached — but touch neither `store` nor `jobs`.)
   `migrate` is a second, narrow exception alongside `serve`: it connects to storage directly (via
   `crate::migrate::run`/`status`, the same runner `serve` calls on every startup) because migration must work
   without, and before, a daemon exists — see `docs/adr/004-versioned-database-migrations.md`.
   Enforced by the `prek` `store-isolation` hook: it greps `main.rs`, `cli.rs`, `client/`, `mcp/` and `api/`
   for any `crate::`/`memcastle::` `store` or `jobs` path, in code or in a string, allowing only `main.rs`'s `SurrealStore`
   import for `migrate`.
2. **Job status only changes through `domain::Job::apply`** — no other code assigns `job.status` directly.
   Enforced by `domain::job`'s table-driven test over every (status, event) pair, including the rejected ones,
   and by the prek `job-status-only-via-apply` hook, which fails on any `.status =` assignment outside `domain/job.rs`.
3. **The job queue is durable, the scheduler is only the execution mechanism** —
   a job's state survives a daemon restart.
   Enforced by `jobs::Scheduler::recover`, its per-state unit tests in `jobs::tests`,
   and `tests/persistence.rs` (SIGKILL a daemon mid-job, restart, the job resumes).
4. **One daemon per embedded palace, one writer** — `store` is only ever constructed by `server::run` or the `migrate`
   CLI command (the same second exception as (1)); nothing else opens the embedded SurrealKV path directly.
   SurrealKV's file lock enforces the single writer for an embedded palace.
   A remote palace may be shared by several daemons; job leases and fencing keep that safe
   (see `docs/adr/006-job-leases.md`).
   Enforced by the prek `single-writer` hook: `SurrealStore::connect` and the `surrealkv:` endpoint
   may only appear under `server/`, `store/` and in `main.rs`.
5. **Schema management is SurrealKit's, not MemCastle's** — `database/schema/*.surql` is applied through
   SurrealKit's `Sync`/`embed_schema!()` (`store::mod`), never a hand-rolled schema-diff/versioning engine.
   MemCastle owns only its own application data-migration steps and version watermark
   (`crate::migrate`, `store::migration_state`) — see `docs/adr/004-versioned-database-migrations.md`.
   Enforced by the prek `no-hand-rolled-ddl` hook: no `DEFINE`/`REMOVE`/`ALTER` DDL or `INFO FOR` statement
   may appear in Rust code under `src/` (comments excepted).
6. **Every route but `GET /api/health` passes the authentication layer, and MCP never touches credentials** —
   the layer wraps the merged router in `server::run`, so a route added later is guarded by default,
   and token generation and revocation are REST/CLI operations with no MCP tool.
   A token or secret is never logged, serialised or persisted in plaintext (`config::Secret` redacts it;
   the store holds only a digest) — see `docs/adr/014-optional-token-authentication.md`.
   Enforced by `tests/auth.rs` (every route and an unknown path refused without a token, no credential-named MCP tool)
   and `tests/auth_lifecycle.rs` (no token in the log, the database or any file).
7. **The database admin endpoint is opt-in, loopback by default, and runs on the daemon's own handle** —
   only an explicit `memcastle db start` (REST `/api/db`) opens it, `serve` never does, and it never binds beyond loopback
   without `--allow-remote` *and* authentication.
   It serves each connection from a clone of the daemon's `Surreal<Any>`, never a second connection or process,
   and has no MCP tool — see `docs/adr/015-database-admin-endpoint.md`.
   Enforced by `tests/db_endpoint.rs` (a started daemon has no endpoint, unsafe binds are refused,
   foreign origins are refused, the endpoint sees and shares the daemon's data),
   by `tests/auth.rs` (`/api/db` is guarded, no MCP tool mentions the database)
   and by `tests/auth_lifecycle.rs` (no token in the log or any file).
8. **An integration is lifecycle glue over MCP and HTTP, and nothing else** —
   everything under `integrations/` decides *when* to call MemCastle and never reaches storage, the job code or the
   database admin endpoint, and every integration satisfies the same conformance matrix
   (`docs/integration-contract.md`, `docs/adr/019-shared-integration-contract.md`).
   Client lifecycle logic does not move into MemCastle to make two clients look alike.
   Enforced by the prek `integrations-http-only` hook: it fails on `surrealdb`, `surrealkv`, `SurrealStore`, a
   `store`/`jobs` path or `/api/db` anywhere under `integrations/` (Markdown and `node_modules` excepted),
   and by `tests/integration_contract.rs`, which replays `tests/fixtures/integration/` against a real daemon and fails
   when the contract page, the capability manifest and the test names disagree.

## Layout

```text
src/
├── main.rs     the memcastle binary: parses Cli, dispatches (and hosts `migrate`'s direct storage access)
├── cli.rs      argument types only
├── lib.rs      module wiring
├── error.rs    the crate's error type
├── term.rs     terminal presentation for the CLI: colour, TTY detection and confirmation prompts
├── config/     typed configuration (defaults -> file -> env -> CLI -> validate) and Unix XDG paths
├── domain/     Palace/Wing/Room/Drawer/Job, checkpoint payloads, entities, memory modes — pure types, no I/O
├── store/      SurrealDB connection and repository methods (schema is applied from `database/schema/`)
├── migrate/    versioned data migrations and the version watermark, run before serving
├── assets/     runtime asset resolution (override, installed, embedded); never user data, never the network
├── dbadmin/    the database admin endpoint: SurrealDB's WebSocket protocol over the daemon's own handle
├── jobs/       the scheduler: claiming, dispatch, cooperative pause/cancel, crash recovery
├── mining/     the mining job handler
├── checkpoint/ the checkpoint job handler (durable, resumable memory writes)
├── audit/      the audit job handler (read-only consistency report)
├── repair/     the repair job handler (narrow, dry-run-first fixes)
├── search/     the retrieval contract and ranking policy (lexical, semantic, hybrid, temporal, graph expansion)
├── embed/      embedding providers (command, OpenAI-compatible HTTP) behind one trait, and the `Embed` job handler
├── app/        application services — the one layer mcp/api call into (the CLI reaches it over HTTP, via `client/`)
├── server/     the daemon composition root + lifecycle (registry file)
├── mcp/        MCP tool surface, over HTTP
├── api/        the REST API (health/status/jobs/search/recall/wake-up/diary/wings/rooms/drawers/auth-token/db/shutdown)
└── client/     the CLI's HTTP client for a running daemon, and the human renderings of its answers (status, tables)

integrations/   per-agent lifecycle adapters (Pi, OpenCode, ...), in each agent's own language, over MCP and HTTP only
skills/         reusable agent instructions shared by every integration; `tests/skills.rs` holds them to the
                tools, commands and routes they name (docs/skills.md)
tests/fixtures/integration/   the language-neutral conformance fixtures every integration is held to
```

Dependencies point inward: `cli / mcp / api -> app -> domain + store/jobs/search -> store`,
where the `cli` arrow is an HTTP call to the daemon through `client/`, not a Rust call into `app`.
Nothing in `domain` knows SurrealDB exists; nothing in `cli`/`mcp`/`api` knows `store` exists.

## Style

**Every non-obvious line carries a comment saying why.** Not what — the code says what.
Ideally naming the failure it prevents.
A comment that restates the code is worse than none.

**Errors are typed and actionable.** `thiserror` for the library, `miette` at the binary edge.
A diagnostic must carry the two things the user does not already know:
what specifically failed, and what to do about it.
Diagnostic codes are `memcastle::<module>::<kind>`,
and a code is a public identifier users grep for — renaming one is a breaking change.

**Test names are sentences.** `an_unchanged_input_produces_no_output`, not `test_run_2`.
The name should say what would be broken if it failed.

**Markdown prose uses semantic linefeeds.** One sentence per line; only wrap inside a sentence, at a clause boundary,
when it would otherwise exceed the 120-column limit `.markdownlint-cli2.yaml` enforces.
This keeps a diff scoped to the sentence that actually changed.
The rule applies to the linted documents (`AGENTS.md`, `CONTRIBUTING.md`, `README.md` and `docs/`);
`PLAN.md`, `integrations/README.md` and `skills/README.md` are working documents outside that lint scope.

**Documentation ships with the behaviour.**
A change to a CLI flag, a setting, an environment variable, an MCP tool, a REST route or any other user-visible behaviour
updates the page that documents it in the same change (`docs/cli.md`, `docs/configuration.md`, `docs/mcp-and-api.md`, ...).
Document what the code does, not what it will do, and diagram architecture with Mermaid.
See [Development](docs/development.md#documentation).

## Commits

Conventional Commits, enforced by commitlint on `commit-msg`.
The type becomes a changelog heading, so choose it as if someone will read it in release notes — because they will.

## Releases

Driven by gh-ship.
Never bump a version or push a tag by hand:
`cliff.toml` derives the version from the commit history, `prepare-release` applies it,
and `.github/ship.yml` is the contract between them.
See CONTRIBUTING.md.

## Before you push

```sh
mise run ci
```

Formatting, Clippy, spelling, workflow and Markdown linting, the architecture guard hooks, tests
and the documentation build.
This is the local equivalent of CI's lint, test and docs steps; CI additionally runs every prek hook, coverage on three
operating systems and `gh ship validate`.

## This repository is generated from a template

The toolchain, hooks, CI and release workflows come from [rust.tpl](https://github.com/noirbizarre/rust.tpl)
and are updated with `git tpl update`.
Files carrying template-owned content end with a `# --- project-specific ...` marker
(`mise.toml`, `prek.toml`, `Cargo.toml` and a few more): add below it, never above.

Changing template-owned content here fixes it in one repository.
Changing it in the template fixes it in all of them —
prefer that.
