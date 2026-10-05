# ADR-026: Mining sources are pluggable as WebAssembly components behind the one adapter contract

## Status

Accepted, builds on [ADR-023](023-unified-source-model-for-mining.md) (the `SourceAdapter` contract is the logical API;
this decides how an implementation is loaded),
[ADR-004](004-versioned-database-migrations.md) (the one new table is SurrealKit schema),
[ADR-014](014-optional-token-authentication.md) and [ADR-015](015-database-admin-endpoint.md) (administrative operations
are REST and CLI, never MCP),
and [ADR-019](019-shared-integration-contract.md) (a conformance suite both implementations must pass).
It adds a registry of installed sources, which ADR-023 left open ("nothing precludes a registry later").

## Context

[ADR-023](023-unified-source-model-for-mining.md) made a source one small adapter behind a shared pipeline.
It left the adapters inside the binary: adding one meant a file under `src/mining/adapters/`, an arm in `mining::run`,
and a release.
That does not scale to the sources people actually ask for (Slack, Claude, GitHub, Atlassian, a local chat database),
for three reasons.
Each brings an SDK or a native database binding that every MemCastle build would then compile, for the sake of one origin.
Each is maintained by someone who is not the maintainer of the daemon, and should not need the daemon's release cycle.
And each runs code the user did not write, against the user's files and credentials, so *where it runs and what it may
touch* is a security question the contract alone does not answer.

The requirements pull in two directions.
The core must not compile source-specific dependencies, and third parties must be able to ship a source.
But the contract, the pipeline, identity, cursors, idempotency and the durable job must stay MemCastle's, identical for
a source whoever wrote it.

## Decision

- **One logical contract, two implementations.**
  `SourceAdapter` stays exactly the contract of ADR-023.
  A built-in source implements it natively; an installed source implements it as a WebAssembly component that
  `mining::wasm::WasmAdapter` adapts to the same trait.
  `mining::registry` turns a provider name into an `AnySource` (an enum: the two built-ins and the WebAssembly adapter),
  so `pipeline::mine` is untouched, and still names no provider and reads no file.
  The only change to the trait is that `provider`, `description` and `default_room` return `&str` rather than
  `&'static str`, so a loaded source can own its name.

  ```mermaid
  flowchart TB
      J["Mine job"] --> R["mining::registry<br/>name to AnySource"]
      R --> N1["directory<br/>native"]
      R --> N2["pi-sessions<br/>native"]
      R --> W["WasmAdapter<br/>sandbox per call"]
      W --> C["installed component<br/>.wasm + manifest"]
      N1 --> P["pipeline::mine<br/>normalize, chunk, ingest, cursor"]
      N2 --> P
      W --> P
  ```

- **The component interface is WIT, not a bespoke ABI.**
  `wit/memcastle-source.wit` defines the `memcastle:source` world: it exports `adapter` (`identify`, `default-wing`,
  `default-room`, `discover`, `read`, `normalize`, the same six methods) and imports `host` (`run-process`).
  Cursors and metadata cross as JSON text, so the contract does not change when a source carries a richer cursor.
  `description` and the capabilities are the manifest's, not calls: they are needed to list a source that is disabled.
  Components are `wasm32-wasip2`: Rust compiles to one directly, and TypeScript and Python go through a component
  toolchain that embeds their runtime.
- **The contract is versioned and compatibility is a policy, not a hope.**
  `CONTRACT_VERSION` (`0.1.0`) is the version of the WIT file.
  Before `1.0` a source runs only on a host of the same minor version; from `1.0`, on any host of the same major whose minor
  is at least the source's; a patch version never matters.
  A source also declares the MemCastle versions it runs on as a semver requirement.
  One that fails either check is `unavailable`, with the reason, and is never loaded.
- **A source is a package: a manifest and a component.**
  `memcastle-source.toml` states identity (name, version, description), compatibility, capabilities (the same three as a
  built-in adapter), permissions, resource limits, and, for development only, how to build and test it.
  Every table refuses unknown keys, so a misspelt permission is an error rather than a source that silently lacks it.
  The distributable is a `tar.gz` of the manifest and `source.wasm`, deterministic so a digest can be published, and read
  by exact top-level file name only.
