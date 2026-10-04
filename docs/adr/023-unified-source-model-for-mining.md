# ADR-023: Mining goes through one source model, in three stages, with the cursor kept by MemCastle

## Status

Accepted, builds on [ADR-004](004-versioned-database-migrations.md) (the two new tables are SurrealKit schema, with no
Rust DDL and no data migration),
[ADR-007](007-memory-mode-gate-follows-data-access.md) (mining a source is a write),
[ADR-008](008-replay-safe-job-resume.md) (ids derived from the job keep a replay from duplicating)
and [ADR-021](021-richer-retrieval.md) ("chunking is a mining decision", made here).
It changes one consequence of [ADR-008](008-replay-safe-job-resume.md) and [ADR-018](018-palace-hierarchy-management.md):
re-mining no longer duplicates.

## Context

Mining read one kind of source, a directory, and wrote one drawer per file, verbatim.
It remembered nothing between jobs: mining the same tree twice filed it twice, a file over 256 KiB was skipped, and a
source that is not a directory had nowhere to go.

Issues #85 and #39 ask for more than a second reader.
Phase 5 plans adapters for Slack, ChatGPT, Claude, Codex, OpenCode, GitHub and Atlassian, and the transcripts of the
agents MemCastle already serves.
Each of those would otherwise bring its own idea of "what is new", "what is the same document" and "where did I stop",
and the second adapter would be the first to disagree with the first.

Two forces pull on the design.
Memory can reach the palace two ways: an agent writes it through MCP and spends its tokens doing so, or MemCastle
acquires it directly and spends none.
The second way must not need a model, a running agent or a credential written into memory.
And the thing that differs between sources (discovery, authentication, formats) is small next to the thing that does not
(chunking, deduplication, durability, resumption), so the second must exist once.

## Decision

