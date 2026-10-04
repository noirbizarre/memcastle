-- Entity extraction bookkeeping (docs/adr/024): which drawers the extraction job has already read.
-- Applied through SurrealKit's `Sync` like every other file here (docs/adr/004); purely additive, so no data migration.
--
-- Drawers are canonical memory and are never written by extraction: this marker lives beside them, not on them, so
-- "has this drawer been read" costs no change to the drawer's own row. A drawer with no entities is still marked,
-- otherwise it would be sent to the extractor again on every sweep.

-- The record id is the drawer's id, so marking twice finds the same row.
DEFINE TABLE IF NOT EXISTS drawer_extraction SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS drawer ON TABLE drawer_extraction TYPE string;
-- The extractor that read it (`heuristic`, `command`, `http`).
DEFINE FIELD IF NOT EXISTS extractor ON TABLE drawer_extraction TYPE string;
DEFINE FIELD IF NOT EXISTS entities ON TABLE drawer_extraction TYPE int;
DEFINE FIELD IF NOT EXISTS relations ON TABLE drawer_extraction TYPE int;
DEFINE FIELD IF NOT EXISTS job ON TABLE drawer_extraction TYPE option<string>;
DEFINE FIELD IF NOT EXISTS extracted_at ON TABLE drawer_extraction TYPE datetime;
