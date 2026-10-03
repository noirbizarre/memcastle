# ADR-017: The daemon's background lifecycle lives under `memcastle daemon`, and `serve` stays the foreground server

## Status

Accepted, amends [ADR-011](011-split-bind-address-and-port.md), [ADR-012](012-status-reports-a-stopped-daemon-and-exits-by-state.md)
and [ADR-014](014-optional-token-authentication.md) (the commands they name moved)

## Context

Running the daemon was spread over four top-level words with unequal footing:

- `serve` ran it in the foreground, and `daemon` was an alias for it.
- `stop` asked it to shut down.
- `restart` stopped it, spawned a detached `serve` and waited until that was serving,
  and quietly doubled as the only way to start a daemon in the background.
- Nothing started one in the background on purpose, so a person who only wanted their terminal back had to learn
  that "restart" was the answer.

The alias made `daemon` read as a synonym for the foreground server, while the word is the natural name for the
background process and for the group of commands that manage it.
`db` already shows the shape (`db start`, `db stop`, `db status`), so a flat `stop` and `restart` beside a `daemon`
alias were the odd ones out.

The constraints are the invariants:

- The CLI has no business logic MCP and HTTP cannot reuse (invariant 1).
  Starting a process is not business logic, but it must stay clear of `store` and `jobs`.
- One daemon per embedded palace (invariant 4): a second `serve` on a held palace only dies on the file lock,
  after a failed startup.
- A supervisor needs a foreground process.
  systemd `Type=simple`, launchd and containers all want to own the process, not a parent that exits.

## Decision

- **`daemon` is a command group: `daemon start`, `daemon stop` and `daemon restart`.**
  `serve` stays what it was, minus its `daemon` alias: the daemon in the foreground, logging to standard error,
  which is what a supervisor runs.
- **`stop` and `restart` are removed from the top level, with no alias.**
  The project is pre-release, so this is a breaking change made once, as in [ADR-011](011-split-bind-address-and-port.md)
  and [ADR-015](015-database-admin-endpoint.md), not a compatibility layer kept for good.
  A bare `memcastle daemon` names no action and prints help instead of starting something.
- **`daemon start` spawns `serve` detached and waits until it is serving.**
  "Serving" is the registry file appearing with a live daemon behind it, the signal every other command discovers the
  daemon by, not the process merely existing.
  It is the spawn-and-wait half of what `restart` already did, now reachable on its own.
- **`daemon start` fails when a daemon already answers for the palace.**
  The error is `memcastle::client::already_running`, naming the address and pointing at `daemon restart` and `status`.
  A second daemon would die on the palace lock after the cost of a failed startup, and "start" succeeding without having
  started anything would hide that the flags it was given were ignored.
- **`daemon restart` is unchanged in behaviour,** including starting a daemon when none is running.
  `daemon start` and `daemon restart` share `serve`'s flags and pass them, `--config` and the resolved `--palace` on.
- **A background daemon is detached for real.**
  Standard streams go to null, and the process leaves the launching terminal's group (its own process group on Unix,
  `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on Windows).
  Otherwise a Ctrl-C in the terminal that ran `daemon start` would kill the daemon it had just started.
- **The CLI's reach does not grow.**
  `daemon start` and `daemon restart` use `server::lifecycle` and `DaemonClient`, and touch neither `store` nor `jobs`;
  `daemon stop` is only a `DaemonClient` call.
  Invariant 1 now names `daemon start` and `daemon restart` where it named `restart`, and the `store-isolation` hook
  still holds without change.
- **`status` stays top-level.**
  It is the command a script runs without knowing whether a daemon exists, and `daemon status` would only be a second
  spelling of it.

## Alternatives rejected

- **Keep `stop` and `restart` as top-level commands, or as hidden aliases.**
  Two spellings for every action, and `--help` either still lists the flat words or teaches one thing while accepting
  another.
  A hidden alias is a compatibility layer for a pre-release interface with no users to protect.
- **`daemon start --foreground` instead of `serve`.**
  One entry point, but every systemd unit, test and debugging recipe would change, and "the command that starts the
  background process" would have a flag meaning "do not".
  `serve` as the foreground server and `daemon` as the background one say which is which.
- **`daemon start` as a no-op that succeeds when a daemon is running.**
  Idempotent, which scripts like, but flags such as `--port 9000` would be silently ignored by the daemon that was
  already there, and the caller would believe the daemon listens where it does not.
  `daemon restart` is the idempotent spelling for "make it what I asked for".
- **A real daemonizing `serve` (double fork, `setsid`, pidfile).**
  More machinery than a spawn needs, cannot work on Windows, and breaks the supervisor case that `serve` exists for.
- **Redirecting the background daemon's log to a file.**
  Useful, but it needs a location, rotation and a place in the configuration, and the documentation already sends
  anyone who needs the log to `serve` in the foreground or to a supervisor.
  Left for a later decision, not smuggled in with this one.

## Consequences

- Scripts and notes that said `memcastle stop`, `memcastle restart` or `memcastle daemon` (for `serve`) break and have to
  change, and the old words now fail as unknown commands.
- `status` points at `memcastle daemon start`, `daemon restart` and `daemon stop`, and `not_running` points at
  `daemon start` (or `serve` in the foreground).
- `daemon start` and `daemon restart` are a best-effort detached spawn, not a supervisor:
  nothing restarts the daemon if it dies, and its log output is discarded.
- `daemon stop` still returns once the shutdown request is accepted, without waiting for the process to exit.
- A daemon started this way no longer shares its launching terminal's process group, so signals sent to that group
  (Ctrl-C, closing the terminal on most shells) leave it running until `daemon stop`.
- The detach on Windows is code a Linux CI run does not exercise.
