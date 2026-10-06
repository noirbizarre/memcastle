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
   via `server::lifecycle` and spawn `serve` detached — but touch neither `store` nor `jobs`.
   `source init`, `build`, `test`, `package`, `index` and `keygen` work on a project directory or on archives with no
   daemon at all, through `crate::source`, which `tests/source_isolation.rs` holds to touching neither `store` nor `jobs`
   nor the network, and `source install` of a file or a project directory reads it locally to show its permissions before
   calling the daemon.
   `integration list`, `install`, `update` and `remove` work on files and the agent's own commands with no daemon at all,
   through `crate::integration`, which `tests/integration_isolation.rs` holds to touching neither `store`, `jobs`, the
   client nor the network, and which nothing but `main.rs` may call, so there is no MCP tool and no route that installs
   one (see `docs/adr/034-agent-integration-distribution.md`).
   `miner` (list, get, set, enable, disable, remove, reload, run) calls only `client::DaemonClient`, like every other
   daemon command: the rules for a miner live in `app::miners` (see `docs/adr/037-persistent-miner-configuration.md`).
   `source search`, `install <name>` and `update` never fetch anything themselves: the daemon does, through
   `crate::distribution`, and the CLI calls it over REST.
   `note` also reads the project directory through `crate::project` to choose a wing and room, then calls the daemon.)
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
   The one other exception is the web dashboard's static files: `GET`/`HEAD` of `/ui` and below, only when `web.enable`
   is set, named in `api::auth::is_public` and nowhere else (see `docs/adr/035-web-dashboard.md`).
   A token or secret is never logged, serialised or persisted in plaintext (`config::Secret` redacts it;
   the store holds only a digest) — see `docs/adr/014-optional-token-authentication.md`.
   Enforced by `tests/in_process/auth.rs`
   (every route and an unknown path refused without a token, `/ui` refused while the dashboard is off, no
   credential-named MCP tool), `tests/in_process/web.rs` (with the dashboard on, only `/ui` is open: `/uix`, every `/api`
   route and `/mcp` are still refused) and `tests/auth_lifecycle.rs` (no token in the log, the database or any file).
