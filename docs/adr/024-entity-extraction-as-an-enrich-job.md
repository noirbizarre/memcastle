# ADR-024: Entity extraction is an enrich job that only adds graph records, with provenance and a closed vocabulary

## Status

Accepted, builds on [ADR-007](007-memory-mode-gate-follows-data-access.md) (extraction is a write),
[ADR-008](008-replay-safe-job-resume.md) (ids derived from the evidence keep a replay from duplicating),
[ADR-014](014-optional-token-authentication.md) (a provider's key is a `Secret`, and a command never sees MemCastle's
own environment),
[ADR-021](021-richer-retrieval.md) (the graph that expansion walks, and the provider pattern copied from embeddings)
and [ADR-023](023-unified-source-model-for-mining.md) (the "enrich" stage it reserved, which reads what ingest filed).

## Context

The knowledge graph held only what a checkpoint's `fact` put there, and a `mentions` link a person made by hand.
Graph-aware search (ADR-021) therefore had nothing to walk for a palace built by mining, which is most palaces.
Issue #40 asks for entities and relationships to be extracted from mined content, without a heavyweight dependency in the
Rust core: an external call is acceptable, an embedded model runtime is not.

Three forces shape it.

- **Canonical memory is never silently replaced by derived data.**
  Entities, relationships and summaries are derived.
  Extraction may add graph records; it may not rewrite, supersede or delete the drawers it read.
- **Model output is unruly.**
  A model asked for a predicate says `works on`, `is working on`, `contributes to` and `hacks on`.
  If each became its own label the graph would fragment into synonyms,
  which is the lesson MemPalace's `kg_normalize` recorded and `domain::entity` flagged for this issue.
- **A fact has to be traceable and has to be able to stop being true.**
  "Ada works on MemCastle" was read from a particular chunk of a particular document at a particular revision,
  and when that chunk is replaced the fact has lost its evidence.

## Decision

- **Extraction is its own job, `Extract`, run after mining, not a step inside it.**
  It is the "enrich" stage of ADR-023: it reads what the pipeline filed.
  The pipeline, the chunker and the adapters are unchanged and know nothing of it, so invariant 9 still holds, and a slow
  or failing provider cannot hold up a mining cursor.
  A mining job that completes queues one sweep, coalesced like the embedding sweep, and so does startup;
  `memcastle extract` and `POST /api/jobs {"type":"extract"}` run one by hand.
- **The database is the cursor, through a marker beside the drawer.**
  A drawer is read when it is current, carries a mining origin (`source.origin`) and has no row in `drawer_extraction`.
  The marker is a side table rather than a field of the drawer so that reading a drawer never writes it.
  A drawer that names nothing is marked too, otherwise it would be sent to a model again on every sweep.
  Replaced drawers are not read: their replacement is.
- **Extraction only adds, and every write is idempotent.**
  It writes entities, `mentions` links, `relates_to` edges and then the marker, through the same store operations a
  checkpoint's `fact` uses (`get_or_create_entity`, `link_drawer_entity`, `create_relationship`), never a second graph.
  The marker comes last, and the edge id is derived from the drawer and the fact, so a crash anywhere replays the
  drawer harmlessly (ADR-008).
  A test compares every stored field of every drawer before and after, and `tests/source_isolation.rs` fails if
  `src/extract` can call a drawer writer or a source writer.
