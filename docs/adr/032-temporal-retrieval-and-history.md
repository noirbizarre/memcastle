# ADR-032: Temporal retrieval is one overlap rule over validity time, and history walks explicit supersession links

## Status

Accepted.
Amends [ADR-021](021-richer-retrieval.md): `current` and `as_of` keep their meaning and become two cases of one rule,
which also gives an interval, and a superseded drawer now records what replaced it.
Builds on [ADR-005](005-timestamp-representation.md) (the string form `valid_to` is compared in)
and [ADR-007](007-memory-mode-gate-follows-data-access.md) (history is a read).

## Context

ADR-021 made point-in-time search work: `current`, `as_of` and `include_historical`.
Issue #162 asks for the rest of what a memory that changes over time needs.
An agent should be able to ask what was true during a period, and how a piece of knowledge evolved,
without rebuilding the answer from a pile of search hits.

Two gaps stood in the way.

- **There was no interval.**
  "What did we believe during January?" has no spelling as a list of points,
  and a client that sampled points would miss a fact that began and ended between two of them.
- **Nothing recorded which drawer replaced which.**
  `supersede_drawer` set the old drawer's `valid_to`, handed its name to the replacement and stopped.
  A history could only be guessed from a room, a name that the old drawer no longer has, a mining origin,
  or the old `valid_to` equalling the new `valid_from`, and the last was not even true of mined drawers:
  the pipeline built the replacement, then read the clock again to close the old one, leaving a few milliseconds in which
  both were valid.

The same issue forbids a temporal index or datastore, and requires lexical, semantic, hybrid and graph retrieval to agree.

## Decision

- **Time means validity time.**
  `valid_from` and `valid_to` say when the knowledge was true.
  `created_at` and `updated_at` say when MemCastle recorded it, and are never searched.
  A memory discovered long after the period it describes is found by asking about that period.
- **One rule, an overlap.**
  Validity is the half-open range `[valid_from, valid_to)`, and no `valid_to` means open-ended.
  A record overlaps the window `[from, until)` when `valid_from < until` and it has no `valid_to` or `valid_to > from`.
  Touching is not overlapping: a record closed exactly at `from`, or opened exactly at `until`, is outside.
  A point *t* is the window `[t, t + 1ns)`, so `current` (*t* = now) and `as_of` run the very same clause as an interval,
  and a supersession at *t* leaves exactly one of the two records valid at *t*.
  `include_historical` is no clause at all.
- **The clause lives in one place.**
  `store::retrieval::VALIDITY_WHERE` is spliced into the shared scope every leg applies, so lexical, semantic and hybrid
  search cannot disagree, and into graph expansion's `relates_to` hops, so an edge is judged exactly as a drawer is.
  It is a predicate beside the full-text and vector lookups: no index, no second structure.
- **`Temporal` gains `Between { from, until }`.**
  The wire form is `{"between": {"from": ..., "until": ...}}` on `POST /api/search`.
  An empty or reversed window (`from >= until`) is refused where it enters, `memcastle::input::invalid`, because it contains
  no instant and would otherwise silently match nothing.
- **The options are `from` and `until`, and the instants may be dates.**
  `as_of`, `from` with `until`, and `include_historical` are mutually exclusive, and `from` without `until` is an error
  naming the missing end.
  Both ends are required: an open end is a different question (a lower bound alone is "since", an upper bound alone is
  "ever before"), better added deliberately than inferred.
  An instant is RFC 3339 or a `YYYY-MM-DD` date, read as midnight UTC at the start of that day,
  so `--as-of 2026-01-01` works as written and `--from 2026-01-01 --until 2026-02-01` is exactly January.
  One conversion, `SearchOptions::into_query`, serves REST, MCP and the CLI.
- **Supersession is recorded.**
  `drawer` gains `supersedes` and `superseded_by`, plain drawer ids like every foreign key,
  and `supersede_drawer` sets both inside the transaction that closes the old drawer and opens the new one.
  The link is written by the store, not by a caller, so a pair cannot be half-linked and no writer can forget it.
  A drawer closed without a replacement has no `superseded_by`: that is an invalidation, not a missing link.