- **Nothing is ambient: permissions are explicit, agreed to, and enforced by the host.**
  A guest starts with no filesystem, no environment, no network and no programs.
  The manifest may ask for: read access to the directory being mined (`locator`) or to named absolute directories;
  the network, all or nothing; named programs through `run-process`, by exact name with no shell; and named
  environment variables.
  There is no write access to the filesystem.
  Every grant is built in one function (`host::state`) from the manifest's permissions and nothing else, and
  `normalize` runs with none, so its purity is enforced rather than requested.
  Memory is capped, and one call's time is bounded by the engine's epoch interruption, so a source stuck in a loop is
  stopped rather than trusted to return.
  Each call gets a fresh store, which is what keeps a source stateless.
- **Installing needs consent to exactly those permissions.**
  `source install` shows what the package asks for and asks.
  The daemon recomputes a digest of the name and the normalized permissions from the uploaded package and installs it
  only if the caller sent that digest, so consent cannot be reused for another source or after an upgrade that asks for
  more.
  Without a terminal the CLI never consents on its own behalf: a script passes `--yes` or the reviewed `--consent`.
  A package that asks for nothing needs no consent.
- **Installed sources have a lifecycle, and `unavailable` is computed, not stored.**
  `installed`, `enabled` and `disabled` are stored, and change only through `SourcePackageState::apply`, a table-driven
  state machine like a job's (AGENTS.md invariant 2: one function, every pair tested).
  `unavailable` is a fact about the files and the running MemCastle (a missing or altered component, an incompatible
  contract), so it is derived on every look and could never go stale.
  The component's SHA-256 is stored at install and checked at every load: a file swapped on disk is `unavailable`, not
  run.
  Built-in sources are always enabled and cannot be disabled, replaced or removed.
- **The registry is local, and installing is administrative.**
  Installed packages live under `mining.sources_dir` (by default under the XDG data directory); the state and the
  agreed manifest live in the `source_package` table (SurrealKit schema, no data migration).
  The daemon runs on the stored manifest, never on the copy beside the component.
  Install, enable, disable and remove are REST (`/api/source-packages`) and CLI, with no MCP tool, so an agent cannot
  install code or widen its own reach, and are not gated by a memory mode, which guards access to memory.
  Listing and showing a source are reads and are gated as `GET /api/sources` is.
- **Development is a command group that needs no daemon.**
  `memcastle source init --template rust|typescript|python|cli` scaffolds a project that builds and passes the
  conformance cases at once; `build` runs the manifest's build command and checks the result is a component, not a
  module; `test` runs the conformance cases against the built component in the same sandbox; `package` writes the
  archive and reads it back.
  These touch neither the store nor the jobs, a third narrow exception to invariant 1 beside `serve`/`migrate`,
  enforced by `tests/source_isolation.rs`.
  The `cli` template is a component that wraps a command-line program through `run-process`, which is how a source
  written as a script is packaged behind the same contract.
- **One conformance suite, run against both kinds.**
  `tests/fixtures/sources/conformance/` holds language-neutral cases (a tree, the page size, the documents and canonical
  fields expected).
  `source::conformance` runs them against any `SourceAdapter`:
  Paging resumes strictly after a cursor, a foreign cursor is `cursor_invalid`,
  revisions and normalization are stable, and nothing is left after the last cursor.
  The native `directory` source and the reference WebAssembly source in `sources/directory/` pass the same cases, and
  `source test` runs them for a third party's.
- **The reference sources live under `sources/` and are not part of the build.**
  One directory per source, each a complete package.
  They are maintained as examples and as the second implementation the conformance suite runs against.
  A source that ships with MemCastle's releases, without being compiled into the binary, uses exactly the package
  contract a user installs; none was bundled by this decision, and
  [ADR-033](033-source-distribution.md) bundles `pi` and `opencode` and defines how packages are found and updated.
