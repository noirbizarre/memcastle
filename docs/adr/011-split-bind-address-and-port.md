# ADR-011: The listener's address and port are separate settings, bound before the daemon does anything else

## Status

Accepted

## Context

The daemon's listener was one setting, `server.bind`, holding a full `host:port` socket address.
That made the common change, "same address, different port", restate an address nobody wanted to change,
and it could not be spelled the way the rest of the tooling world spells it: `--bind 127.0.0.1 --port 8787`.

The listener is also the most likely thing to fail at startup: another process, or another daemon, already has the port.
The daemon used to bind only after it had created the palace directory, migrated storage, re-queued interrupted jobs
and started the scheduler.
A start that was always going to fail had already changed things,
and left the dispatch task running when it returned the error.

MemCastle has no authentication.
Listening beyond loopback exposes the palace to whoever can reach the machine,
so the default must stay local-only and leaving it must be a deliberate act.

## Decision

- **`bind` is an IP address and `port` is a port.**
  `[server] bind` and `port`, `MEMCASTLE_BIND` and `MEMCASTLE_PORT`, `--bind` and `--port` on `serve` and `restart`.
  Each layer of the usual precedence (defaults, config file, environment, command line) sets either independently.
  `ServerConfig::socket_addr` is the only place the two are combined.
- **The defaults are `127.0.0.1` and `8420`.**
  Nothing listens on a wildcard address unless someone writes one.
  Port `0` stays valid and means "any free port", which the tests rely on;
  the registry file records the port the OS chose.
- **The old `host:port` form is refused, with a message that says what changed.**
  A `bind` that still carries a port names `--port`, `MEMCASTLE_PORT` and `server.port` in its error,
  rather than a bare "invalid IP address syntax".
  The project is pre-release, so this is a breaking change made once, not a compatibility layer kept for good.
- **The listener is bound first.**
  `server::run` binds before creating the palace, connecting storage, migrating or recovering.
  A taken or unavailable address fails the start having changed nothing.
  The registry file, which is how clients find the daemon, is still written only once the daemon is ready to serve.
- **A failed bind is its own diagnostic, `memcastle::server::bind_failed`.**
  It names the address, and its help depends on the OS error: port in use, permission denied, address not on this machine.
  Each has a different remedy, and one generic "check the bind address" sent people to the wrong setting.
- **Clients dial loopback for a wildcard listener.**
  A daemon on `0.0.0.0` or `::` records that in its registry file, which is not an address every platform can connect to.

## Alternatives rejected

- **Keep `bind` as a socket address and add a port override.**
  Least churn, but `--bind 127.0.0.1` from the request would not parse,
  and there would still be two ways to say the port.
- **Accept both forms in `bind`.**
  Backward compatible, but a port in `bind` and a port in `port` need a precedence rule of their own,
  and every layer would have to document it.
- **Warn when the address is not loopback.**
  Considered and declined: the default is already loopback, and choosing another address is an explicit act.
- **Bind after startup, as before.**
  Fewer moving parts in `run`, but every failed start pays for its side effects.

## Consequences

- An existing `bind = "127.0.0.1:8420"` in a config file, or `MEMCASTLE_BIND` or `--bind` with a port,
  stops working and says how to fix it.
- Connections that reach the listener while storage is opening wait in its backlog instead of being refused.
  A client polling `/api/health` sees a slow answer rather than a refused connection during those seconds;
  the registry file still appears only when the daemon is ready.
- Exposing the daemon beyond loopback is one setting away, and nothing but the operator stands between the palace
  and the network.
  Authentication is a separate decision.

## Amendment (2026-09-30)

Authentication was decided in [ADR-014](014-optional-token-authentication.md).
It is optional and off by default, so the loopback default above still stands, and the daemon logs a warning when it
listens beyond loopback with authentication disabled.
