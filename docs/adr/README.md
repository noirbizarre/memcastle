# Architecture Decisions

Records of the decisions that shape this project, and — more usefully — the reasons behind them.
An ADR is written when a choice is hard to reverse or likely to be re-proposed.

The point is not the decision; it is the alternatives that were rejected and why.
A record that only states the outcome saves nobody the argument.

A decision is changed by writing a new ADR that supersedes the old one, never by editing the old one.
The history is the value.

## Format

`NNN-kebab-case-title.md`, numbered in the order written, with the sections:

- **Status** — Proposed, Accepted, or Superseded by ADR-NNN
- **Context** — the forces in play, before any decision
- **Decision** — what was decided
- **Consequences** — what this costs, including what it makes harder

## Index

- [ADR-001](001-surrealkv-embedded-storage-engine.md) — SurrealKV as the only embedded storage engine in Phase 1
- [ADR-002](002-memory-mode-session-scoping.md) — memory mode is per-session/per-request, never daemon-global
- [ADR-003](003-checkpoint-as-a-durable-job.md) — checkpoint is a durable job; diary writes are a direct call
- [ADR-004](004-versioned-database-migrations.md) — versioned MemCastle data migrations,
  decoupled from the SurrealDB engine and storage backend
- [ADR-005](005-timestamp-representation.md) — timestamps are `datetime` when required,
  canonical RFC 3339 strings when optional