- **Enforcement is tests and a lockfile check.**
  `tests/source_isolation.rs`: only `src/mining/wasm/` names the engine;
  the host and the adapters never reach the store or the jobs (recursively);
  the pipeline names no runtime;
  the host calls nothing that hands over ambient authority
  (inherited environment, standard streams, arguments, a writable directory),
  and opens the network only inside the manifest's flag.
  `tests/dependencies.rs`: no source-specific SDK or native database binding in `Cargo.lock`, and the engine is built
  without the features a source host never uses.
  `tests/in_process/auth.rs`: every `/api/source-packages` route is guarded and no MCP tool installs or changes a source.

## Alternatives rejected

- **A bespoke raw-WASM ABI, or a plugin framework such as Extism.**
  The component model and WIT are the standard, typed, language-neutral way to describe this boundary, with bindings
  generators for the languages sources are written in.
  A hand-written ABI would be ours alone to document, version and port.
- **Native dynamic libraries.**
  They run with the daemon's full authority and tie a source to MemCastle's compiler and ABI.
  That is the opposite of every requirement above.
- **A source as an external process speaking JSON over standard input.**
  It is the simplest thing and the weakest sandbox: the process has the user's full authority, and "what may it touch" is
  unanswerable.
  The need it serves (a script as a source) is met inside the model: the `cli` template is a component whose permissions
  list the programs it may run.
- **Making every built-in source WebAssembly.**
  `directory` and `pi-sessions` are small, native, and in the hot path of every mine; a sandbox buys nothing for code
  that ships in the binary.
  The contract is logical, and the conformance suite is what keeps the two kinds from drifting.
- **A host allow-list for the network.**
  It needs `wasi:http` outbound filtering, which is not what a `wasm32-wasip2` component's sockets go through.
  All-or-nothing is honest about what is enforced today and is stated in the consent prompt; the manifest format leaves
  room for a list later.
- **Gating install by a memory mode.**
  The mode is a session's privilege over memory (ADR-007: the gate follows data access).
  Installing a source reads and writes no memory; what protects it is that it is administrative, consented to, and absent
  from MCP.
- **Storing `unavailable`.**
  It would be a cache of a filesystem fact the database cannot observe, wrong the moment a file changed.

## Consequences

- The core links `wasmtime` and `wasmtime-wasi`, built without default features.
  The release binary and the cold build grow, and the engine's Rust version sets the floor: `41` is the newest line that
  supports the crate's MSRV of 1.90, and moving to a newer one is a deliberate MSRV bump.
  Cranelift is optimised even in debug builds (`[profile.dev.package.*]`), because compiling a component unoptimised takes
  tens of seconds.
- A component is compiled when first used and cached by digest, so the first mine after a daemon start pays for it.
- Sources are local to a daemon: with a shared remote palace, each daemon has its own `sources_dir`.
  A daemon without the files reports the source `unavailable` rather than failing, which is why that state exists.
- The host's network permission is all or nothing, there is no filesystem write, and a source that needs a credential
  reads it from an environment variable it listed.
  Credentials stay references ([ADR-023](023-unified-source-model-for-mining.md)): a value is only ever in the process
  environment the user started the daemon with.
- TypeScript and Python sources embed a language runtime, so their components are megabytes larger and slower to start
  than a Rust one, and only their standard library (plus pure packages bundled with them) is available.
  Their scaffolds are provided and documented, and checked only structurally by the suite: the toolchains are not
  installed in CI.
- A changed WIT contract is a new contract version,
  and every installed source of the old one becomes `unavailable` with a reason that says to rebuild it,
  never a mysterious failure at the first mine.
- Adding a built-in source is still one file under `src/mining/adapters/`, one variant in `registry::AnySource` and an
  entry in `registry::BUILTIN_NAMES`, and a section in `docs/mining-sources.md`.
  Adding any other source is a package.
- Official sources (OpenCode, Claude, ...) can be built from `sources/` and bundled with releases without touching the
  core binary.
  None is bundled by this change.
  OpenCode's is built from `sources/opencode/` and is a candidate for bundling
  ([ADR-030](030-opencode-history-is-an-installed-webassembly-source.md)).
