# ADR-025: Deduplication is a conservative domain decision: exact copies are not stored twice, likely ones are linked

## Status

Accepted, builds on [ADR-004](004-versioned-database-migrations.md) (the backfill is a data migration),
[ADR-008](008-replay-safe-job-resume.md) (a replay must neither duplicate nor link a drawer to itself),
[ADR-021](021-richer-retrieval.md) (the lexical index that proposes candidates),
[ADR-023](023-unified-source-model-for-mining.md) (identity of mined content, which this refines)
and [ADR-024](024-entity-extraction-as-an-enrich-job.md) (the entity identity this replaces).
It amends ADR-008, ADR-023 and ADR-024 where they said duplicates and variants are not handled.

## Context

Nothing recognised the same memory twice.
An agent that wrote a fact again, a checkpoint retried by a different job, or a one-character typo in a repeat
all produced new, unrelated drawers, and the knowledge graph split `Ada`, `ada` and `ADA` into three entities with no
connection (issue #160).

Four forces shape the answer.

- **Canonical memory is verbatim.**
  A drawer's `content` is never replaced by a normalised or merged form, so everything computed to compare two drawers is
  derived and beside it.
- **Whether two memories are one is a judgement, not a constraint.**
  A unique index can refuse a write but cannot tell an exact copy from a typo, and would also refuse the copies that are
  legitimate: a superseded drawer, the same text in another room, two identical mined files.
- **A wrong merge is worse than a missed one.**
  A missed duplicate costs a second drawer.
  A wrong merge loses a distinct fact, or attaches one person's history to another.
- **Basic detection must not need a model.**
  Embeddings arrive after the write, from an optional provider, and an LLM is neither always available nor deterministic.

## Decision

Detection is deterministic and explainable, lives in `domain`, and records a verdict instead of acting on it.
The database only proposes candidates; MemCastle decides.

### Drawers

Within one room, among drawers that are currently valid, a new drawer is one of:

| Verdict | Rule | What happens |
|---|---|---|
| `exact` | The same bytes (content hash). | An unnamed write is **not stored**: the writer is handed the existing drawer. |
| `normalized` | The same words once case, punctuation and whitespace are ignored (equal fingerprint). | Stored, and linked. |
| `near` | Similarity at or above `[dedup] near_threshold` (default 0.9): a typo, a one-character edit. | Stored, and linked. |
| distinct | Anything lower. | Stored, nothing recorded. |

- **A link is a `similar_to` edge** from the newer drawer to the older, with the verdict, the similarity and the evidence
  (`same_hash`, `same_fingerprint`, the threshold in force).
  Both drawers stay.
  Deleting the edge undoes the call, and `GET /api/drawers/{id}/duplicates` shows it.
- **Similarity** is edit similarity over the normalised text (an adjacent swap is one edit) up to 2,000 characters, and
  the Jaccard index of character trigrams beyond, so the cost per write stays bounded.
  Texts under 12 normalised characters are only ever `exact` or `normalized`: one letter in a short phrase is a
  different word (`cat` / `car`), not a typo.
  Accents are not folded.
- **Candidates come from the store**: an indexed lookup on the content hash and the fingerprint, then a BM25 query on
  the text's own words, so a typo is found through the words it did not break.
  This needs no embedding.
  Semantic (embedding) similarity is deliberately not used at write time: the vector does not exist yet.
  Memories that are alike in meaning but not in words are found by search, not linked.
- **Scope.**
  Candidates are drawers in the same room, current only (a superseded drawer must not block saying something again).
  A diary entry is only compared with the same agent's entries, since identities never see each other's diary.
  A named write is always stored, because the name is its identity, and is linked to what it duplicates.
- **Mining skips nothing.**
  Two identical files are two records ([ADR-023](023-unified-source-model-for-mining.md)), so a mined chunk is stored as
  before and linked to what it resembles; the job's result counts them as `similar`.
- **A checkpoint item that is an exact copy** is not stored, still advances the job (the memory exists, the item is not
  lost) and is counted in the job result's `duplicates`.
- **Replays.**
  A drawer is never compared with itself, and links are idempotent,
  so a replayed job writes neither a second drawer nor a second edge.

### Entities

A name seen in a source resolves, in this order and only among entities of its kind (a vague `other` observation may
match any), to:

1. **the same name**, or
2. **the same key**: lowercase, letters and digits kept, `+` and `#` kept (`C++` is not `C`), dots and apostrophes
   dropped (`Node.js` is `nodejs`), any other run of characters one space — rule `normalized`; or
3. **a recorded alias** of an entity — rule `alias`; or
4. **exactly one entity one edit away** in a name of at least six characters — rule `typo`;
5. otherwise a **new entity**.
   If it resembles others (two edits, or one edit in a short name, with similarity of at least 0.75), or two entities are
   one edit away, a `possibly_same_as` edge records each resemblance and the name stays distinct.

- **The canonical name is never rewritten.**
  The spelling that was taken to be the entity is added to its `aliases`, and the `mentions` edge records the name as the
  drawer spelled it, the rule and the confidence (`observation`), beside the extraction provenance.
  Provenance and source-specific names therefore stay queryable after resolution.
- **Nothing merges two entities that already exist, and nothing deletes one.**
  Older duplicates stay as they are; new sightings converge on one of them deterministically.
- **A person settles an ambiguous name with `POST /api/entities/{id}/aliases`**, after which the spelling converges.
- `[dedup] entity_fuzzy = false` leaves rules 1 to 3 and turns off the typo rule and the review links.

### Where it lives

- `domain` holds the policy (`fingerprint`, `similarity`, `classify`, `entity_key`, `resolve`), pure and unit-tested.
- `store` finds candidates and records edges, and holds no matching rule.
- `dedup` orchestrates a drawer write; it names no mining source and reads no file, and `tests/source_isolation.rs` holds
  it to that, because the mining pipeline calls it.
- The extract job and the manual mention route call the entity resolver; extraction still only adds graph records.
- Schema additions are in `database/schema/palace.surql`: `drawer.fingerprint` with indexes on it and on `content_hash`,
  `entity.key`, `aliases` and `alias_keys`, and the `similar_to` and `possibly_same_as` relations.
  Data migration 3 (`since-0.2`, piece `dedup-keys`) fills the keys of existing records and merges nothing.

## Alternatives rejected

- **A unique constraint on the content hash.**
  It refuses exactly the copies that are legitimate, cannot see a typo, and makes the decision in the database.
- **Merge near-duplicates automatically.**
  A similarity score is not identity: "use port 8080" and "use port 8081" are one character apart and opposite facts.
  Linking keeps both and says why.
- **Decide with embeddings or an LLM.**
  Not deterministic, not always configured, and not available when the drawer is written.
  They may later add signals to the same edge; none is needed for basic detection.
- **A separate `Dedup` enrich job** (like extraction).
  It would let semantic similarity in, but an exact copy would already be stored and returned as a new drawer, and the
  acceptance case is that it is not.
- **Fold accents and strip all punctuation from entity keys.**
  `café` / `cafe` and `C++` / `C` are different things; a missed merge is cheaper than a wrong one.
- **Merge existing duplicate entities in the migration.**
  Rewriting every edge of every palace at once is irreversible and has no reviewer.

## Consequences

- An agent that states a memory again, in the same room, gets the drawer it already has.
  `created` is `false` over REST, a diary write returns the existing entry, and a checkpoint reports `duplicates`.
- A typo or a case variant is stored.
  Retrieval can therefore return both, and the `similar_to` edge is how a client, an audit or a later pass finds the pair.
- One extra indexed lookup and a bounded text comparison per drawer written, and one candidate query per entity named.
  The entity query scans entities of the kind by key length,
  which is fine for a personal palace and is the first thing to index if one grows very large.
- Two agents writing the same fact to one room share one drawer, and so do an agent and a person.
  Their provenance is that of whoever wrote it first; a diary is the exception.
- Casing and punctuation variants of an entity now converge, which changes the answer ADR-024 gave (`Ada` and `ada` were
  two entities).
  Entities already stored keep their ids.
- Mining keeps one record per document, so the same paragraph in ten files is ten drawers linked to each other.
- The entity limit is conservative on purpose: names under six characters never converge on a typo, and a name equally
  close to two entities converges on neither.
