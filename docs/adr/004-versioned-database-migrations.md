# ADR-004: Versioned MemCastle data migrations, decoupled from the SurrealDB engine and storage backend

## Status

Accepted; implemented in #44.

## Context

MemCastle owns long-lived memory data (drawers, entities, relationships, jobs) in SurrealDB.
Once deployed, the schema and stored records must be evolvable without forcing a user to delete/recreate their palace.

Before #44 (Phase 1, as originally shipped): there was exactly one migration file, `store/migrations/0001_init.surql`,
applied unconditionally on every `SurrealStore::connect`. Every `DEFINE` in it was `IF NOT EXISTS`, so re-running it
after the first connect was a no-op — there was no version-tracking table, no ordered set of numbered steps beyond
that one file, and no `memcastle migrate` CLI command. This was sufficient only because nothing shipped yet had
needed a destructive or reshaping change to an already-released schema.

Three version concepts must not be conflated:
(1) the SurrealDB *engine* version (3.x, upgraded independently via the `surrealdb` crate dependency),
(2) the embedded *storage backend* choice
(SurrealKV today — ADR-001; a future server-side backend decision is explicitly deferred),
and (3) MemCastle's own *application data version* — the schema/data shape MemCastle code expects,
which changes on MemCastle's own release cadence and must be tracked and migrated independently of the other two.

**Schema management is not MemCastle's to reimplement.** [SurrealKit](https://github.com/surrealdb/surrealkit)
(`surrealkit` crate, used as a Rust library, not its CLI) already provides declarative schema sync and staged
rollouts for SurrealDB, including a compile-time `embed_schema!()` macro for embedded backends. Building a second,
parallel schema-diff/versioning/tracking engine inside MemCastle when one already exists and is designed for exactly
this embedding scenario would be needless duplication and a maintenance burden with no offsetting benefit.

## Decision

- Two migration shapes, kept deliberately separate:
  - **Schema.** Declarative `.surql` files under `database/schema/`, embedded into the binary via SurrealKit's
    `embed_schema!()` macro and applied through its `Sync` builder. SurrealKit — not MemCastle — owns diffing,
    content-hash tracking (its own `__entity`/`__rollout` metadata tables), and pruning. MemCastle does not
    implement a parallel schema-diff/versioning/tracking system. SurrealKit's `Rollout` API is reused, not
    reimplemented, for a future schema change that genuinely needs a staged expand/contract with a rollback path.
  - **Data.** An ordered, immutable list of versioned Rust steps (`crate::migrate::DataMigration`) for changes
    that can't be expressed as additive schema sync — a rename, reshape, split/merge, or backfill. These are
    MemCastle's own, deterministic, and resumable/idempotent where practical, and must never silently destroy
    canonical memory.
- One runner (`crate::migrate::run`/`status`), used identically by two entry points: normal daemon
  startup (`server::run`), and an explicit `memcastle migrate` (`--check`, `--status`) CLI command. The CLI command
  connects to storage directly, the same narrow exception `serve` already is to "the CLI only calls
  `client::DaemonClient`" — migration must work without, and before, a daemon exists. Both entry points call the
  exact same function; the CLI is a thin wrapper, never a second migration system.
- A persisted MemCastle data-version watermark (`migration_state` table — itself one of the SurrealKit-managed
  schema files, but whose *rows* are read/written exclusively by `store::migration_state`, never SurrealKit's own
  bookkeeping), separate from anything SurrealDB or SurrealKit itself tracks. Migrations are ordered, immutable
  once released, and forward-only.
- Runner sequence: open the datastore (`SurrealStore::connect`, which does *not* sync schema or migrate itself)
  → sync schema for real once up front (so the watermark/lock table exists at all — see Consequences)
  → acquire an exclusive migration lock (a lease-based compare-and-swap over one row, tolerant of a crashed holder)
  → read the current data version → run all pending data migrations in order, recording the watermark after each
  success → re-synchronize the declarative schema (picks up anything the release also shipped) → release the lock.
  A failed migration fails the daemon closed — it must never serve a partially migrated database — and the
  watermark stays at the last step that succeeded, so a later, corrected run resumes rather than replays.
- `--status`/`--check` report MemCastle's own watermark/pending-migrations list only — a plain read, no lock, no
  mutation. They deliberately do **not** also invoke a SurrealKit dry-run: empirically (`surrealkit` 1.0.0-beta.2),
  a dry-run diff against a `SCHEMAFULL` table that has never been synced for real even once errors instead of
  reporting "would create" — and a status command must never fail on exactly the palace state it exists to
  describe. Full schema-side status is SurrealKit's own tooling to report, not MemCastle's to re-derive.
- The migration model works unmodified against the embedded SurrealKV backend today
  and against a future remote SurrealDB backend later — the runner does not get to know or care which one is active,
  the same way `store::Backend` already hides that choice from the rest of the codebase (ADR-001).

## Non-goals

- Does not re-open the embedded-storage-engine choice (ADR-001) —
  SurrealKV remains the only embedded backend Phase 1 compiles.
- Does not decide the server-side storage backend —
  deferred until a server deployment model exists (reaffirming ADR-001's own deferral).
- No separate database-management binary, no dependency on a separately installed SurrealKit CLI —
  the `surrealkit` crate is used purely as a library (`default-features = false`, no `cli` feature), reached
  through the one `memcastle` binary.
- Does not reimplement anything SurrealKit already does: no parallel schema hash table, no schema diff engine, no
  schema-definition migration framework. If a future change needs a staged/expand-contract schema transition,
  reach for SurrealKit's `Rollout` API rather than inventing a MemCastle-specific equivalent.

## Consequences

- Every future data migration becomes an immutable, append-only artifact once released —
  fixing a mistake in a shipped migration means writing a new migration that corrects it, never editing the old one.
- A daemon that fails to migrate must fail to start: a MemCastle instance that cannot certify its own data version
  must not serve agents against a database it cannot vouch for.
- The runner's own bookkeeping table (`migration_state`) has to exist before the runner can even read "what version
  is this palace at" — so unlike the abstract "read version, then sync schema" ordering one might expect, the very
  first thing `crate::migrate::run` does is a real (non-dry-run) schema sync, unconditionally. This is idempotent
  and cheap when already applied, and is a one-time bootstrap concern distinct from the schema re-sync that happens
  again after data migrations run.
- `SurrealStore::connect` no longer syncs schema or migrates as a side effect of connecting (unlike the original
  Phase 1 stand-in) — every caller that needs a fully migrated, ready-to-use store must go through
  `crate::migrate::run` explicitly (as `server::run` and `memcastle migrate` both do), or, for test code that
  doesn't care about migration orchestration, `SurrealStore::connect_memory_for_tests`.
