# ADR-008: Resuming a job is replay-safe

## Status

Accepted

## Context

A job handler writes an item's records and only then saves its checkpoint (`{"next_index": n}`).
A crash between the two leaves the records written and the checkpoint stale, so the resumed attempt redoes that item.
With the original design that duplicated it:

- mining and checkpoint created each drawer under a fresh random id, so the redone item stored a second drawer;
- a replayed `add` fact opened a second current edge for the same fact.

Resume after a crash is not an edge case: `Scheduler::recover` (see the [architecture](../architecture.md))
re-queues every job left `Running` (on a remote palace, only those whose lease has expired),
a shutdown re-queues in-flight jobs, and a lease that lapses
([ADR-006](006-job-leases.md)) hands a job to another daemon that starts from the last checkpoint.

## Decision

Redoing an item must land on the record it already wrote.

- **Ids are derived, not random.**
  A drawer's id is derived from the job id and the item index (`mine-drawer:{index}`, `checkpoint-drawer:{index}`)
  and a fact edge's id likewise (`checkpoint-edge:{index}`), by `DrawerId::derive` and `RelationshipId::derive`
  (SHA-256 over the seed and the name, shaped like a UUID so it round-trips through every store write).
- **Writes skip what exists.**
  `create_drawer_once` and `create_relationship` do nothing if the id is already there.
  `supersede_relationship` is skipped if the replacement exists, and `invalidate_relationship` on an edge that is already
  closed changes nothing (and keeps its original close time).
  A relationship id that never existed is still an error, so replay safety does not hide a mistyped id.
- **The checkpoint keys and the id strings are persisted state.**
  `next_index` (mining, checkpoint), `next_step` (demo) and the derivation names are part of what is on disk;
  renaming one makes an in-flight job restart or duplicate, so they are not renamed and tests resume a checkpoint
  written in the current format.
- **Handlers that only read, or only delete, are idempotent by construction.**
  An audit only reads; a repair recomputes the live orphan set, and deleting an already-deleted drawer is a no-op.
  They keep no checkpoint, and a resumed run starts over.

## Alternatives rejected

- **Make the write and the checkpoint one transaction.**
  A handler's unit of work spans several records and, for a fact, several statements; a transaction per item would
  serialise everything on the job record that the heartbeat and the user's pause requests also write.
  It would also not cover a resume by a different daemon.
- **Deduplicate by content hash.**
  Two identical files, or the same fact stated twice, are legitimately two records; content is the wrong identity.
- **Checkpoint before writing.**
  Trades a duplicate for a lost item: a crash after the checkpoint and before the write skips the item forever,
  which is worse, because nothing reports it.

## Consequences

- A job can be resumed, retried, re-queued by shutdown or taken over after a lease lapses with the same result:
  each record exists once.
  This is what lets [ADR-006](006-job-leases.md) settle for at-least-once execution.
- Ids are a function of (job, item), so re-running a *different* job over the same input still creates new records;
  the guarantee is per job, not global deduplication.
- The derivation strings are a compatibility surface.
  Changing one needs a data migration.
