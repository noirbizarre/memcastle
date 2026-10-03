# ADR-021: Retrieval is SurrealDB-native and derived, with MemCastle owning only the contract and the policy

## Status

Accepted, builds on [ADR-004](004-versioned-database-migrations.md) (schema is SurrealKit's, never Rust DDL),
[ADR-005](005-timestamp-representation.md) (the validity columns this ADR finally queries),
[ADR-007](007-memory-mode-gate-follows-data-access.md) (reads and writes are gated by what they do)
and [ADR-008](008-replay-safe-job-resume.md) (the embedding sweep is replay-safe by construction)

## Context

Search was BM25 over drawer content and nothing else.
The `embedding`, `valid_from` and `valid_to` fields existed and were never read, and a `relates_to` graph existed with no
way from a drawer into it.
Issue #42 asks for semantic, hybrid, temporal and graph-aware retrieval without growing a second database:
no vector store, no graph store, no index file to fall out of sync, and no Rust re-implementation of what SurrealDB already
does.

The retrieval *contract* is MemCastle's: which scope applies, what "current" means, how legs combine, what a hit looks like.
The *execution* belongs to the database.

A model is not MemCastle's either.
No LLM or embedding model is linked in, and the daemon should not become a place where API keys live.

## Decision

- **Embeddings are derived data.**
  The drawer is canonical and an embedding only ever fills its `embedding` field,
  through one store method that touches no other column.
  A test pins that content, hash and timestamps are identical before and after.
- **One vector index, in the schema file.**
  `drawer_embedding_idx` is an HNSW index (cosine, `F32`) declared in `database/schema/palace.surql`,
  since Rust may not carry DDL.
  Its dimension is fixed at 768 and mirrored by `domain::EMBEDDING_DIMENSION`, which a test keeps equal to the schema.
  A model with a different native size must emit 768 (OpenAI-style `dimensions`), or a future ADR changes the index and
  re-embeds.
- **Every leg shares one scope, applied inside the database before ranking and before the limit.**
  Wing, room, tags (all of), source kind and temporal validity form one `WHERE` fragment used by lexical, vector,
  hybrid and graph queries, so the legs cannot drift apart.
  KNN is filtered during the index traversal; a post-filter would return fewer than `limit` in-scope neighbours,
  or none, when the globally nearest drawers are out of scope.
- **Fusion is SurrealDB's `search::rrf`.**
  Reciprocal rank fusion needs ranks, not scores, which is what makes a BM25 score and a cosine distance combinable.
  Rust does three small things: it embeds the query, breaks ties by drawer id so an ordering never depends on engine
  iteration order, and maps rows to the domain's `SearchHit`.
- **Ranking is a request option, spelled `ranking`.**
  `auto` (the default) is hybrid when the query can be embedded and lexical otherwise, so a palace without a provider behaves
  as before.
  A provider that fails degrades `auto` to lexical with a warning.
  `semantic` and `hybrid` are promises: without a vector they fail with `memcastle::search::semantic_unavailable`
  rather than silently answering lexically.
  The option is not called `mode` because `--mode` already means the memory mode.
- **Embeddings come from a provider behind one trait, or from the caller.**
  - `command` runs an operator-supplied program with JSON on stdin and stdout, so the program owns every credential and
    MemCastle holds none.
    The daemon's own `MEMCASTLE_*` variables are removed from its environment so the auth token cannot leak to a script.
  - `http` calls an OpenAI-compatible `/embeddings` endpoint (OpenAI, Ollama, llama.cpp, vLLM).
    A key is optional, held as `config::Secret`, and never logged or serialised.
  - A caller may send a vector with `PUT /api/drawers/{id}/embedding` or `query_embedding`, which needs no provider at all.
- **Embedding runs as a durable job.**
  `Embed` lists drawers with no embedding, embeds them in passes and stores the vectors.
  The database is the cursor, so a pause, a crash or a re-run loses nothing and re-embeds nothing.
  It is queued automatically after anything that writes drawers (coalesced to one waiting job) and once at startup,
  and `memcastle embed` queues it by hand.
  A provider failure fails the job, because silent holes would make semantic search quietly worse.
- **Temporal semantics are one rule.**
  A drawer or edge is valid at *t* when `valid_from <= t` and it has no `valid_to` or `valid_to > t`: the end is exclusive,
  so a record superseded at *t* and its replacement are never both valid and never both absent.
  `current` (the default) is *t* = now, `as_of` is any instant, and `include_historical` is no predicate at all.
  `valid_to` is compared as a canonical string, which ADR-005 made equal to comparing instants.
- **Drawers are superseded, never edited.**
  Superseding closes the old drawer and opens a replacement in one transaction.
  The old content, hash and id are untouched, so a point-in-time search still sees what was believed then.
  `(room, name)` is unique, so the superseded drawer gives up its name (it stays addressable by id) and the replacement
  takes it.
  Diary reads and wake-up highlights skip superseded drawers; management listings still show them.
- **The graph reaches drawers through `mentions`.**
  A `drawer -> mentions -> entity` edge, unique per pair, is derived data written by `POST /api/drawers/{id}/mentions`
  (and by #40's extractor once it exists).
  Expansion walks `seed -> mentions -> entity -> relates_to -> entity <- mentions <- drawer` inside SurrealDB,
  following only `relates_to` edges valid at the search's point in time.
  It appends related drawers after the direct hits, each with a `graph` signal and the entities that connect it.
  It never reorders or replaces a direct hit.
- **Search results are additive.**
  A hit is still the stored drawer verbatim plus `score`, now with optional `signals` and `via`.
  `score` is comparable only within one response, since its meaning follows the ranking.
  Search projections omit the embedding: a vector is hundreds of floats nobody asked for.

## Alternatives rejected

- **A separate vector database or a `usearch` index file.**
  A second source of truth that has to be kept in step with the drawers, and exactly what the single-store design avoids.
- **Embedding inside the daemon with a bundled model.**
  It would make the binary large and platform-specific, and pick one model for everyone.
- **Asking an agent CLI (OpenCode, Claude Code, Pi) to embed.**
  They are chat agents without an embedding endpoint, and calling them would invert the integration direction
  (invariant 8): integrations call MemCastle, never the reverse.
- **Reciprocal rank fusion in Rust.**
  It would re-implement what `search::rrf` does and need both candidate lists on the Rust side.
- **A weighted linear blend of BM25 and cosine.**
  The scores live on different scales and BM25 is not comparable across queries, so the weights would need tuning per palace.
- **Embedding at write time, inline.**
  It would make every write depend on a provider being up and fast.
  A job keeps writes independent and retries for free.
- **Rewriting a drawer in place to correct it.**
  It destroys history and breaks the rule that content is immutable.
- **Resolving query words to entities for graph expansion.**
  That is entity extraction, which is #40's work; expansion starts from the drawers the search already found.

## Consequences

- A palace needs no provider and behaves as before; configuring one turns on hybrid ranking by default.
- The vector dimension is part of the schema, so changing models across dimensions is a schema change plus a re-embed.
- Embedding granularity is one vector per drawer: mining stores a whole file as one drawer, and the provider sees its first
  8,000 characters.
  Chunking is a mining decision, outside this ADR.
- The HNSW index, supersession and `mentions` links survive a restart because they live in the same SurrealKV files as the
  drawers, which `tests/persistence.rs` proves across a real process boundary.
- `audit` reports drawers without an embedding, which is now a real backlog when a provider is configured, and still
  informational.
- BM25 scores are zero when every drawer matches the term (inverse document frequency vanishes), so a lexical result
  is only as ordered as the corpus is varied; fusion with the vector leg is what keeps such a query useful.