- **Mining is three stages, and only the first two know what a source is.**

  ```mermaid
  flowchart LR
      A["acquire<br/>adapter: discover, read"] --> N["normalize<br/>adapter: pure"] --> I["chunk and ingest<br/>core: drawers, cursor"] -.-> E["enrich (optional, later)<br/>entities, summaries"]
  ```

  A `SourceAdapter` finds candidates past a cursor, reads one into a `RawDocument` and normalizes it into a
  `CanonicalDocument`.
  Nothing in it touches the store, the job or a drawer.
  The pipeline owns everything after: the shared chunker, deduplication, the drawers, the cursor and the job's
  checkpoint.
  Semantic work (#40's entity extraction, summaries) is a further stage that reads what ingest filed.
  It is not implemented here, and the contract says where it attaches: it never rewrites drawers.
- **A source has an identity, and MemCastle keeps its cursor.**
  The identity is `(provider, account, locator)`, and the source's id is derived from it, so two jobs mining the same
  place share one record.
  The record holds the adapter's cursor, opaque to the core; the last job to advance it; and a credential *reference*
  (an environment variable's name, a file's path), never a value.
  The cursor is stored on the source, not in the job: a later job continues where an earlier one stopped, which is what
  "incremental" means.
- **A cursor is an optimisation, never a correctness mechanism.**
  Ingest is idempotent, so reading a document twice is always safe, and a lost, reset (`mine --full`) or distrusted cursor
  costs reading, not duplicates.
  The cursor moves only after the drawers and the document record are written, so it is never ahead of the data.
- **Idempotence is by document identity and revision, not by content.**
  Each document of a source has a `source_document` record: the revision last ingested, and the chunks (position,
  hash, drawer) it became.
  An unchanged revision is skipped before it is normalized.
  A changed one keeps every chunk whose hash is unchanged, supersedes only those that differ ([ADR-021](021-richer-retrieval.md)'s
  `valid_to`, so history stays), and closes chunks past the end of a document that shrank.
  Appending to a transcript therefore rewrites its last chunk and nothing before it.
  Drawer ids are derived from the job as well as the chunk, so a replay inside a job lands on what it wrote
  ([ADR-008](008-replay-safe-job-resume.md)) while a document reverted to an older text opens a new drawer rather than
  colliding with the closed one.
- **Raw documents are kept only where the origin is not durable.**
  An adapter declares `retains_raw`.
  A file on disk does not need a second copy in the palace; a session file the user rotates away, or a chat export,
  does.
- **Chunking is shared, deterministic and in characters.**
  A document that fits is one drawer, verbatim.
  Otherwise whole segments (a message, a paragraph the adapter delimited) are packed up to `mining.chunk_chars`, and only
  a segment larger than a chunk is cut, at a paragraph break, then a line break, then mid-line.
  Determinism is what lets a re-mine recognise a chunk by its hash.
- **A source is addressed by name, and the wire shape of a directory job does not change.**
  `JobKind::Mine` keeps `{"type": "mine", "path": ...}`; a source adapter is
  `{"type": "mine", "provider": ..., "locator": ...}`, and `full` is added only when true.
  Adding an adapter adds a provider name, not a job kind, an enum variant or a scheduler arm.
  The jobs already on disk deserialize unchanged.
- **Two adapters ship, which prove the contract rather than complete the list.**
  `directory` is the previous behaviour as an adapter, now incremental and chunked.
  `pi-sessions` reads the Pi coding agent's session files straight from disk.
  Both use a modification-time watermark cursor; `pi-sessions` retains raw.
  Neither needs credentials, so `CredentialRef` is defined and stored but exercised by no shipped adapter.
- **Enforcement is a test, not a convention.**
  `tests/source_isolation.rs` holds the pipeline and the chunker to naming no provider and reading no file, adapters to
  never reaching the store or the jobs, and the pipeline to being the only writer of source records.

## Alternatives rejected

- **A `MiningSource` variant, and a match arm, per source.**
  It is the seam #58 left, and it is the second abstraction: every arm would re-implement dedup and cursors.
  The variant stays only for the legacy directory wire form.
- **Deduplicate by content hash.**
  [ADR-008](008-replay-safe-job-resume.md) rejected it for replay, and it is wrong here too: two documents with the same
  text are two documents, and an edited document would never be recognised as the same one.
  Identity is the source's own id for the document, and the hash only decides which chunks of it changed.
- **Keep the cursor in the job's checkpoint only.**
  A new job would start from scratch, so a daily mine would read everything daily.
  The job's checkpoint still holds the run's own cursor, so a paused or crashed run resumes where it was, whatever `--full`
  said.
- **No `source_document` table: derive everything from the drawers' `source` field.**
  Unchanged detection would need a query over drawers per document, and "which drawers did this revision become" has no
  home once a document shrinks.
  The table is small bookkeeping, derived and rebuildable, and no search reads it.
- **Make the adapter responsible for chunking, or for filing.**
  Then every adapter chooses its own sizes and its own idea of idempotence, and the embedding window of
  [ADR-021](021-richer-retrieval.md) would be honoured by some and not others.
- **An adapter per MCP tool or a registry of user-defined sources.**
  Registering sources is a second feature (storage, CRUD, credentials).
  A source is created the first time a job mines it; `memcastle sources` shows what exists.
  Nothing precludes a registry later, since the record already exists.
- **Mining Pi sessions from the Pi integration.**
  Invariant 8: an integration decides *when* to call MemCastle and never reads storage.
  A daemon that reads the history itself also mines sessions that ended long ago, with no agent running.

## Consequences

- Re-mining is idempotent: an unchanged tree files nothing, an edit supersedes only what changed.
  This replaces the "re-mining duplicates, and only the first copy keeps its name" behaviour of
  [ADR-008](008-replay-safe-job-resume.md) and [ADR-018](018-palace-hierarchy-management.md), whose notes point here.
- Documents mined before this ADR have no `source_document` record.
  The first mine under the new model files them once more, and the older copy keeps the drawer name.
  After that they are tracked.
  Nothing migrates the old drawers, and deleting them is the user's call.
- A watermark cursor cannot see a file that appears with an old modification time or a file that was deleted, and it does
  not propagate deletions.
  `mine --full` re-reads for the first; the second is a non-goal here.
  A provider with a change feed supplies a cursor that does see them.
- `--full` re-reads and skips what is unchanged.
  Changing `mining.chunk_chars` therefore does not re-chunk documents already ingested: their revision has not moved.
- A mining job is bounded by `mining.max_documents` and says `truncated` when there is more.
  Because the cursor persists, running it again continues, which the old fixed limit could not do.
- The `pi-sessions` reader is best effort against Pi's session format (version 3 as observed).
  Unknown entries are skipped rather than failing the session, and tool results and reasoning are deliberately not filed.
- Adding a provider is one file under `src/mining/adapters/`, one arm in `mining::run` and `mining::providers`, and a
  section in `docs/mining-sources.md`.
  The pipeline, the scheduler, the job shape, the REST routes and the MCP tool do not change.
- Authentication for external providers is designed for but not built: a credential reference is stored and never a
  value, and `config::Secret` is where a value would be held in memory.
  The first adapter that needs one decides how the reference is resolved, in its own ADR if that is not obvious.
