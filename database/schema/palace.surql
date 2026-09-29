-- The palace hierarchy, the search index, the knowledge graph, and the
-- durable job queue.
--
-- Applied via SurrealKit's `Sync` (see `store::mod`'s `surrealkit::embed_schema!()`
-- and `SurrealStore::sync_schema`), not a hand-rolled loop: SurrealKit tracks
-- this file's content hash in its own `__entity` metadata and only reapplies
-- it when the content actually changes — see docs/adr/004. `IF NOT EXISTS` on
-- every DEFINE is kept anyway as a harmless extra safety net.

DEFINE TABLE IF NOT EXISTS palace SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON TABLE palace TYPE string;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE palace TYPE datetime;

DEFINE TABLE IF NOT EXISTS wing SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS palace ON TABLE wing TYPE string;
DEFINE FIELD IF NOT EXISTS name ON TABLE wing TYPE string;
DEFINE FIELD IF NOT EXISTS description ON TABLE wing TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE wing TYPE datetime;
-- One wing name per palace: mining the same project twice must find the
-- existing wing rather than silently fork a duplicate.
DEFINE INDEX IF NOT EXISTS wing_palace_name ON TABLE wing FIELDS palace, name UNIQUE;

DEFINE TABLE IF NOT EXISTS room SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS wing ON TABLE room TYPE string;
DEFINE FIELD IF NOT EXISTS name ON TABLE room TYPE string;
DEFINE FIELD IF NOT EXISTS description ON TABLE room TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE room TYPE datetime;
DEFINE INDEX IF NOT EXISTS room_wing_name ON TABLE room FIELDS wing, name UNIQUE;

DEFINE TABLE IF NOT EXISTS drawer SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS room ON TABLE drawer TYPE string;
DEFINE FIELD IF NOT EXISTS content ON TABLE drawer TYPE string;
DEFINE FIELD IF NOT EXISTS content_hash ON TABLE drawer TYPE string;
-- `FLEXIBLE`: without it, a SCHEMAFULL table's `TYPE object` field silently
-- strips any nested key that isn't itself a separately-`DEFINE FIELD`'d
-- path — writing `{kind: ..., uri: ..., agent: ...}` would persist as `{}`.
-- `FLEXIBLE` opts a field's *contents* out of that enforcement while the
-- table around it stays SCHEMAFULL. Applies to every nested-object field
-- below (`provenance`, `kind`, `progress`, `properties`) for the same
-- reason.
DEFINE FIELD IF NOT EXISTS source ON TABLE drawer TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS tags ON TABLE drawer TYPE array<string>;
DEFINE FIELD IF NOT EXISTS tags.* ON TABLE drawer TYPE string;
DEFINE FIELD IF NOT EXISTS embedding ON TABLE drawer TYPE option<array<float>>;
DEFINE FIELD IF NOT EXISTS provenance ON TABLE drawer TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS valid_from ON TABLE drawer TYPE datetime;
-- `option<string>`, not `option<datetime>`: casting a bound `NONE` value to
-- `<datetime>` errors rather than passing through in SurrealDB 2.x, so an
-- *optional* datetime round-trips as a plain RFC3339 string instead (see
-- `store::mod`'s module doc on the cast convention this schema relies on).
DEFINE FIELD IF NOT EXISTS valid_to ON TABLE drawer TYPE option<string>;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE drawer TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON TABLE drawer TYPE datetime;
DEFINE INDEX IF NOT EXISTS drawer_room_idx ON TABLE drawer FIELDS room;

-- The "basic working search path": lexical/full-text search over drawer
-- content. Semantic/vector search is a later phase, added on top of this
-- same table (an `embedding`-based index alongside this one), not a
-- replacement for it.
DEFINE ANALYZER IF NOT EXISTS drawer_text TOKENIZERS blank, class FILTERS lowercase, snowball(english);
DEFINE INDEX IF NOT EXISTS drawer_content_search ON TABLE drawer
    FIELDS content FULLTEXT ANALYZER drawer_text BM25 HIGHLIGHTS;

-- Knowledge-graph tables, wired by `store::entities` (create/supersede/
-- invalidate/list relationships). Mining/MCP still don't populate them —
-- real extraction is #40's deliberate future work, not an oversight.
DEFINE TABLE IF NOT EXISTS entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON TABLE entity TYPE string;
DEFINE FIELD IF NOT EXISTS kind ON TABLE entity TYPE string;
DEFINE FIELD IF NOT EXISTS properties ON TABLE entity TYPE object FLEXIBLE;
-- One (name, kind) pair per entity: mirrors `wing_palace_name` above so
-- `create_entity`'s get-or-create is race-safe, not just "usually fine".
DEFINE INDEX IF NOT EXISTS entity_name_kind ON TABLE entity FIELDS name, kind UNIQUE;

DEFINE TABLE IF NOT EXISTS relates_to TYPE RELATION IN entity OUT entity SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS predicate ON TABLE relates_to TYPE string;
DEFINE FIELD IF NOT EXISTS confidence ON TABLE relates_to TYPE float;
DEFINE FIELD IF NOT EXISTS valid_from ON TABLE relates_to TYPE datetime;
-- `option<string>`: see the `drawer.valid_to` comment above.
DEFINE FIELD IF NOT EXISTS valid_to ON TABLE relates_to TYPE option<string>;

-- The durable job queue. SCHEMAFULL on everything the scheduler itself
-- reasons about (`status` in particular — an invalid status must never
-- silently persist); `kind`/`progress`/`checkpoint` stay loosely-typed
-- objects because their shape is handler-defined, not scheduler-defined.
DEFINE TABLE IF NOT EXISTS job SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS kind ON TABLE job TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS status ON TABLE job TYPE string
    ASSERT $value IN ["queued", "running", "paused", "completed", "failed", "cancelled"];
DEFINE FIELD IF NOT EXISTS priority ON TABLE job TYPE int;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE job TYPE datetime;
-- `option<string>`: see the `drawer.valid_to` comment above — casting a
-- bound `NONE` to `<datetime>` errors, so optional dates stay plain strings.
DEFINE FIELD IF NOT EXISTS started_at ON TABLE job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS completed_at ON TABLE job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS requested_by ON TABLE job TYPE string;
DEFINE FIELD IF NOT EXISTS progress ON TABLE job TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS attempt ON TABLE job TYPE int;
DEFINE FIELD IF NOT EXISTS max_attempts ON TABLE job TYPE int;
DEFINE FIELD IF NOT EXISTS checkpoint ON TABLE job TYPE object FLEXIBLE;
-- A report-producing job's final output (currently only `Audit`, see
-- `domain::job::Job::result`'s doc comment for why this is a separate field
-- from `checkpoint` rather than a reuse of it). `option<object>`, not
-- `object`: unlike `checkpoint` (always present, defaulted to `{}`), most
-- job kinds never set this at all.
DEFINE FIELD IF NOT EXISTS result ON TABLE job TYPE option<object> FLEXIBLE;
DEFINE FIELD IF NOT EXISTS error ON TABLE job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS lease_owner ON TABLE job TYPE option<string>;
DEFINE FIELD IF NOT EXISTS lease_expires_at ON TABLE job TYPE option<string>;
-- Backs `claim_next_job`'s "oldest, highest-priority queued job" scan.
DEFINE INDEX IF NOT EXISTS job_status_idx ON TABLE job FIELDS status, priority, created_at;