- **A migration pairs history that already exists.**
  Data migration 3 (`since-0.2`, piece `supersession-lineage`) links a closed drawer to a successor in the same room that opened at the
  instant it closed, or that came from the same mined document chunk and opened within ten seconds before.
  A link is written only when exactly one drawer qualifies, and an ambiguous or unmatched close is left alone.
- **Mining opens the replacement at the instant it closes the old drawer.**
  The pipeline sets the replacement's `valid_from` to the instant it passes to `supersede_drawer`, as the manual path already
  did, so a point-in-time search lands on one version and never two.
- **History is a read of the chain, by drawer id.**
  `drawer_history` walks the links both ways from any member and returns every version oldest first, each verbatim:
  identity, validity period, provenance and content, without embeddings.
  The walk is bounded and remembers where it has been, so a corrupt link ends it rather than looping, and a link to a deleted
  drawer ends it too.
  It is exposed as `GET /api/drawers/{id}/history`, `memcastle drawer history`, and `memcastle_history`.
  It is a read like search (ADR-007), so a read-only session may use it and a disabled one may not.
  The MCP tool is read-only and takes an id, which every search hit carries, so an agent searches (with `include_historical`
  or `as_of`) and then follows a hit, with nothing to reconstruct.

## Alternatives rejected

- **A temporal index or a table of versions.**
  The issue forbids a second structure, and validity is already on the drawer.
  A predicate beside the existing lookups costs a comparison per candidate,
  which the prefilter already pays for the rest of the scope.
- **Inferring history at read time.**
  The old drawer gives up its name on supersession and many drawers have none, a mining origin only identifies mined chunks,
  and `old.valid_to == new.valid_from` was false for mined drawers until now.
  A chain that is right for some drawers is worse than one that is explicit for all of them.
- **A `supersedes` edge table instead of two fields.**
  It would be a third shape for a one-to-one relation that a drawer can carry in two columns and that history reads by id.
  It would also make the walk a graph query for no gain.
- **Separate point and interval constructs in the query.**
  Two predicates that are meant to agree would eventually differ at a boundary.
  Treating a point as a one-nanosecond window makes that impossible by construction.
- **A single `--between A B` flag.**
  It is awkward to spell as a query-string parameter and as an MCP argument, which would then not match the CLI.
- **`--valid-from` and `--valid-to` as the names.**
  They echo the drawer's fields, but a drawer's `valid_to` is an exclusive end that can be open,
  and a search window is neither.
  They would also read as record fields in the output.
- **Accepting a half-open interval (`--from` alone).**
  See above: it is a different question, and the answer can still be had with a far bound until a use case argues for it.
- **A history addressed by search text.**
  It would put ranking decisions (which hits, how many chains) into an operation whose point is to be exact.
  Search finds the drawer, history explains it.
- **Letting an MCP caller write a back-dated drawer to test the model.**
  Writing validity time is a separate decision with its own consequences for provenance; nothing here needs it.

## Consequences

- An agent can ask what is true now, what was true on a date, what was true across a period, and how a decision evolved,
  with the same options on REST, MCP and the CLI and the same answer from every ranking.
- `current` and `as_of` results are unchanged, except that a mined drawer's predecessor no longer overlaps it by milliseconds.
- Every drawer row gains two optional columns, carried in each projection, and `Drawer` gains two optional fields that are
  absent from the JSON when unset, so existing clients and the fixtures are unaffected.
- A palace written before this release has its mined and manual supersessions linked on the next start,
  and any it could not pair unambiguously read as one-version histories.
- The window's end is exclusive on both sides,
  which is the convention that makes `--from 2026-01-01 --until 2026-02-01` mean January,
  and the one thing a user used to inclusive ranges has to learn.
- There is still no way to write a drawer with a back-dated `valid_from`,
  so validity and record time differ today only for mined content and for corrections,
  which is where the distinction matters.
