# ADR-007: The memory-mode gate follows what an operation reads or writes, not its method name

## Status

Accepted

## Context

[ADR-002](002-memory-mode-session-scoping.md) decided that memory mode is per-session and enforced in one place,
`AppServices::require_read`/`require_write`.
It left open which operations count as memory operations.
The first answer was by name: `search`, `recall`, `wake_up`, `diary_read` and the checkpoint and diary writes were gated,
and everything else (job listing, job control, `mine`, `repair`, `audit`) was called "administrative".

That classification leaked in both directions:

- **A job record carries its whole input.**
  A checkpoint job's payload is the memory being written, so a `Disabled` session,
  which must behave as if MemCastle did not exist,
  could read palace content through `jobs list` and `jobs show`, and through the MCP job tool.
- **A `ReadOnly` session could mutate the palace through the back door.**
  `mine` files drawers and an applied `repair` deletes them, yet neither was gated,
  so a session forbidden to write could submit a job that did.

## Decision

An operation is gated by what it does to palace content, whatever it is called or which interface exposes it.

- **Reads** (rejected only in `Disabled`): `search`, `recall`, `wake_up`, `diary_read`, and job `list` and `show`,
  because a job record exposes its input.
- **Writes** (rejected in `ReadOnly` and `Disabled`): `checkpoint`, `emergency_checkpoint`, `diary_write`,
  `mine`, and an applied `repair` (`dry_run = false`).
- **Administrative, never gated:** `status` (counts and a version, no content), job control
  (`pause`, `resume`, `cancel`, `retry`, which need a job id that a session unable to list jobs never learns),
  `demo` (touches no palace content), `audit` and a dry-run `repair` (they only report).
- A rejection names the operation the caller actually made (`emergency_checkpoint`, not `checkpoint`).
- Read-only operations must not write:
  a `ReadOnly` read of a wing that does not exist returns an empty result and creates nothing.

A new operation is classified explicitly, in `app::AppServices` and in `domain::memory_mode`'s module documentation,
by asking two questions: does it return palace content, and can it change it?

## Alternatives rejected

- **Gate by method name or verb** (`list_*` and `get_*` are reads, `submit_*` is administrative).
  This is how the leak arose: `submit_mine` sounds like queueing, and `get_job` sounds like metadata.
- **Gate nothing that is "administrative" and document it.**
  It is true for `status` and job control, but not for anything whose result contains palace content
  or whose effect changes it.
- **Redact job records instead of gating them.**
  A job's input is arbitrary and nested; a redaction that misses a field is a leak, and one that hides too much
  makes the record useless to the session that submitted it.
- **Per-capability flags.**
  Already rejected in ADR-002: three modes cover every case seen,
  and a flag matrix would have to be kept in step with this list.

## Consequences

- `ReadOnly` keeps job reads (it can already read the same content through `search`), and loses `mine` and applied `repair`.
- `Disabled` cannot list, show or otherwise read jobs, which also stops it discovering job ids,
  but can still control a job it already holds an id for.
- Every interface inherits the classification, because it lives in `app`; MCP, REST and the CLI cannot disagree.
- Every new operation has to be classified, which is a review burden by design.
