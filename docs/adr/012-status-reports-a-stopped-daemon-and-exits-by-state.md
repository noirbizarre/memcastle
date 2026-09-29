# ADR-012: `memcastle status` answers for a stopped daemon too, and its exit code says which state it found

## Status

Accepted

## Context

`memcastle status` printed the daemon's status counts as JSON, and failed with `memcastle::client::not_running`
when nothing answered.
It could not answer most of what someone opening a terminal wants to know:
whether a daemon is running, where it listens, which palace it serves, whether its datastore is healthy and migrated,
and what to type next.
The address it had dialed was not shown, the palace path was known only to the CLI's own configuration,
and "the daemon is up but its database is not" looked the same as "no daemon".

`GET /api/health` is deliberately static: it proves the process answers, and touches nothing else.
Orchestrators poll it, and a database hiccup must not make one restart a daemon that would recover on its own.

Discovery already exists: a live registry file, then the configured address (see ADR-011).
The registry file is a hint and never the source of truth for "is a daemon running".

## Decision

- **`status` is a report, not just a probe.**
  It prints whether a daemon answered, the endpoint it dialed and whether that came from the registry file or the
  configuration, the MCP URL, the palace name and path, the datastore's backend, health and migration state, the counts,
  and the next command to run.
  `--json` prints the same report for scripts.
  The flag lives on `status` alone: no other command has a human rendering, so a global flag would promise one.
- **A stopped daemon is an answer.**
  With nothing listening, `status` prints the configured endpoint, the palace path and the registry file's state
  (`absent`, `live`, or `stale` when the recorded PID is gone) and how to start the daemon.
  Nothing new is discovered: it is the same registry-then-configuration resolution every command uses.
- **The exit code carries the state.**
  0 is a running daemon with a healthy datastore, 1 is a running daemon that is degraded (datastore unreachable or
  migrations pending) or any error, 3 is no daemon.
  Three follows `systemctl status`, and keeps "stopped" distinguishable from "broken" in a script.
  A timeout or a 5xx from something that does answer stays an error, because it is not evidence of absence.
- **Datastore health is part of `/api/status`, which stays HTTP 200.**
  `AppServices::status` pings the store, reads the migration watermark and reports both under `datastore`,
  with the backend kind and a location that never carries a password.
  A failing ping is reported as `datastore.ok = false` with the error, and zeroed counts, rather than as a 500:
  the daemon is up, and a 500 would make it look absent exactly when the difference matters.
- **`/api/health` stays a cheap liveness check.**
- **The report's new fields are additive and defaulted.**
  A newer CLI reads an older daemon's report and the reverse.
  The MCP `memcastle_status` tool serves the same report.

## Alternatives rejected

- **Keep failing with `not_running` when nothing answers.**
  Correct, but a stopped daemon is the case where `status` is most useful, and a diagnostic on stderr is not
  something a script can read.
- **Make `/api/health` check the datastore and return 503.**
  Better for a load balancer that should stop routing to a broken node, worse for a supervisor that restarts on failure.
  Nothing here needs that yet, and it can be added as a separate readiness endpoint without changing `status`.
- **A global `--json` or `--format`.**
  Every command already prints JSON, so it would change nothing for them and promise a human form that does not exist.
- **A new discovery mechanism, such as a socket or a PID file next to the registry file.**
  The registry file and the configured address already answer the question, and a second source is a second thing to
  go stale.
- **Return 1 for "not running".**
  The simplest contract, but `memcastle status || memcastle serve` then also starts a daemon over a broken one.

## Consequences

- The CLI's `status` JSON changed shape: the daemon's report is now under `daemon`, next to `running`, `endpoint`,
  `endpoint_source`, `mcp_url`, `palace_path` and `registry`.
  The REST and MCP reports only gained fields.
  These field names are a scripting contract from here on.
- A `status` request now runs a ping and reads the migration watermark on top of the counts it already read.
- On platforms without a process-liveness probe, a stale registry file is reported as `live`, as discovery already
  treats it.