7. **The database admin endpoint is opt-in, loopback by default, and runs on the daemon's own handle** —
   only an explicit `memcastle db start` (REST `/api/db`) opens it, `serve` never does, and it never binds beyond loopback
   without `--allow-remote` *and* authentication.
   It serves each connection from a clone of the daemon's `Surreal<Any>`, never a second connection or process,
   and has no MCP tool — see `docs/adr/015-database-admin-endpoint.md`.
   Enforced by `tests/in_process/db_endpoint.rs` (a started daemon has no endpoint, unsafe binds are refused,
   foreign origins are refused, the endpoint sees and shares the daemon's data),
   by `tests/in_process/auth.rs` (`/api/db` is guarded, no MCP tool mentions the database)
   and by `tests/auth_lifecycle.rs` (no token in the log or any file).
8. **An integration is lifecycle glue over MCP and HTTP, and nothing else** —
   everything under `integrations/` decides *when* to call MemCastle and never reaches storage, the job code or the
   database admin endpoint, and every integration satisfies the same conformance matrix
   (`docs/integration-contract.md`, `docs/adr/019-shared-integration-contract.md`).
   Client lifecycle logic does not move into MemCastle to make two clients look alike.
   Enforced by the prek `integrations-http-only` hook: it fails on `surrealdb`, `surrealkv`, `SurrealStore`, a
   `store`/`jobs` path or `/api/db` anywhere under `integrations/` (Markdown and `node_modules` excepted),
   and by `tests/in_process/integration_contract.rs`, which replays `tests/fixtures/integration/` against a real daemon
   and fails when the contract page, the capability manifest and the test names disagree.

9. **Source-specific code stays out of the mining pipeline, and adapters stay out of storage** —
   everything that differs between mining sources (discovery, reading, normalizing) lives in one adapter under
   `src/mining/adapters/`, and the pipeline, the chunker and the domain model know no source by name and read no file.
   An adapter never touches the store or the jobs, and only the pipeline writes a source's cursor and document records,
   after the drawers they describe (see `docs/adr/023-unified-source-model-for-mining.md`).
   A source is built in or an installed WebAssembly component (`src/mining/wasm/`, the only module that names the
   runtime), behind the same contract, and the pipeline cannot tell which
   (see `docs/adr/026-pluggable-source-adapters-as-webassembly-components.md`).
   Sources arrive from a registry through `src/distribution/`, which only `app` calls
   (and `config`, only to parse a location at load), which touches neither
   the store, the jobs nor the runtime, and which returns bytes that have passed the index's SHA-256 and the trust policy
   (see `docs/adr/033-source-distribution.md`).
   A source that ships with MemCastle arrives from nowhere: it is unpacked beside the binary, installed from the start,
   and read in place by `src/mining/bundled.rs`, which touches neither the store, the jobs nor the network
   (see `docs/adr/039-bundled-sources-are-installed-from-the-start-and-the-official-registry-is-published.md`).
   Entity extraction is the stage after: it reads drawers, names no source, and only adds graph records, never writing a
   drawer (see `docs/adr/024-entity-extraction-as-an-enrich-job.md`).
   Enforced by `tests/source_isolation.rs`, which fails on an adapter name or file access in `pipeline.rs`, `chunk.rs`
   or `adapter.rs`, on an adapter reaching `store` or `jobs`, on any other module calling the source writers,
   and on `src/extract` naming a source or calling a drawer writer.
   Deduplication is called by the pipeline for every chunk it stores, so `src/dedup` obeys the same rule: it names no
   source, reads no file and writes no source record, and the matching policy in `domain/fingerprint.rs` and
   `domain/resolution.rs` stays pure (see `docs/adr/025-memory-deduplication-and-entity-resolution.md`).
   The host, the adapters and `src/source/` reaching `store` or `jobs`, and any module but `src/mining/wasm/` naming the
   runtime, fail the same test.

10. **An installed source has no ambient authority, and installing one is administrative** —
    a WebAssembly source starts with no filesystem, environment, network or programs, and the host grants exactly what its
    manifest lists and the user consented to at install (a digest of the name and the permissions), read-only for files,
    with a memory ceiling and a time limit.
    Its component is checked against the digest recorded at install on every load, and `unavailable` is computed,
    never stored.
    Install, enable, disable and remove are REST and CLI only, with no MCP tool, so an agent cannot install code or widen
    its own reach (see `docs/adr/026-pluggable-source-adapters-as-webassembly-components.md`).
    The same holds for searching, installing from and updating from a registry, which make the daemon fetch code:
    `mining.registries` is the official registry by default and is read only when a user runs one of those commands,
    never at startup, a package must match its index's SHA-256 (for an entry naming a GitHub repository, the digest
    GitHub reports for the release asset), pass the trust policy and carry the name and version the index lists,
    and an update that asks for permissions the installed version did not is never installed without consent
    (see `docs/adr/033-source-distribution.md`).
    A source that ships with MemCastle is the one exception to consent and trust, because it is the release itself:
    enabling it needs no digest, it is never fetched, installed, updated or removed through a registry
    (`memcastle::source::bundled`), and enabling it is still REST and CLI only
    (see `docs/adr/039-bundled-sources-are-installed-from-the-start-and-the-official-registry-is-published.md`).
    Enforced by `tests/source_isolation.rs` (the host calls nothing that inherits the environment, standard streams,
    arguments or a writable directory, and opens the network only inside the manifest's flag),
    by `tests/wasm_projects.rs` (a real source cannot read outside its grant, see the environment, outrun its time
    limit or memory, or run an unlisted program),
    by `tests/wasm_runtime.rs` (no install without the exact consent, an altered component is never run)
    and by `tests/in_process/auth.rs`
    (every `/api/source-packages` and `/api/source-registry` route is guarded, no MCP tool installs, searches or changes
    a source), by `tests/source_isolation.rs` (`distribution` reaches no store, jobs or runtime, only `app` calls it, and
    `config` only to parse a location, the
    local tooling and adapters open no network, the digest and trust checks are in `Registry::fetch` with no origin that
    skips them, and the bundle is read from disk and nothing else) and by
    `tests/wasm_registry.rs` (a tampered or substituted package is never installed, `required` trust refuses what no
    trusted key signed, an update never widens permissions silently, a registry never installs, updates or replaces a
    bundled source) and `tests/wasm_bundle.rs` (the release script's bundle is listed as installed and enabled without
    consent).

11. **The web dashboard is a client of the REST API and nothing else, and it is opt-in** —
    everything under `web/` reaches MemCastle only over HTTP, never storage, the job code or the database admin endpoint,
    exactly as an integration does (invariant 8), and the daemon serves it only when `web.enable` is set.
    It is found through the one assets root at `web/dist/`, in a checkout and in a package alike, and what it may do
    is what the REST API lets any client do: no shutdown, source installation or token management from the page.
    Its login is the database console's (the user `memcastle`, the token as the password), checked by the same layer on
    every request; the token lives in `sessionStorage` and is never put in a URL, a cookie or a log
    (see `docs/adr/035-web-dashboard.md`).
    Enforced by the `prek` `web-http-only` hook (it fails on `surrealdb`, `surrealkv`, `SurrealStore`, a `store`/`jobs`
    path or `/api/db` anywhere under `web/`, Markdown, `node_modules` and `dist` excepted),
    by `tests/in_process/web.rs` (off by default, no path leaves `web/dist`, the headers, a missing build is a 503 and
    never a failed start, `/api/config` carries no secret),
    by `tests/web_bundle.rs` (the package holds the build and nothing else, and a daemon serves it from an installed prefix
    and from the checkout through the same lookup)
    and by `web/test` (the login, the route guard and the client, with a real daemon).

12. **Changing a miner is administrative, and the daemon is the only writer of the `[[miners]]` section** —
    miner definitions live in the configuration file the daemon was started from, and adding, changing, enabling,
    disabling, removing, reloading and running one are REST and CLI only, so an agent can read the miners
    (`memcastle_miner_list`, `memcastle_miner_get`) but cannot decide what the daemon mines.
    A secret is never written to the file (`credential` is a reference, and a secret-looking key is refused) nor returned
    (a credential is shown as a kind and whether it resolves), and a change made through the daemon never widens a
    scope without being told to.
    The rest of the file is never rewritten, only that section, in place (see
    `docs/adr/037-persistent-miner-configuration.md`).
    Enforced by `tests/in_process/auth.rs` (every `/api/miners` route guarded, and the only miner MCP tools are the two
    read-only ones), by `tests/in_process/miners.rs` (validation before anything is written, a scope never widens
    silently, a credential never leaks, a cursor survives every change, REST, CLI and MCP agree)
    and by `config::miners_file`'s tests (comments and other tables survive an edit, a stale write is refused).

## Layout

```text
src/
├── main.rs     the memcastle binary: parses Cli, dispatches (and hosts `migrate`'s direct storage access)
├── cli.rs      argument types only
├── lib.rs      module wiring
├── error.rs    the crate's error type
├── term.rs     terminal presentation for the CLI: colour, TTY detection and confirmation prompts
├── config/     typed configuration (defaults -> file -> env -> CLI -> validate) and Unix XDG paths; `miners_file`
│               rewrites the `[[miners]]` section of the file in place and nothing else
├── domain/     Palace/Wing/Room/Drawer/Job, checkpoint payloads, entities, memory modes — pure types, no I/O
├── store/      SurrealDB connection and repository methods (schema is applied from `database/schema/`)
├── migrate/    versioned data migrations and the version watermark, run before serving
├── assets/     runtime asset resolution (override, installed, embedded); never user data, never the network
├── dbadmin/    the database admin endpoint: SurrealDB's WebSocket protocol over the daemon's own handle
├── jobs/       the scheduler: claiming, dispatch, cooperative pause/cancel, crash recovery
├── project.rs  the project-local `.config/memcastle.toml` and `MEMCASTLE_WING`/`MEMCASTLE_ROOM`: a directory read, shared by
│               the directory adapter and the CLI's `note`; no store, no jobs
├── mining/     the mining job handler: the source adapter contract, the shared pipeline and chunker, the built-in adapters
│               (the directory adapter alone reads a project's `.config/memcastle.toml`, for its default wing, through `project`),
│               the registry that names them, `bundled`, which reads the sources unpacked beside the binary, and the
│               WebAssembly host that runs installed sources (`wasm/`)
├── integration/ agent integrations: manifest, discovery under the assets root, install/update/remove with a receipt, and
│               the Pi and OpenCode adapters (no store, no jobs, no network, no daemon)
├── source/     source packages: manifest, archive, scaffolding, build, signing, publishing an index, and the conformance runner
│               (no store, no jobs, no network)
├── distribution/ finding and fetching source packages: registry indexes, locations, the SHA-256 and signature checks;
│               called only from `app` (and `config`, to parse a location), no store, no jobs
├── checkpoint/ the checkpoint job handler (durable, resumable memory writes)
├── audit/      the audit job handler (read-only consistency report)
├── repair/     the repair job handler (narrow, dry-run-first fixes)
├── search/     the retrieval contract and ranking policy (lexical, semantic, hybrid, temporal, graph expansion)
├── embed/      embedding providers (command, OpenAI-compatible HTTP) behind one trait, and the `Embed` job handler
├── dedup/      drawer deduplication: assess what a new drawer duplicates or resembles, link it, skip an exact copy
├── extract/    entity extraction providers (heuristic, command, OpenAI-compatible HTTP) behind one trait, and the `Extract` job handler
├── app/        application services — the one layer mcp/api call into (the CLI reaches it over HTTP, via `client/`);
│               `miners` reads and rewrites the configured miners
├── server/     the daemon composition root + lifecycle (registry file)
├── mcp/        MCP tool surface, over HTTP
├── api/        the REST API (health/status/config/jobs/search/recall/wake-up/diary/notes/wings/rooms/drawers/entities/graph/
│               sources/source-packages/source-registry/miners/auth-token/db/shutdown; `docs/mcp-and-api.md` lists every route),
│               and `web.rs`, the dashboard's static files under `/ui` (only when `web.enable`; no store, no jobs)
└── client/     the CLI's HTTP client for a running daemon, and the human renderings of its answers (status, tables)

wit/            the source contract (`memcastle:source`), the one definition components and the host are built from
sources/        official and reference WebAssembly sources (`directory`, `pi`, `opencode`), one package per directory, built and
                tested but not compiled into MemCastle; `pi` and `opencode` ship with releases (`packaging/sources/build.sh`)
integrations/   per-agent lifecycle adapters (Pi, OpenCode, ...), in each agent's own language, over MCP and HTTP only;
                each carries a `memcastle-integration.toml` and is bundled into `dist/` for `memcastle integration install`
web/            the web dashboard: a Vue 3 and OpenVue application, a client of the REST API, built into `web/dist/` and
                served under `/ui` from the assets root when `web.enable` is set (docs/web.md)
skills/         reusable agent instructions shared by every integration; `tests/in_process/skills.rs` holds them to the
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
The rule applies to the linted documents (`AGENTS.md`, `CONTRIBUTING.md`, `README.md`, `docs/` and `skills/*/SKILL.md`);
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

## Scope and cost

Doing what was asked is the job, and widening it costs the user time, money and a busy machine,
so staying inside the request is a rule here and not a courtesy.

- **Stay inside the request.**
  "Add tests for these two files" means those two files, not "close the coverage gap".
  When a request can be read broadly, do the narrow reading, say what was left, and ask before widening.
- **A broad task is fine when it is asked for.**
  If the user states it explicitly ("raise coverage across the patch", "get this module to 100%", "speed up the tests"),
  that is a focused task and the broad reading is the right one.
  State the plan and a rough time estimate first, then do it.
- **Let CI measure what CI measures.**
  Coverage comes from the Codecov comment on the pull request, which arrives minutes after a push and costs the machine
  nothing.
  Do not run `cargo llvm-cov` over the whole suite locally: it rebuilds the dependency tree instrumented, writes thousands
  of profile files and takes tens of minutes of every core.
  Run it only when the user asks for a coverage session.
- **Run what you touched.**
  While iterating, run the tests of the module or binary you changed
  (`mise run test -- <filter>`; that is the basic suite, and the `tests/wasm_*.rs` binaries are
  `mise run test:wasm -- <filter>`).
  Run the full suite once, before pushing.
- **Two strikes, then ask.**
  If the same approach fails twice (a tool, a flag, a workaround), stop and ask rather than building a third one.
- **Estimate before anything heavy.**
  Describe the cost first, and wait for a yes, for anything expected to take more than about ten minutes or to saturate
  the CPU: instrumented builds, repeated full-suite runs, many WebAssembly builds.
- **Do not promise before knowing.**
  Do not call work easy or small until its size is known.
  When it turns out larger than said, report the real size and offer to stop.

## Before you push

```sh
mise run ci
```

Formatting, Clippy, spelling, workflow and Markdown linting, the architecture guard hooks, the basic and WebAssembly
tests, the build and test of every reference source and integration (which needs bun and the `wasm32-wasip2` target)
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