- **The vocabulary is closed for extracted facts.**
  Entity kinds are `person`, `organization`, `project`, `tool`, `place`, `concept` and `other`;
  predicates are `works_on`, `member_of`, `depends_on`, `uses`, `owns`, `part_of`, `located_in` and `related_to`.
  Whatever a provider says is read into them in one place, `Extraction::extract`, so a provider can stay dumb:
  an unknown kind becomes `other` and an unknown predicate the deliberately weak `related_to`,
  endpoints must be entities the same answer listed,
  self-loops, non-finite confidences and anything below `extraction.min_confidence` are dropped,
  and sizes are capped.
  Facts a person or agent asserts (a checkpoint's `fact`, `drawer mention`) keep free-form labels, normalised by trim and
  lower case as before: the closed set is a rule about what a machine may invent, not about what a person may say.
- **Provenance and validity are on the edge.**
  `mentions` and `relates_to` carry an optional `provenance` (`domain::FactProvenance`): the drawer, its mining origin
  (source, provider, document, chunk, revision), the extraction job and the extractor.
  A fact is valid from the document's `occurred_at` if the source says when it happened, otherwise from the drawer's
  `valid_from`, never later than now.
  When a drawer is superseded or retired, the next sweep closes the open facts that cite it, at the instant the drawer
  stopped being current, and extracts its replacement.
  History stays queryable with `include_expired`; a fact somebody asserted has no provenance and is never closed this way.
- **The extractor is pluggable, and off by default.**
  `[extraction] provider` is `none`, `heuristic`, `command` or `http`, shaped like `[embeddings]`.
  `heuristic` is a small deterministic extractor in the daemon: capitalised names, `@handles`, code spans, and a fixed
  table of phrases (`works on`, `depends on`, ...) between two names in one sentence.
  It is modest on purpose and needs no configuration.
  `command` runs the operator's program with JSON over stdin and stdout, so the program owns the model and its credentials.
  `http` calls an OpenAI-compatible `/chat/completions`, with the vocabulary in the prompt.
  Because `command` and `http` send mined text elsewhere, nothing is extracted until an operator chooses a provider.
- **The graph is readable over REST, read-only.**
  `GET /api/entities`, `/api/entities/{id}/relationships` and `/api/entities/{id}/mentions` return provenance and validity.
  There is no MCP tool and no write route: as with embedding, extraction is an operator concern, not something an agent
  turns on.

## Alternatives rejected

- **Extract inside the mining pipeline, per chunk.**
  Simpler to follow, but it ties model latency and failure to the cursor, makes the pipeline depend on a provider, and
  needs a second implementation for every other writer of drawers.
- **A heuristic only, or a provider only.**
  A heuristic alone caps quality at what a phrase table can do;
  a provider alone gives nothing out of the box and a test suite that needs a stub for everything.
  Having both costs one trait.
- **An LLM runtime in the daemon.**
  The issue rules it out, and a daemon that links a model is a daemon that has to ship and update one.
- **A free-form vocabulary, cleaned up later.**
  Synonym labels are written into edges and into history, where cleaning them means rewriting facts.
- **A flag on the drawer saying it was read.**
  It would write the canonical row on every sweep, and widen the type every writer and test constructs.
- **Deleting the facts of a replaced drawer.**
  A retracted fact is still something someone may ask about ("what did we believe in March?"), which is why relationships
  are closed and not removed.

## Consequences

- With `provider = "none"` nothing changes: no job, no cost, no network.
- A provider that fails fails the job, visibly, and the drawers it did not reach are read by the next sweep.
  The exception is an `http` model whose *reply* is not the JSON asked for: that is read as "nothing found" and logged,
  because failing would block every drawer behind it for ever.
- Changing provider does not re-read drawers already marked.
  Re-extraction with a new extractor is a deliberate non-goal here, and a later change can key the marker on the extractor.
- An entity is identified by `(name, kind)`, with the name case-sensitive.
  `Ada` and `ada` are two entities, and an extractor that cannot classify a name (`other`) joins an existing entity of that
  name rather than adding a second one.
  Aliasing and merging are not attempted.
- Several drawers stating the same fact give several edges, each with its own provenance, and none is
  deduplicated.
  Counting or collapsing them is a read-side concern.
- A drawer that is deleted outright (rather than superseded) leaves its `mentions` and `relates_to` records behind;
  expansion ignores them because it selects drawers, and `audit` does not report them yet.
- Only drawers that came through a mining source are read.
  Drawers written by a checkpoint, a diary entry or by hand carry no origin and are not extracted from.

## Amendment: entity resolution (2026-10-04)

[ADR-025](025-memory-deduplication-and-entity-resolution.md) replaces the entity identity above.
An entity is no longer only `(name, kind)` with a case-sensitive name: a name that differs from a known entity in case,
punctuation or spacing, is a recorded alias, or is a unique one-character typo in a name of six characters or more
converges on that entity, and the drawer's own spelling is kept on the `mentions` edge.
Ambiguous names stay distinct, linked by `possibly_same_as`.
Entities are still never merged or deleted, and extraction still only adds graph records.
Several drawers stating the same fact still give several edges.
