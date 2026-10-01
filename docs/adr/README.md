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
- [ADR-002](002-memory-mode-session-scoping.md) — memory mode is per-session/per-request, never daemon-global
  (gated operations amended by ADR-007)
- [ADR-003](003-checkpoint-as-a-durable-job.md) — checkpoint is a durable job; diary writes are a direct call
- [ADR-004](004-versioned-database-migrations.md) — versioned MemCastle data migrations,
  decoupled from the SurrealDB engine and storage backend
- [ADR-005](005-timestamp-representation.md) — timestamps are `datetime` when required,
  canonical RFC 3339 strings when optional
- [ADR-006](006-job-leases.md) — running jobs are held by a heartbeat lease,
  not by an assumption of one daemon
- [ADR-007](007-memory-mode-gate-follows-data-access.md) — the memory-mode gate follows what an operation
  reads or writes, not its method name
- [ADR-008](008-replay-safe-job-resume.md) — resuming a job is replay-safe
- [ADR-009](009-shutdown-drains-jobs.md) — shutdown drains running jobs and hands them back to the queue
- [ADR-010](010-unix-xdg-paths.md) — configuration, data and state follow the Unix XDG layout on Linux and macOS
- [ADR-011](011-split-bind-address-and-port.md) — the listener's address and port are separate settings,
  bound before the daemon does anything else
- [ADR-012](012-status-reports-a-stopped-daemon-and-exits-by-state.md) — `status` answers for a stopped daemon too,
  and its exit code says which state it found
- [ADR-013](013-release-packaging-and-asset-resolution.md) — releases are one binary plus an optional package layout,
  and assets resolve override, installed, embedded
- [ADR-014](014-optional-token-authentication.md) — authentication is an optional bearer token, checked at one layer,
  and never an MCP capability
- [ADR-015](015-database-admin-endpoint.md) — the database admin endpoint is an opt-in listener inside the daemon,
  over its own database handle
