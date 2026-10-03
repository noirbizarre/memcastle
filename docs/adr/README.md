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
- [ADR-008](008-replay-safe-job-resume.md) — resuming a job is replay-safe
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
  with cascading transactional deletes, optional drawer names and no MCP tool
- [ADR-019](019-shared-integration-contract.md) — integrations share one documented contract and language-neutral
  fixtures, not a framework, and reach MemCastle only over MCP and HTTP
- [ADR-020](020-skills-are-versioned-with-the-repository.md) — agent skills are plain files versioned with the
  repository and installed by copying, and a test holds them to the tools, commands and routes they name
