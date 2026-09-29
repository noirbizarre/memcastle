# ADR-006: Running jobs are held by a heartbeat lease, not by an assumption of one daemon

## Status

Accepted

## Context

`Scheduler::recover` used to treat every `Running` row as belonging to a dead daemon and re-queue it.
That is only safe because an embedded palace is guarded by SurrealKV's file lock: a second daemon cannot even open it.
With `Backend::Remote` nothing stops a second daemon starting.
Its `recover` would re-queue jobs the first is still running, and both would then execute them.
The claim was a `SELECT` followed by an unconditional `UPSERT`, so two daemons could also claim the same queued job.

`Job::lease_owner` was set on claim and cleared on release, but `lease_expires_at` was never populated,
so there was no way to tell a live daemon's job from an abandoned one.
ADR-001 defers server deployment; this is the job-queue half of that work.

## Decision

A running job is held by a lease that its daemon renews, and everything that could steal or clobber it checks the lease.

- **A claim is a compare-and-set.**
  `claim_next_job` selects the candidate, then writes it only `WHERE status = 'queued'`.
  If another daemon won the race the write matches nothing and the claim returns `None`; the next poll tries again.
  The claim sets `lease_owner` and `lease_expires_at = now + jobs.lease_ttl_secs` (default 30).
- **The owner renews it.**
  A heartbeat every third of the TTL extends `lease_expires_at` on each job the daemon is running,
  conditionally on the job still being `Running` and leased to that daemon.
- **Recovery goes by lease when the store is shared.**
  An embedded store is exclusive, so startup recovery still re-queues every `Running` job at once
  (the file lock proves the previous owner is gone, and waiting out a lease would delay every restart).
  A remote store is shared, so recovery, and a reaper running alongside the heartbeat,
  re-queue only jobs whose lease has expired and that the reaping daemon is not itself running.
  The write that re-queues is guarded on the lease still being the one that was read,
  so a lease renewed a moment ago is not reaped from under its owner.
  A job with no expiry (written before leases existed) counts as expired.
- **Every write by a leased worker is fenced.**
  A checkpoint and the final state are written only `WHERE lease_owner = <this worker>`.
  A write refused for that reason is `Error::LeaseLost`, and the run is abandoned rather than marked `Failed`,
  because the failure would overwrite the new owner's record with a stale verdict.
  The `lease_expires_at` and stop-request columns are kept, not overwritten, by a save while the owner still holds the job.
- **A daemon that cannot renew fences itself.**
  If a renewal finds the lease gone, the job is interrupted at its next boundary.
  If the store is unreachable for a whole TTL, every running job is interrupted,
  because a partitioned daemon is the one party that cannot be told it lost its jobs.
- **Worker ids are unique** (`memcastle-<pid>-<random>`), since fencing compares them.

On a partition the outcome is at-least-once, not exactly-once:
the partitioned daemon keeps executing until its next heartbeat or write fails,
the healthy daemon reaps and re-runs the job, and the partitioned daemon's writes to the job record are refused.
Side effects outside the job record (drawers, edges) are not fenced, but they are replay-safe by construction
(ids derived from job and item, existing records skipped), so a job run twice produces the same records once.

## Alternatives rejected

- **A daemon lock record**, like the migration lock, so a second daemon refuses to start.
  It stops the wrong thing.
  A lock is held per daemon, so a crashed daemon's lock must expire before its replacement can start,
  which is the same TTL problem, and it does nothing for the claim race or for a stalled daemon that is still alive.
  It would also forbid running several daemons against one palace, which is the point of a server deployment.
  A per-job lease gives failover without forbidding scale-out.
- **Making the file lock the only guarantee and documenting the remote backend as single-daemon.**
  Cheap, but it leaves the failure mode (two daemons silently running one job) one misconfiguration away.
- **Exactly-once execution through distributed transactions.**
  Out of proportion: handlers are already replay-safe, and at-least-once with fencing gives the same observable result.

## Consequences

- A remote palace can be shared by several daemons:
  a dead one's jobs move after at most one TTL, and a live one's are never taken.
- Each running job costs one small write per third of the TTL.
- The TTL is a trade-off users can tune: a host that pauses longer than it without being dead loses its jobs to a peer
  (harmless, by replay safety, but wasted work), and a longer TTL delays failover by the same amount.
- Clock skew between daemons shifts when a lease looks expired.
  The TTL should comfortably exceed any skew; NTP-level skew is far below the 30 second default.
- The write-conflict retry in `store::retrying_on_conflict` applies to every leased write,
  so a heartbeat racing a checkpoint costs a retry, not a lost lease.
- Two schedulers over one store are the unit-test shape for all of this (`jobs::tests`);
  SurrealKV's own lock cannot be reopened in a process, so the embedded path stays covered by `tests/persistence.rs`.
