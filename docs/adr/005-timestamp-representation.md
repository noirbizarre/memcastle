# ADR-005: Timestamps are `datetime` when required, canonical RFC 3339 strings when optional

## Status

Accepted

## Context

Timestamp columns were an accident of how each was first written, not a decision.
Required ones (`created_at`, `drawer.valid_from`, `relates_to.valid_from`) are SurrealDB `datetime`s.
Optional ones (`drawer.valid_to`, `relates_to.valid_to`, `job.started_at`, `completed_at`, `lease_expires_at`,
`migration_state.lock_expires_at`) are `option<string>`.

The split has a reason: with the 3.x driver a bound `NONE` cast to `<datetime>` is an error,
so a column that is sometimes absent cannot be written as `<datetime>$value` without a branch per call site.
A plain string binds `None` cleanly.

What was missing was a rule for those strings.
Writers used `DateTime::to_rfc3339`, whose fractional digits vary with the value and whose suffix is `+00:00`.
Comparing two such strings lexically is only correct while every writer emits the same form,
and nothing enforced that.
No query orders or ranges over them today (`valid_to` is only tested for truthiness, `lock_expires_at` is parsed in Rust),
so the risk was latent, but the first range query on `job.completed_at` would have been silently wrong.

## Decision

Keep the two column types and pin the string form:

- **Required timestamps stay `datetime`.** The database owns their format and ordering.
- **Optional timestamps stay `option<string>`, in one canonical form**:
  UTC, nine fractional digits, a `Z` suffix (`2026-09-29T14:22:47.123456789Z`).
  It is fixed-width with a fixed offset, so lexical order is chronological order, and nanoseconds are lossless.
- **One writer.** `store::stored` produces that form, and every timestamp write in `store` goes through it;
  the ad-hoc `.to_rfc3339()` calls are gone.
- **Reads are unchanged.** `chrono` parses any RFC 3339 form, so nothing on the read path depends on the canonical one.
- **Migration 2 (`canonical-timestamps`)** rewrites rows written by earlier versions into the canonical form once.
  It changes the spelling, never the instant, writes nothing to rows already canonical, and fails closed,
  naming the record, on a value that is not a timestamp.

## Alternatives rejected

- **Migrate the optional columns to `option<datetime>`.**
  This is the "obviously right" schema, but it needs a schema change, a data migration of five columns,
  and a workaround for binding `NONE` at every write site (or a `NULL`-tolerant cast).
  The cost is real and the benefit is a property the canonical string already gives:
  chronological order.
  Reconsider if a driver release makes `option<datetime>` binding painless,
  or if a query needs `datetime` functions on these columns.
- **Leave it as it was, with a comment.**
  The comment would be the only thing keeping the next writer honest, which is the failure this record exists to avoid.

## Consequences

- Lexical comparison of the optional timestamp columns is now valid, and a test pins the form.
- A new writer that bypasses `store::stored` reintroduces the problem; the store module's documentation says so,
  and review is the only guard.
  A `datetime` column would make it impossible, which is the argument for the rejected option.
- Migration 2 touches every job row with a timestamp on the first start after upgrading.
  It is a one-off proportional to the job count, and jobs are never deleted, so that count only grows.
- `migration_state.lock_expires_at` is written in the canonical form but not migrated: it is transient (a lease of minutes)
  and only ever parsed, never compared as a string.
