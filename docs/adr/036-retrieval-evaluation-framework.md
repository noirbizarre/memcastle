# ADR-036: Retrieval is evaluated by an HTTP-only harness in the test tree, against a committed quality baseline

## Status

Accepted.
Relates to [ADR-021](021-richer-retrieval.md) and [ADR-032](032-temporal-retrieval-and-history.md), whose retrieval it measures,
and to [ADR-013](013-release-packaging-and-asset-resolution.md), whose release allowlist keeps it out of every artifact.

## Context

Issue #120 asks for a reproducible way to tell whether retrieval got better or worse, as lexical, semantic, hybrid,
temporal and graph-aware retrieval land: recall and precision style metrics, latency, documented datasets and commands,
a documented baseline for the lexical retrieval, results that can be compared across implementations,
and an explicit path for LongMemEval where its licence and availability allow.
It also asks that the fixtures be small and deterministic enough for CI, that larger runs be explicit,
that quality be kept apart from embedding-model quality, and that no benchmark number become a release gate.

Four decisions follow, and each has an alternative that is easier for one force and worse for the others.

- Where the harness lives, and what it may touch.
- Where vectors come from, so that engine quality is not confounded with model quality.
- How time is represented when no write route accepts a timestamp.
- What is committed, and what is only reported.

## Decision

- **The harness is a module of the `in_process` test binary, `tests/in_process/retrieval_eval/`, and a client of a real
  daemon over HTTP only.**
  It writes through the REST API and searches through `POST /api/search`, so it measures what a client gets
  (filters, fusion and expansion included) and never reaches the store, which is what an integration is held to
  (invariant 8).
  Living in the test tree keeps it out of the binary and out of the release allowlist,
  and being a module of the existing binary adds no second link of the 600 MB server.
  There is no `memcastle eval` command, no MCP tool and no route.
- **The everyday suite runs the small bundled dataset as a regression floor, and everything heavier is an `#[ignore]`d test
  behind a `mise` task.**
  `tests/fixtures/retrieval/core.json` (59 drawers, 39 judged queries in five categories) runs in `mise run test` against
  `baseline.json`, and fails when any metric of any configuration falls more than 0.02.
  `eval`, `eval:baseline`, `eval:compare` and `eval:longmemeval` are explicit and are not part of `check` or `ci`.
- **Vectors come from the harness, not from a provider.**
  The daemon runs with embeddings off, the harness attaches a deterministic vector to each drawer
  (`PUT /api/drawers/{id}/embedding`) and sends one with each query (`query_embedding`).
  The stand-in model is a hashed bag of concepts defined by the dataset, with a faint dense component so unrelated texts
  never tie exactly.
  Quality is therefore a property of the engine and the dataset, and a report records where its vectors came from
  so a provider run is never mistaken for it.
- **Time is expressed in epochs, seeded over HTTP.**
  The daemon stamps validity times itself, so the harness writes epoch by epoch, records an instant after each,
  and a temporal query refers to those instants.
  No query carries a wall-clock date.
  The alternative needs the store, which the harness must not touch.
- **Equal scores are ordered by the dataset's own document id before scoring.**
  The daemon breaks ties by an identifier it generates at random, and a metric must not vary with it.
- **Graph expansion is measured on top of lexical search.**
  Expansion appends related drawers after the direct hits, and a vector search always fills its page,
  so the related drawers would fall beyond every cut-off, seeded by whatever filled the page.
- **A report is JSON and carries its configuration.**
  It names the dataset (with the SHA-256 of its bytes), the vectors, the embedding provider and model the daemon had,
  the backend, the cut-offs, the repetitions and the scale.
  A comparison refuses datasets or parameters that differ instead of comparing them.
- **The baseline holds quality only.**
  Latency and ingest rate are reported and compared by hand on one machine, never committed and never asserted.
- **LongMemEval is a converter for a file the user downloads.**
  Sessions become drawers in a wing per question, the question is the query, and answer sessions are the judgments.
  The data is never bundled, a synthetic file in its format covers the converter in the suite,
  and the report states that it is session retrieval and not a LongMemEval score.

## Alternatives rejected

- **A `memcastle eval` subcommand.**
  It would put benchmark code, a dataset format and an invariant to defend into the shipped binary,
  for something a contributor runs and a user does not.
  The same harness could be promoted later; nothing here prevents it.
- **A separate crate.**
  A second lockfile and toolchain to maintain, and a second copy of the daemon-in-a-test helpers.
- **Seeding through the store with fixed timestamps.**
  Fully fixed dates, but it bypasses the HTTP path and couples the benchmark to storage internals,
  and the single-writer rule (invariant 4) would have to be argued around.
- **Embedding through a stub provider.**
  More realistic for the embed pipeline, but slower, and it puts the sweep and its timing into the retrieval numbers.
  A provider is supported for LongMemEval, where it is the point.
- **Asserting timings in the suite.**
  Machine-dependent, so it would flake on a loaded runner, and issue #120 rules out a release gate.
- **Criterion benchmarks.**
  They measure a function, not the retrieval a client sees, and say nothing about quality.
- **Vendoring a public benchmark.**
  Large, licensed by its authors, and a different question from regression detection on our own contract.

## Consequences

- The everyday suite gains one test of about half a minute (seeding a daemon is most of it), with its own slow-timeout
  override, and a baseline file that a deliberate retrieval change must update in the same pull request.
- A change to the dataset invalidates the baseline.
  The comparison says so, through the hash, rather than reporting a difference that is the data.
- The suite's dataset is small, so its numbers cover what it contains and no more:
  the documentation page states what they do and do not establish.
- A new retrieval capability adds a configuration and queries to the dataset in the same change,
  and refreshes the baseline.
- The harness measures the daemon of the checkout it runs in, so comparing two implementations means running it on each.
- Nothing new ships: the release allowlist is explicit, so the fixtures are not in any artifact.
