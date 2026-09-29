-- MemCastle's own data-migration bookkeeping table — NOT SurrealKit's.
-- SurrealKit tracks its own schema-sync/rollout state in its own `__entity`/
-- `__rollout` tables (see docs/adr/004: MemCastle must never duplicate
-- that). This table is the one piece of migration state MemCastle itself
-- is allowed to own: its application data-migration version watermark, and
-- the exclusive lock `crate::migrate::run` holds while applying pending
-- steps. SurrealKit only applies *this file's schema* (the DEFINE
-- statements below, same as any other schema file); every row in the table
-- is read/written exclusively by `store::migration_state`.
--
-- A single fixed row (`migration_state:state`) rather than one row per
-- applied migration: `version` is a watermark, not a ledger. Resumability
-- comes from `migrate::run_with` comparing each step's version against
-- this watermark, not from replaying an applied-migrations history.
--
-- Self-contained on purpose (no reference to any other table): this file's
-- application order relative to `palace.surql` never matters.

DEFINE TABLE IF NOT EXISTS migration_state SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS version ON TABLE migration_state TYPE int;
-- option<string>, not option<datetime>: casting a bound NONE to <datetime>
-- errors rather than passing through — see `store::mod`'s module doc on
-- the cast convention this schema relies on.
DEFINE FIELD IF NOT EXISTS lock_owner ON TABLE migration_state TYPE option<string>;
DEFINE FIELD IF NOT EXISTS lock_expires_at ON TABLE migration_state TYPE option<string>;
DEFINE FIELD IF NOT EXISTS updated_at ON TABLE migration_state TYPE datetime;
