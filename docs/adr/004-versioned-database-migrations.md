# ADR-004: Versioned MemCastle data migrations, decoupled from the SurrealDB engine and storage backend

## Status

Accepted (design). Implementation is tracked as issue #44 and has not landed
yet — this ADR records the settled decision the epic asks to document ahead
of that work, per issue #20's "or at least have settled designs" allowance.

## Context

MemCastle owns long-lived memory data (drawers, entities, relationships,
jobs) in SurrealDB. Once deployed, the schema and stored records must be
evolvable without forcing a user to delete/recreate their palace.

Today (Phase 1, as shipped): there is exactly one migration file,
`store/migrations/0001_init.surql`, applied unconditionally on every
`SurrealStore::connect` (see `store::MIGRATIONS`/`Store::migrate`). Every
`DEFINE` in it is `IF NOT EXISTS`, so re-running it after the first connect
is a no-op — there is no version-tracking table, no ordered set of numbered
steps beyond that one file, and no `memcastle migrate` CLI command. This is
sufficient for Phase 1 because nothing shipped yet has needed a destructive
or reshaping change to an already-released schema.

Three version concepts must not be conflated: (1) the SurrealDB *engine*
version (3.x, upgraded independently via the `surrealdb` crate dependency),
(2) the embedded *storage backend* choice (SurrealKV today — ADR-001; a
future server-side backend decision is explicitly deferred), and (3)
MemCastle's own *application data version* — the schema/data shape
MemCastle code expects, which changes on MemCastle's own release cadence and
must be tracked and migrated independently of the other two.

## Decision

- One `MigrationRunner`/`DatabaseMigrator` application service, used
  identically by two entry points: normal daemon startup, and an explicit
  `memcastle migrate` (`--check`, `--status`) CLI command. The CLI command
  is a thin wrapper, never a second migration system.
- A persisted MemCastle data-version record, separate from anything
  SurrealDB itself exposes. Migrations are ordered, immutable once
  released, and forward-only.
- Startup sequence: open the datastore → acquire an exclusive migration
  lock → read the current data version → run all pending migrations in
  order → synchronize the declarative schema → record the new version →
  only then accept client connections. A failed migration fails the daemon
  closed — it must never serve a partially migrated database.
- Two migration shapes: schema-only changes (additive
  `DEFINE ... IF NOT EXISTS`, synchronized declaratively) and data
  migrations (explicit, versioned Rust/SurrealQL steps for
  renames/reshapes/backfills), which must be deterministic and
  resumable/idempotent where practical, and must never silently destroy
  canonical memory.
- The migration model works unmodified against the embedded SurrealKV
  backend today and against a future remote SurrealDB backend later — the
  runner does not get to know or care which one is active, the same way
  `store::Backend` already hides that choice from the rest of the codebase
  (ADR-001).

## Non-goals

- Does not re-open the embedded-storage-engine choice (ADR-001) — SurrealKV
  remains the only embedded backend Phase 1 compiles.
- Does not decide the server-side storage backend — deferred until a
  server deployment model exists (reaffirming ADR-001's own deferral).
- No separate database-management binary, no dependency on a separately
  installed SurrealDB/SurrealKit CLI — one runner, reached through the one
  `memcastle` binary.

## Consequences

- Until issue #44 lands, MemCastle's actual behavior remains the Phase 1
  stand-in described in Context: a single idempotent schema file with no
  recorded version. Adequate for additive, `IF NOT EXISTS`-shaped schema
  growth; not capable of expressing a data-reshaping or backfilling change.
  Any such change must wait for this runner to exist, or risks being
  applied inconsistently across palaces that first connected at different
  MemCastle versions.
- Every future migration becomes an immutable, append-only artifact once
  released — fixing a mistake in a shipped migration means writing a new
  migration that corrects it, never editing the old one.
- A daemon that fails to migrate must fail to start: a MemCastle instance
  that cannot certify its own schema state must not serve agents against a
  database it cannot vouch for.
