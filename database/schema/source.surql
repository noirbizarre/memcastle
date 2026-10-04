-- The unified mining source model (docs/adr/023): what MemCastle remembers about
-- the places it mines between jobs. Applied through SurrealKit's `Sync`, like
-- every other file here (docs/adr/004); purely additive, so no data migration.
--
-- Drawers are still the canonical memory. These two tables are bookkeeping that
-- makes mining incremental and idempotent: where the last run stopped, and which
-- revision of each document is already filed as which drawers.

-- One row per place that is mined. The record id is derived from
-- (provider, account, locator), so two jobs mining the same place agree on it
-- and no unique index over the three fields is needed.
DEFINE TABLE IF NOT EXISTS source SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS provider ON TABLE source TYPE string;
-- `option<string>`: local sources have no account.
DEFINE FIELD IF NOT EXISTS account ON TABLE source TYPE option<string>;
DEFINE FIELD IF NOT EXISTS locator ON TABLE source TYPE string;
-- A *reference* to a credential (`{type: "env", name: ...}`), never the secret:
-- nothing in this table may hold one (invariant 6). `{}` means none, because an
-- absent optional object would otherwise need NONE-vs-NULL care on every write.
DEFINE FIELD IF NOT EXISTS credential ON TABLE source TYPE object FLEXIBLE;
-- The adapter's own cursor, opaque to MemCastle. `{}` means "from the beginning".
-- `FLEXIBLE`: see the comment on `drawer.source` in palace.surql.
DEFINE FIELD IF NOT EXISTS cursor ON TABLE source TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS last_job ON TABLE source TYPE option<string>;
-- `option<string>`: see the `drawer.valid_to` comment in palace.surql.
DEFINE FIELD IF NOT EXISTS last_run_at ON TABLE source TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE source TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON TABLE source TYPE datetime;

-- One row per document of a source: the revision last ingested and the drawers
-- that came of it. The record id is derived from (source, external id), so
-- ingesting the same document twice finds the same row.
DEFINE TABLE IF NOT EXISTS source_document SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS source ON TABLE source_document TYPE string;
DEFINE FIELD IF NOT EXISTS external_id ON TABLE source_document TYPE string;
DEFINE FIELD IF NOT EXISTS revision ON TABLE source_document TYPE string;
-- The raw document, kept only for adapters that declare `retains_raw`.
DEFINE FIELD IF NOT EXISTS raw ON TABLE source_document TYPE option<string>;
DEFINE FIELD IF NOT EXISTS raw_hash ON TABLE source_document TYPE string;
DEFINE FIELD IF NOT EXISTS title ON TABLE source_document TYPE option<string>;
DEFINE FIELD IF NOT EXISTS metadata ON TABLE source_document TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS occurred_at ON TABLE source_document TYPE option<string>;
-- `[{index, hash, drawer}]`, in chunk order.
DEFINE FIELD IF NOT EXISTS chunks ON TABLE source_document TYPE array;
DEFINE FIELD IF NOT EXISTS chunks.* ON TABLE source_document TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS acquired_at ON TABLE source_document TYPE datetime;
DEFINE FIELD IF NOT EXISTS job ON TABLE source_document TYPE option<string>;
-- Backs "how many documents does this source have".
DEFINE INDEX IF NOT EXISTS source_document_source_idx ON TABLE source_document FIELDS source;
