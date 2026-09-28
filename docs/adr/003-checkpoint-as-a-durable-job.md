# ADR-003: Checkpoint is a durable job; diary writes are a direct call

## Status

Accepted

## Context

`pi-palace`, the reference implementation this project departs from, routes every write through its daemon's job queue
specifically to dodge a multi-process file-lock race between independent CLI invocations.
MemCastle already has one process, one writer, one daemon per palace —
that race doesn't exist here, so "goes through a job" can't be justified by lock-avoidance alone;
each write path has to earn a place in the queue on its own terms.

Two durable-write paths exist in Phase 1: checkpoint (persisting an already-classified batch of memory items,
sometimes under time pressure right before a crash) and diary (an agent journaling one free-text entry).

## Decision

`JobKind::Checkpoint` is a durable, scheduler-dispatched job because it needs properties only the job queue provides:

- **Priority-based preemption**: `AppServices::emergency_checkpoint` submits at `Priority::Critical`,
  jumping every other queued job (including `Background`-priority mining) —
  the entire point of an emergency checkpoint is to save state before a crash,
  which requires the scheduler's priority-ordered claim
  (`job_status_idx ON job FIELDS status, priority, created_at`) to mean something.
  The non-emergency path (`AppServices::checkpoint`) submits at `Priority::High`.
  Both route through one shared internal submission helper — one job kind, two priorities, not two job kinds.
- **Resumability**: a checkpoint payload can carry multiple items;
  `checkpoint::run` mirrors `mining::run`'s per-item cooperative pause/cancel,
  persisting `{"next_index": n}` after each item
  so a paused or crash-recovered checkpoint job resumes from where it left off, not from zero.
- **Deliberately no artificial per-item delay** (unlike `jobs::demo`'s `STEP_DELAY`):
  synthetic latency here would work against the very feature — saving state before a crash —
  that motivates emergency checkpoints existing.
  Resumability is instead proven deterministically in tests by calling `run` twice against a forced pause,
  rather than racing wall-clock time against a live scheduler.

`AppServices::diary_write`/`diary_read` are direct, synchronous calls — not job-queued —
because none of the above forces apply:
a diary entry is one small, atomic drawer write with no multi-item resumability need and no preemption requirement.
They still go through the exact same `require_write`/`require_read` `MemoryMode` policy as every other memory operation;
the sync-vs-job split is only about scheduler necessity, never about bypassing consistency or mode enforcement.

## Alternatives rejected

- Routing diary writes through the job queue, `pi-palace`-style, for uniformity with checkpoint/mining —
  rejected because the reason `pi-palace` needs that (multi-process lock contention)
  doesn't exist in MemCastle's single-daemon/single-writer design;
  adding queue latency and job bookkeeping to one small write would be pure cost.
- Making checkpoint a synchronous direct call like diary — rejected because it would give up priority preemption
  (no way for an emergency checkpoint to jump ahead of queued mining work)
  and crash-resumability (an interrupted checkpoint batch would have no persisted resume point).

## Consequences

- Checkpoint jobs get every job-queue guarantee for free:
  crash recovery (`Scheduler::recover` re-queues an interrupted checkpoint job on restart, attempt-budget permitting),
  retry via `JobEvent::Retry`, and visibility through `list_jobs`/`get_job`.
- An emergency checkpoint can and will delay already-queued `Background`-priority mining behind it —
  the intended trade-off, not a bug.
- Diary writes have no queue-provided retry/attempt-budget:
  a failed diary write returns `Err` straight to the caller, which must retry itself if it wants to.
  Acceptable because a diary write is never decomposed into resumable units in the first place.
