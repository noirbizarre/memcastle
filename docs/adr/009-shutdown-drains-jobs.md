# ADR-009: Shutdown drains running jobs and hands them back to the queue

## Status

Accepted

## Context

A daemon stops for ordinary reasons (a deploy, a config change, SIGTERM from a service manager).
Treating each stop as a crash is wrong in two ways.
A job left `Running` waits for the next start's recovery, which spends crash-recovery budget on a job that did not fail
(see `Job::recovery_attempts`), and it loses all work since its last checkpoint.
The opposite extreme, waiting for every job to finish, makes shutdown take as long as the longest mine,
which no service manager will wait for.

Two things constrain the answer.
A user's pause is a promise: a restart must not silently un-pause a job they paused.
And some handlers had no way to be interrupted at all: audit and repair never looked at the pause flag.

## Decision

Shutdown is a bounded drain.

- **Stop claiming, then interrupt.**
  The dispatch loop exits, and every running job is asked to stop at its next unit-of-work boundary.
  The interrupt is an ordinary pause request that handlers already honour, so no handler has a second code path;
  what differs is who asked, which the scheduler records (`JobControl::was_interrupted`).
- **Hand the job back.**
  A job stopped by shutdown checkpoints and goes straight back to `Queued`, so the next daemon resumes it unasked.
  A job the *user* paused stays `Paused`: a user's pause always wins over a shutdown interrupt, even one already in flight,
  and a pause or cancel request is persisted on the job record so it also survives a crash that lands first.
- **Every handler can be interrupted.**
  Audit checks between wings and every 500 drawers, and an applied repair before each delete.
  Neither keeps a checkpoint, and a resumed run starts over, which is safe ([ADR-008](008-replay-safe-job-resume.md)).
- **The wait is bounded and configurable.**
  `jobs.drain_timeout_secs` (default 10, `MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS`, validated to 1 to 86400)
  is how long the drain waits for the last worker.
  A job still running after it, stuck inside one long unit of work, is left `Running`
  and is recovered by `Scheduler::recover` on the next start, losing only the work since its last checkpoint.
- **A stuck job cannot hold the exit hostage.**
  The bound is what makes an unbounded unit of work a cost (redone work) instead of a hang.

## Alternatives rejected

- **Wait for jobs to complete.**
  Unbounded, and turns every restart of a busy daemon into an outage.
- **Kill running jobs and rely on crash recovery.**
  Correct, but it charges the crash budget for a clean stop and discards work that a checkpoint would have kept;
  after a few deploys an innocent long job would be failed.
- **Persist a "shutting down" state on each job.**
  The re-queue already records the outcome; an extra state would have to be cleaned up by whichever daemon starts next.
- **Reject pause for audit and repair instead of supporting it.**
  It would leave shutdown waiting out the full timeout for a job that could have stopped at once.

## Consequences

- A clean restart costs, at most, the unit of work each job was in the middle of.
- A daemon killed uncleanly (SIGKILL, power loss) still goes through recovery, which honours a persisted pause or cancel
  and otherwise spends one unit of crash-recovery budget.
- The drain timeout is a trade-off users tune: longer keeps more work, shorter meets a supervisor's stop timeout.
  Setting it above the supervisor's own timeout means the supervisor kills the daemon mid-drain, which recovery then handles.
- A job that ignores the interrupt is visible: the daemon logs how many were left behind.
