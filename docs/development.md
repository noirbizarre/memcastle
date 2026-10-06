# Development

## Prerequisites

- Node and bun, only for `web/` and `integrations/` (`mise run web:check` and `integrations:check` install them for the
  task; the Rust build needs neither).
- Rust, via `rustup`, which installs the channel pinned in `rust-toolchain.toml` (mise does not manage Rust itself).
- The remaining tools (nextest, prek, typos, ...) via `mise` — run `mise install` once.

That's it — SurrealKV, the embedded storage engine, is pure Rust,
so the storage engine itself needs no C/C++ toolchain.
(A transitive TLS dependency, `aws-lc-sys`, may use `cmake` on some targets; that is unrelated to storage.)

## Everyday tasks

```sh
mise run build      # cargo build
mise run test       # the basic suite: cargo nextest run, without the WebAssembly tests (accepts nextest selectors)
mise run test:wasm  # the WebAssembly suite: every tests/wasm_*.rs binary (slow; needs the wasm32-wasip2 target)
mise run sources:check      # build and conformance-test every source under sources/
mise run sources:test -- directory   # the same for one source, which is what each CI leg runs
mise run lint       # cargo clippy --all-targets --all-features -- -D warnings
mise run format     # cargo fmt --all
mise run guards     # the architecture guard hooks, described below
mise run integrations:check # typecheck and test each package under integrations/, and install the bundles (needs bun)
mise run integrations:build # bundle integrations/*/src into integrations/*/dist, for `--assets-dir "$PWD"`
mise run integrations:package # lay out what a release ships in target/bundled-integrations
mise run web:check  # typecheck, test, build and package the web dashboard, then serve it from a real daemon (needs bun and node)
mise run web:build  # build web/ into web/dist, for `--assets-dir "$PWD"` with `web.enable`
mise run web:dev    # the dashboard's dev server, proxying /api to the running daemon
mise run web:package # lay out what a release ships in target/bundled-web
mise run eval       # measure retrieval quality and latency on the bundled dataset (docs/retrieval-evaluation.md)
mise run eval:baseline # rewrite the committed retrieval baseline, after a change meant to move quality
mise run eval:compare -- a.json b.json # what changed between two retrieval reports
mise run eval:longmemeval -- file.json # session retrieval on a downloaded LongMemEval file
mise run check      # every lint, the guards, both test suites, the sources, the integrations and the dashboard, without modifying the tree
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
mise cli job list
mise cli job demo --steps 5   # exercise the scheduler without mining anything
mise cli search "job scheduler"
mise cli daemon stop
```

