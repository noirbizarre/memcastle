# ADR-030: OpenCode history is an installed WebAssembly source that runs the `opencode` command

## Status

Accepted, builds on [ADR-023](023-unified-source-model-for-mining.md) (the source model),
[ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) (sources as WebAssembly components) and
[ADR-028](028-pi-history-is-an-installed-webassembly-source.md) (Pi's history as an installed source), issue #90.

## Context

Mining OpenCode's history must work on sessions that ended long ago, with no OpenCode running and no model called, and
must put no OpenCode or SQLite code in the core.
Pi was easy: its sessions are plain files, so a source with read-only access to a folder is enough (ADR-028).
OpenCode is not.
Since 1.2 it keeps every session in one SQLite database that it writes while it runs, in write-ahead-logging mode: the
committed pages of a live database are split between the database file and its log, so reading the file alone is unsafe,
and a source that linked a SQLite engine to read it would copy OpenCode's storage format into MemCastle's supply chain.
The JSON files OpenCode wrote before 1.2 are stale (nothing newer is ever written there) and cover only old history.

## Decision

- **OpenCode history is the `opencode` source, a WebAssembly component built from `sources/opencode/`.**
  It implements the same contract as every source and has no OpenCode-specific code in the core.
- **It acquires sessions by running the `opencode` command**, the program that owns the database:
  `opencode db` lists the sessions whose `time_updated` is past the cursor, and `opencode export <session>` returns one
  session as JSON.
  The manifest asks to run that one program, plus the variable `XDG_DATA_HOME` that OpenCode reads to find its data.
  It asks for no file access and no network, so the component never opens OpenCode's database or its credential files.
- **The discovery query is the one piece of OpenCode's internals the source depends on.**
  `opencode session list` has no way to ask only for what changed, and listing every session on every run would make
  incrementality a lie.
  The query reads two columns, is confined to the adapter,
  and fails with OpenCode's own message when OpenCode changes it.
  The cursor's values are checked before they are written into the query, since a query cannot bind parameters.
- **Identity, provenance and idempotency follow the rest of the model.**
  A session is identified by its OpenCode id, dated by when it was created, and given a revision that changes when its
  export does.
  A grown session files only its new tail: the header holds nothing that changes as a session goes on, notably not its
  title.
- **What is filed is what was said.**
  User and assistant text, tool calls as one-line markers and attached files by name.
  Reasoning, tool outputs, patches, snapshots, injected text and compaction summaries are left out, as in ADR-028.
- **A session whose export exceeds the host's output cap, or that is empty, or gone since discovery, is skipped, not an
  error.**
  One session must not fail every job at the same place and keep everything after it from being mined.
- **Tests do not need OpenCode.**
  The source ships a stand-in `opencode` command in `fixtures/bin/`, which the conformance cases and
  `tests/wasm_opencode.rs` put first on the `PATH`.
- **Bundling is a candidate, not a change.**
  The source is maintained under `sources/opencode/` so that it can ship alongside releases
  without being linked into the binary, but no bundling mechanism existed yet
  ([#159](https://github.com/noirbizarre/memcastle/issues/159)).
  [ADR-033](033-source-distribution.md) added it: releases now bundle this source, and it installs by name.

## Consequences

- **Installing grants more than a file read.**
  A program runs with the daemon's own authority; the host decides which program and with what environment, not what it
  does.
  That is why the permission is one named program with no shell, the child's environment is cleared of everything the
  manifest did not list, and installing needs the user's consent to exactly that
  (invariant 10).
  It is a wider grant than Pi's, and the page says so.
- **OpenCode must be installed, and on the daemon's `PATH`.**
  The source says so when it is not, at `identify`, before any job does real work.
  A daemon started by a service manager may not have the `PATH` of the user's shell.
- **Mining reads at the speed of OpenCode's command line**, a few seconds per call on a large database.
  The 60-second call limit applies to each call, not to a job.
- **A new OpenCode could break discovery**, and the failure is loud and quotes OpenCode.
  The legacy JSON history written before 1.2 is not mined.
- **Pi history needs an install step, and so does this**, though a release bundles both
  ([ADR-033](033-source-distribution.md)), so the step is `memcastle source install opencode`.
