# Architecture Decisions

Records of the decisions that shape this project, and — more usefully — the reasons behind them.
An ADR is written when a choice is hard to reverse or likely to be re-proposed.

The point is not the decision; it is the alternatives that were rejected and why.
A record that only states the outcome saves nobody the argument.

A decision is changed by writing a new ADR that supersedes the old one, never by editing the old one.
The one allowed edit is an *amendment*: a dated note at the end of a record, or a pointer in its Status,
for a change that leaves the decision standing (a mechanism that moved, or a list that grew).
The history is the value.

## Format

`NNN-kebab-case-title.md`, numbered in the order written, with the sections:

- **Status** — Proposed, Accepted, Superseded by ADR-NNN, or Accepted with a pointer to the ADR that amends it
- **Context** — the forces in play, before any decision
- **Decision** — what was decided
- **Alternatives rejected** — what else was on the table and why it lost; a record whose alternatives are
  argued in its Context, or that is about scope, may say so there or use **Non-goals** instead
- **Consequences** — what this costs, including what it makes harder

## Index

- [ADR-001](001-surrealkv-embedded-storage-engine.md) — SurrealKV as the only embedded storage engine in Phase 1
  (C-free claim narrowed by a dated note)
- [ADR-002](002-memory-mode-session-scoping.md) — memory mode is per-session/per-request, never daemon-global
  (gated operations amended by ADR-007)
- [ADR-003](003-checkpoint-as-a-durable-job.md) — checkpoint is a durable job; diary writes are a direct call
  (submission methods renamed by a dated note)
- [ADR-004](004-versioned-database-migrations.md) — versioned MemCastle data migrations,
  decoupled from the SurrealDB engine and storage backend
- [ADR-005](005-timestamp-representation.md) — timestamps are `datetime` when required,
  canonical RFC 3339 strings when optional
- [ADR-006](006-job-leases.md) — running jobs are held by a heartbeat lease,
  not by an assumption of one daemon
- [ADR-007](007-memory-mode-gate-follows-data-access.md) — the memory-mode gate follows what an operation
  reads or writes, not its method name (extended by ADR-018)
- [ADR-008](008-replay-safe-job-resume.md) — resuming a job is replay-safe (mining's identity amended by ADR-023)
- [ADR-009](009-shutdown-drains-jobs.md) — shutdown drains running jobs and hands them back to the queue
- [ADR-010](010-unix-xdg-paths.md) — configuration, data and state follow the Unix XDG layout on Linux and macOS
- [ADR-011](011-split-bind-address-and-port.md) — the listener's address and port are separate settings,
  bound before the daemon does anything else (non-loopback warning added by ADR-014,
  commands renamed by ADR-017)
- [ADR-012](012-status-reports-a-stopped-daemon-and-exits-by-state.md) — `status` answers for a stopped daemon too,
  and its exit code says which state it found (`--json` extended by ADR-015, form by stream amended by ADR-016)
- [ADR-013](013-release-packaging-and-asset-resolution.md) — releases are one binary plus an optional package layout,
  and assets resolve override, installed, embedded
  (`.deb` and `.rpm` packages added by a dated note)
- [ADR-014](014-optional-token-authentication.md) — authentication is an optional bearer token, checked at one layer,
  and never an MCP capability (commands renamed by ADR-017)
- [ADR-015](015-database-admin-endpoint.md) — the database admin endpoint is an opt-in listener inside the daemon,
  over its own database handle (`db serve` renamed `db start` by an amendment)
- [ADR-016](016-cli-presentation-follows-the-output-stream.md) — the CLI shows tables, colour and prompts to a terminal
  and plain data to a pipe, with no flag to choose
- [ADR-017](017-daemon-lifecycle-commands-live-under-daemon.md) — the daemon's background lifecycle lives under
  `memcastle daemon` (`start`, `stop`, `restart`), and `serve` stays the foreground server
- [ADR-018](018-palace-hierarchy-management.md) — wings, rooms and drawers are managed through REST and the CLI,
  with cascading transactional deletes, optional drawer names and no MCP tool (re-mining amended by ADR-023)
- [ADR-019](019-shared-integration-contract.md) — integrations share one documented contract and language-neutral
  fixtures, not a framework, and reach MemCastle only over MCP and HTTP
- [ADR-020](020-skills-are-versioned-with-the-repository.md) — agent skills are plain files versioned with the
  repository and installed by copying, and a test holds them to the tools, commands and routes they name
- [ADR-021](021-richer-retrieval.md) — retrieval is SurrealDB-native and derived: one HNSW index, one shared scope,
  `search::rrf` fusion, point-in-time validity, drawer supersession and graph expansion, with embeddings from a provider
  or the caller
- [ADR-022](022-integrations-are-bun-packages-tested-against-a-real-daemon.md) — integrations are self-contained bun
  packages, each with its own small client, tested against a real daemon in their own CI job
- [ADR-023](023-unified-source-model-for-mining.md) — mining goes through one source model in three stages (acquire,
  normalize, chunk and ingest), with idempotent ingestion and a cursor MemCastle keeps per source; re-mining no longer
  duplicates
- [ADR-024](024-entity-extraction-as-an-enrich-job.md) — entity extraction is an enrich job that only adds graph
  records, with provenance on every edge and a closed vocabulary for extracted facts, from a built-in heuristic or
  an external provider
- [ADR-025](025-memory-deduplication-and-entity-resolution.md) — deduplication is a conservative domain decision:
  an exact copy in a room is not stored twice, a typo or a case variant is stored and linked with its evidence, and
  entity spelling variants converge while ambiguous names stay distinct; nothing is merged and nothing needs a model
  (amends ADR-008, ADR-023 and ADR-024)
- [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) — mining sources are pluggable as
  WebAssembly components behind the same adapter contract, with explicit consented permissions, a computed lifecycle,
  a `source` command group for development and one conformance suite run against native and WebAssembly sources
- [ADR-027](027-cross-integration-tests-live-in-a-common-package.md) — tests that need two integrations live in a
  test-only `integrations/common` package that imports their sources, and production code stays per package
  (amends ADR-022)
- [ADR-028](028-pi-history-is-an-installed-webassembly-source.md) — Pi's conversation history is an installed WebAssembly
  source built from `sources/pi/`, and the core has no Pi-specific code; the native `pi-sessions` adapter is removed
  (supersedes part of ADR-023 and ADR-026)
- [ADR-029](029-project-local-configuration.md) — a project declares its wing and room in `.config/memcastle.toml`
  and `MEMCASTLE_WING` / `MEMCASTLE_ROOM`, read by each integration and passed as ordinary arguments; the daemon reads it
  only to choose a mined directory's wing, and a project file cannot pick a palace
- [ADR-030](030-opencode-history-is-an-installed-webassembly-source.md) — OpenCode's conversation history is an
  installed WebAssembly source built from `sources/opencode/` that acquires sessions by running the `opencode` command,
  a wider permission than a file grant, so no OpenCode or SQLite code enters the core