In this repository `mise.toml` sets development defaults in its `[env]` table:
`MEMCASTLE_ASSETS_DIR` is the checkout and `MEMCASTLE_WEB_ENABLE` is `true`.
They apply to `mise run`, to `mise cli` and to any shell with [mise activated](https://mise.jdx.dev/dev-tools/shims.html)
once you are in the project directory, so `mise cli serve` already serves the dashboard from `web/dist` (after
`mise run web:build`) and finds the integrations and sources of the checkout, with no flag.
Because the variables follow the shell, **any `memcastle` you run from the project directory uses them, including an
installed one**: `memcastle integration install pi` there installs the bundle built in the checkout
(`mise run integrations:build`) and not the package's.
Run it from another directory, or `env -u MEMCASTLE_ASSETS_DIR memcastle ...`, to use the installed assets.
A flag still wins over the variable (`--assets-dir`), and your own `mise.local.toml` can override or drop either.

The suites that start a daemon or run the installer are not affected:
`mise run integrations:check` and `mise run web:check` unset every `MEMCASTLE_*` variable before they start,
and the Rust and bun harnesses build their own environment, so a test never depends on where it was run from.

By default the palace lives under `~/.local/share/memcastle/default` and the daemon listens on `127.0.0.1` port `8420`.
Every path, environment variable, flag and the precedence between them is in [Configuration](configuration.md).
`config::Config` is the source of truth if that page ever drifts.

## Testing

- **Unit tests** live next to the code they test (`domain::job`'s state machine, `config`'s validation,
  `store`'s migrations/persistence — the storage tests use SurrealDB's in-memory engine for speed, plus one test
  against a real SurrealKV directory; survival across a restart is proven at the process boundary in
  `tests/persistence.rs`, because SurrealKV's file lock is not released within one process).
- **Two suites.** A test binary that builds or runs a WebAssembly component is named `tests/wasm_*.rs`,
  and that prefix is the whole definition of the WebAssembly suite: `.config/nextest.toml` selects it with
  `binary(/^wasm_/)`, and `mise run test:wasm` and CI's `wasm` job pass the same prefix to Cargo as `--test 'wasm_*'`,
  so that the other test binaries are not compiled for nothing.
  A new `wasm_` file therefore joins the suite with no configuration.
  Everything else is the basic suite, which is what `mise run test` and the `default` and `ci` nextest profiles run;
  the `wasm` and `ci-wasm` profiles run the other one.
  The basic suite runs in CI on Linux, macOS and Windows and needs no WebAssembly target.
  The WebAssembly suite runs in CI on Linux only, in its own `wasm` job with its own Codecov flag,
  and each directory under `sources/` also gets a CI leg of its own (`mise run sources:test -- <name>`), found automatically.
  A test that only reads files, such as `tests/source_docs.rs`, builds nothing and stays in the basic suite.
- **Integration tests** (`tests/`) run against a tempdir palace and an OS-assigned port.
  Those that start the daemon inside the test process are modules of one binary, `tests/in_process/main.rs`:
  each `tests/` binary links the whole server, about 600 MB with debug info, so sixteen of them were most of what
  compiling the tests took, and one links it once.
  To add one, create `tests/in_process/<name>.rs` and list it in `main.rs`.
  nextest still runs every test in a process of its own, and a test is named `in_process <module>::<test>`,
  so `mise run test -- -E 'binary(in_process) & test(auth::)'` selects one file's tests.
  A test that needs a process boundary (a daemon that is killed, a CLI with no daemon) stays a binary of its own.
  Most start the daemon in-process (`tests/common`'s `TestDaemon`); the ones that need a real process boundary
  (SurrealKV's file lock is not released within one process) spawn the `memcastle` binary instead:
  - `tests/in_process/server.rs` — health, status, graceful shutdown, retry (in-process).
  - `tests/in_process/concurrency.rs` — many simulated clients submitting jobs and reading status at once,
    proving the shared store stays consistent (in-process).
  - `tests/in_process/memory_mode.rs`, `tests/in_process/mcp_memory_mode.rs` — per-request and per-MCP-session
    memory modes (in-process).
  - `tests/in_process/audit.rs`, `tests/in_process/repair.rs` — the audit and repair job kinds end to end (in-process).
  - `tests/persistence.rs` — data and job state survive a daemon restart, including a SIGKILL mid-job
    and a pause or cancel requested just before it, `memcastle daemon start`
    and `memcastle daemon restart --bind --port` (subprocess).
  - `tests/mcp_smoke.rs` — the MCP surface end to end: a real MCP client over streamable HTTP against a
    `memcastle serve` subprocess, covering read and write tools, migrations before serving, a second session,
    and persistence across a restart with a reconnecting session (subprocess).
  - `tests/in_process/auth.rs` — optional bearer-token authentication over real HTTP: every route guarded,
    the open health probe, a configured secret, generate, rotate and revoke,
    and MCP with a token and with no credential-management tool
    (in-process).
  - `tests/auth_lifecycle.rs` — the authentication lifecycle through the real CLI: generate, restart, enable, a
    rotated and a revoked token, the daemon refusing to start with nothing to check against, and no token ever reaching
    the log, the database or any file (subprocess).
  - `tests/config_assets.rs` — the assets override is honoured and checked, and a standalone binary starts with no assets
    (subprocess).
  - `tests/dependencies.rs` — the lockfile never pulls a second storage engine into the binary (ADR-001), nor a
    source-specific SDK or native database binding (ADR-026), and the WebAssembly runtime stays built without the
    features a source host never uses.
  - `tests/config_bind.rs` — the listener itself: bind address and port precedence, and real bind failures
    (subprocess).
  - `tests/config_paths.rs` — the XDG config, data and state locations, resolved through the real binary
    (subprocess).
  - `tests/migrate.rs` — `memcastle migrate` and its `--check`/`--status` modes (subprocess).
  - `tests/cli.rs` — the binary's argument parsing and its behaviour with no daemon reachable (subprocess),
    including colour, `completions` and the absence of a `help` subcommand.
  - `tests/in_process/retrieval_eval/` — the retrieval evaluation harness ([Retrieval evaluation](retrieval-evaluation.md)):
    the bundled dataset against its committed baseline, no result outside its scope or time, and the metric, report and
    LongMemEval converter units (in-process). Its explicit runs (`mise run eval*`) are `#[ignore]`d.
  - `tests/in_process/palace.rs` — the `/api/wings/...` hierarchy routes: lifecycle, error contract and memory-mode gates
    (in-process).
  - `tests/shutdown.rs` — a stopping process lets the embedded datastore finish stopping before it exits (subprocess).
  - `tests/in_process/db_endpoint.rs` — the database admin endpoint: opt-in, loopback by default, refused origins,
    and sharing the daemon's data (in-process).
  - `tests/in_process/miners.rs` — persistent miner configuration end to end: several miners with their own scope, creation
    and changes validated before anything is written, a scope never widening silently, hand edits noticed and an invalid
    one keeping the last good miners, a cursor surviving every change that keeps the source, a credential never shown, and
    REST, the CLI and MCP agreeing (in-process, with the CLI as a subprocess client).
  - `tests/in_process/cli_daemon.rs` — CLI flags that change what the daemon is asked: `--mode`, and relative `mine` paths
    against an in-process daemon (subprocess client).
  - `tests/in_process/integration_contract.rs` — the daemon half of the [integration contract](integration-contract.md):
    a real MCP session replays the language-neutral fixtures in `tests/fixtures/integration/`
    (modes, checkpoint payloads, failure classes), and fails when the contract page, the capability manifest
    and the test names disagree (in-process).
  - `src/project.rs` — the project file and the `MEMCASTLE_WING`/`MEMCASTLE_ROOM` overrides, replaying the cases in
    `tests/fixtures/project-config/` (unit tests): discovery, nesting, the git and home boundaries, the environment,
    and what is refused.
    The integrations replay the same cases in TypeScript, see [Project configuration](project-config.md).
  - `integrations/<name>/test/` — the client half of the same contract, one bun suite per integration:
    each starts a real `memcastle serve`, finds it through its registry file and replays the same fixtures through
    the integration's own client.
    Run them with `mise run integrations:check`, see [ADR-022](adr/022-integrations-are-bun-packages-tested-against-a-real-daemon.md).
  - `tests/integration_cli.rs` — `memcastle integration list|install|update|remove` through the real binary, with fake
    `pi` and `opencode` programs and a throwaway home: the lifecycle, idempotence, compatibility refusals,
    preservation of the user's other plugins and settings,
    and resolution from `--assets-dir`, the environment and the binary's own prefix.
    The installer's unit tests are in `src/integration/` (manifest, discovery, adapters, install, update, remove, rollback).
  - `tests/integration_bundle.rs` — builds the bundles with `packaging/integrations/build.sh`, checks the package tree
    (manifests accepted by this MemCastle, no sources or `node_modules`, the skills) and runs the same lifecycle from the
    packaged tree and from the checkout.
    It needs bun, so it is `#[ignore]`d in the basic suite and `mise run integrations:check` runs it.
  - `integrations/common/test/` — what needs two integrations at once: sessions in every memory mode against one
    daemon, and the proof, from the requests on the wire, that an `off` session receives nothing.
    It is test-only and imports the integrations' sources, see
    [ADR-027](adr/027-cross-integration-tests-live-in-a-common-package.md).
  - `tests/in_process/web.rs` — the dashboard's routes against a real daemon: opt-in, the static files and their headers,
    the refusal of any path that leaves `web/dist`, the page for a build that is missing, the public shell with
    authentication on, `/api/config`, and the jobs listing's `kind` and `limit` (in-process).
    `src/assets` unit-tests that a packaged layout and a worktree resolve `web/dist/index.html` through the same lookup.
  - `tests/web_bundle.rs` — builds the dashboard with `packaging/web/build.sh`, checks the package tree (no sources, maps
    or `node_modules`) and serves it from an installed prefix and from the checkout.
    It needs bun and node, so it is `#[ignore]`d in the basic suite and `mise run web:check` runs it.
  - `web/test/` — the dashboard's client, login, route guard and polling (vitest, no daemon), and the same client and login
    against a real `memcastle serve` (`web/test/daemon`, bun), see [Web dashboard](web.md#developing-the-dashboard).
  - `tests/wasm_conformance.rs` — the same conformance cases run against the built-in `directory` source and against the
    reference WebAssembly source built from `sources/directory/` (in-process, WebAssembly suite).
  - `tests/wasm_pi.rs` — the Pi history source (`sources/pi/`) built and run as a component: what it files and leaves out,
    discovery and the credentials file it must never open, provenance, and an install, mine and re-mine against a real
    daemon (in-process, WebAssembly suite).
  - `tests/wasm_opencode.rs` — the OpenCode history source (`sources/opencode/`) built and run as a component against a
    stand-in `opencode` command: what it files and leaves out, the discovery cursor and the query it never lets a
    cursor change, provenance, a machine without OpenCode, and an install, mine, re-mine and grow against a real daemon
    (in-process, WebAssembly suite, Unix only).
  - `tests/source_docs.rs` — the guide's list of conformance cases and of diagnostic codes against what ships
    (no build, basic suite).
  - `tests/wasm_registry.rs` — registries against a real daemon (a directory registry written with the same calls
    `source index` makes): search, install by name with consent, an archive that is not the one published, a package that
    is not the one the index says, version choice, the trust policy, update with and without new permissions, a bundled
    source that a registry never installs, updates or replaces, an unreadable registry, and the CLI publishing, signing,
    searching, installing and updating (in-process and subprocess, WebAssembly suite).
  - `tests/wasm_bundle.rs` — `packaging/sources/build.sh`, the script a release runs, packaging `pi` and `opencode` as an
    unpacked bundle, archives and a registry index (absolute URLs, a published version never indexed again), and a daemon
    that lists both as installed and enables them with no consent (WebAssembly suite, Unix only).
  - `tests/wasm_runtime.rs` — installable sources against a real daemon: consent, the lifecycle, mining through the
    shared pipeline, and refusing a component altered on disk (in-process, WebAssembly suite).
  - `tests/wasm_projects.rs` — `memcastle source init`, `build`, `test` and `package` for real, the sandbox
    (a source cannot read outside its grant, see the environment, outrun its time limit or exceed its memory, or run an
    unlisted program) and installing through the CLI.
    It builds components with Cargo for `wasm32-wasip2` (subprocess, WebAssembly suite);
    see [Building components in tests](#building-components-in-tests).
  - `tests/in_process/skills.rs` — the shared [agent skills](skills.md): every skill is discoverable,
    and every tool, CLI command and REST route it names exists in this release
    (in-process daemon, plus the real binary's help).

Run a subset with nextest's filter syntax, e.g.:

```sh
mise run test -- --filter-expr 'test(job)'
```

A filter on `mise run test` still excludes the `wasm_` binaries; use `mise run test:wasm -- <filter>` for those.
`mise run cover` runs the basic suite with coverage.

### Where the CI time goes

Measured on GitHub's runners after the suites were split, so that a change aimed at speed has a baseline to beat.
Compiling is the test binaries built instrumented for coverage; running is nextest's own summary line.
The figures are from runs that restored their caches; the first run of a new job, or one whose cache was evicted,
compiles `wasmtime` and Cranelift from scratch and takes several minutes longer.

| Job | Compiling | Running | Whole job |
|---|---|---|---|
| Tests, Linux (basic, 1119 tests) | 46 s | 224 s | 5.4 min |
| Tests, macOS (basic) | 25 s | 67 s | 2.5 min |
| Tests, Windows (basic) | 75 s | 215 s | 7.3 min |
| WebAssembly tests, Linux (38 tests) | 10 s with warm caches, 2.8 min cold | 38 s | 1.5 to 4.3 min |
| Source, one leg per `sources/<name>/` | 10 s | seconds | about 1 min |

The WebAssembly tests run in under a minute in CI, and their job finishes well before the basic Linux one.
Both suites build only the test binaries they run: the wasm job passes `--test 'wasm_*'` to Cargo,
and the daemon tests that start a server in the test process are one binary, `tests/in_process`, rather than sixteen.
The time left in the basic suite is its roughly 1100 tests, not compiling or WebAssembly.

#### Building components in tests

Under nextest every test is its own process, so a component built by one test is invisible to the next, and the cost
is how many times Cargo has to compile what.
The helpers in `tests/common/wasm.rs` keep that to one small crate per build:

- Every `wasm_*` test builds into one shared debug target directory, so `wit-bindgen` and `serde_json` compile once.
- Every scaffolded project is seeded with the reference source's `Cargo.lock`.
  A scaffold ships none, and without it each build updated the crates.io index and re-resolved 35 packages while holding
  Cargo's package-cache lock, which every other build then waited for.
- The reference source is built in place, in debug, into that shared directory, so a test never waits for the
  release build with LTO that `mise run sources:check` and CI's per-source job make.
- A test that drives `memcastle source init` must call `share_target_of` afterwards.
  Without it the project builds in release into a `target/` of its own and recompiles every dependency, which used to be
  the two slowest tests in the suite.

Locally this took the WebAssembly suite from about 225 s to 95 s on a cold target directory and from about 92 s to 38 s
on a warm one; in CI, on a warm cache, the tests went from 49 s to 31 s and the slowest from 25 s to 14 s.
What is left of the job is compiling the instrumented test binaries.

## The architecture guard

The non-negotiable invariant in `AGENTS.md` — "the CLI has no business logic MCP/HTTP can't reuse" —
is enforced by the `store-isolation` `prek` hook.
It greps `src/main.rs`, `src/cli.rs`, `src/client/`, `src/mcp/` and `src/api/`
for a direct `store` or `jobs` import (including grouped `use crate::{store::..}` imports,
even when rustfmt spreads them over several lines).
The one allowed exception is `main.rs`'s `SurrealStore` import, which `memcastle migrate` needs.
The pattern matches text, not syntax,
so a diagnostic-code string such as `memcastle::jobs::not_found` trips it too, even inside a comment:
tests should read a code from the error (`Error::body().code`) rather than spell it out in these directories.
If you find yourself wanting to import `store` from one of those,
the fix is almost always to add a method to `app::AppServices` instead,
so the same capability becomes available to every interface at once.

The `single-writer` hook enforces the companion invariant, one daemon and one writer per embedded palace:
`SurrealStore::connect` may only be called from `src/server/`, `src/store/` and `src/main.rs` (for `migrate`).

The `integrations-http-only` hook keeps an integration to MCP and HTTP:
it fails on `surrealdb`, `surrealkv`, `SurrealStore`, a `store` or `jobs` path, or `/api/db` anywhere under `integrations/`
(Markdown and `node_modules` are skipped), see [ADR-019](adr/019-shared-integration-contract.md).
Like the others it is a text match, so a comment that names one of them trips it too.

The authentication invariant has no hook, because a route or an MCP tool is not something a grep can recognise.
It is enforced by tests instead: `tests/in_process/auth.rs` walks a list of the REST routes
(and a route that does not exist) without a token, and fails if an MCP tool's name mentions credentials.

The database admin endpoint ([ADR-015](adr/015-database-admin-endpoint.md)) is guarded the same way:
a daemon that was only started has no second listener, a non-loopback bind needs the opt-in and authentication,
a page from another origin is refused, and no MCP tool mentions the database.
Those are `tests/in_process/db_endpoint.rs`, the `/api/db` entries in `tests/in_process/auth.rs`
and a unit test that `serve` has no flag that starts it.
`surrealdb-core` and `surrealdb-rpc` are pinned to the exact `surrealdb` version: they are SurrealDB internal API,
so bump all three together.

Invariant 9 (source-specific code stays out of the pipeline) and invariant 10 (a source package has no ambient authority)
have no hook, because what they forbid is not something a grep over the whole tree can recognise.
`tests/source_isolation.rs` reads the source text instead: the pipeline, the chunker and the adapter contract name no
adapter, no file access and no WebAssembly runtime; adapters, the WebAssembly host and `src/source/` never reach the store
or the jobs (at any depth); only `src/mining/wasm/` names the engine; and the host calls nothing that hands a guest the
daemon's environment, standard streams, arguments or a writable directory.
Building the reference WebAssembly sources needs the `wasm32-wasip2` target (`rustup target add wasm32-wasip2`, which
`rust-toolchain.toml` requests); `mise run sources:check` builds and tests every source under `sources/`.
`tests/source_isolation.rs` builds nothing and runs in the basic suite on every OS.

Invariant 12 (changing a miner is administrative, and the daemon is the only writer of the `[[miners]]` section) has no
hook either.
`tests/in_process/auth.rs` guards every `/api/miners` route and fails when any MCP tool other than `memcastle_miner_list`
and `memcastle_miner_get` mentions miners; `tests/in_process/miners.rs` holds the rest, and `config::miners_file`'s unit
tests hold that an edit keeps the file's comments and other tables ([ADR-037](adr/037-persistent-miner-configuration.md)).

The integration installer is local tooling like the source tooling, and is held to the same rule in the same way.
`tests/integration_isolation.rs` fails when `src/integration/` mentions the store, the jobs, the daemon client, the
network or an agent's settings file or a credential, and when anything under `src/mcp`, `src/api`, `src/app` or
`src/server` (or any module but the binary) calls it.
`tests/integration_docs.rs` holds [Agent integrations](integrations.md) to the manifest parser and the error codes.

The web dashboard is held to HTTP as an integration is: the `web-http-only` hook fails on `surrealdb`, `surrealkv`,
`SurrealStore`, a `store` or `jobs` path, or `/api/db` anywhere under `web/` (Markdown, `node_modules` and `dist` are
skipped), see [ADR-035](adr/035-web-dashboard.md).
That the dashboard's static files are the only thing besides the liveness probe that needs no token is held by
`tests/in_process/auth.rs` and `tests/in_process/web.rs`, which walk the routes with the dashboard off and on.

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
  (`mise run lint:md` enforces the width on `AGENTS.md`, `CONTRIBUTING.md`, `README.md`, `docs/` and `skills/*/SKILL.md`).
  `mise run spell` runs `typos`; a legitimate new term goes into `typos.toml`'s `extend-words`.

## This repository is generated from a template

See `CONTRIBUTING.md` for the `git tpl` workflow (`mise run tpl:diff`, `mise run tpl:update`),
and `AGENTS.md` for where project-specific content goes so template updates keep merging cleanly.
