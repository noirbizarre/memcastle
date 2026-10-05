# ADR-015: The database admin endpoint is an opt-in listener inside the daemon, over its own database handle

## Status

Accepted, amended by a dated section at the end of this record (`db serve` renamed `db start`)
and by [ADR-035](035-web-dashboard.md) (the web dashboard signs in the way this endpoint does)

## Context

An embedded SurrealKV database has no server, so SurrealDB Studio (Surrealist) has nothing to connect to.
Developers need to inspect and query the live palace before the MemCastle web UI exists.

The constraints come from how storage is owned:

- SurrealKV takes a file lock, so the daemon is the only process that may open an embedded palace (invariant 4).
  Starting `surreal start` on the same directory fails, or worse, corrupts a view of it.
- Schema and migrations belong to SurrealKit and `crate::migrate` (invariant 5),
  so whatever is exposed must not become a second way to change the model.
- The CLI never touches `store` (invariant 1), and every route but `GET /api/health` is authenticated (invariant 6).
- A database console is the most powerful thing the daemon can offer:
  anyone who reaches it can read and rewrite the whole palace, and a browser tab on the developer's own machine can reach
  `127.0.0.1` as easily as the developer can.

No SurrealDB server code can be used as a library.
`surrealdb-core` has a generic RPC engine, but it needs the `Datastore` itself,
and the `Surreal<Any>` handle that `store` and SurrealKit share cannot be built around a datastore we construct.
The wire types and codecs, in `surrealdb-core` and `surrealdb-rpc`, are usable.

## Decision

- **The endpoint is a second listener inside the daemon, started and stopped at runtime.**
  `memcastle db start` is a thin client: it calls `POST /api/db`, and `db stop` and `db status` call `DELETE` and `GET`.
  The daemon binds the listener, so there is still one process, one writer and one open storage directory.
  `memcastle serve` never starts it, and no flag or setting does either.
- **It is an adapter over the daemon's own handle.**
  Each connection runs on a clone of the `Surreal<Any>` the daemon already holds.
  A clone is a separate session over the same datastore,
  so a Studio `USE`, `LET` or sign-in affects only that session and never moves the daemon's own queries.
  `store` gains one accessor for the clone and nothing else: no connection is opened and no storage abstraction is added.
  The module forwards SurrealQL and does not interpret it, so it is not a second application API.
- **It speaks SurrealDB's WebSocket RPC, which is what Studio uses.**
  `GET /rpc` negotiates `flatbuffers` (the Rust SDK), `cbor` or `json`,
  and `GET /health` and `GET /version` answer the probes.
  The codecs and the request and response types are SurrealDB's own, pinned to the exact `surrealdb` version.
  The methods are `query`, `use`, `let`, `unset`, `info`, `version`, `ping`, `signin`, `authenticate`, `invalidate`,
  `reset`, `attach`, `detach` and `sessions`.
  Live queries, transactions and the per-record shorthands (`select`, `create`, ...) are refused with a message that says
  to use `query`.
- **Loopback by default, and remote is a deliberate, double opt-in.**
  The default is `127.0.0.1:8000`.
  Any other address needs `--allow-remote` (or `db.allow_remote`) *and* `auth.enabled`.
  The daemon checks this when it starts the endpoint, not the CLI, so the REST route cannot be used to get around it.
  A remote bind logs a warning that names the cleartext token.
- **Authentication reuses the daemon's token, in-band.**
  A browser cannot set a header on a WebSocket, so Studio signs in with the MemCastle token as the password.
  `signin` and `authenticate` are checked by `AppServices::authenticate`,
  so the endpoint accepts exactly what the REST API accepts and a revoked token stops working at the next connection.
  A client that is not a browser may send `Authorization: Bearer` on the upgrade instead.
  Until a session has authenticated, only the handshake methods work.
  Five refused sign-ins close the connection.
  `signin` answers with the token the client presented, which is what lets Studio's `authenticate` on reconnect be checked
  the same way.
  The user is the fixed name `memcastle`, and any other is refused before the password is looked at.
  It is a fixed name rather than the operating-system user because that is the same on every machine and in every
  container, and there is nothing to resolve or document per host.
  It identifies, it does not authenticate: there is one shared credential.
  With authentication disabled there is no token, so the password is `memcastle` too:
  Studio's login form insists on a user and a password, and a known pair is better than accepting anything.
  A client that never signs in, such as a script, can still use an endpoint whose daemon has authentication disabled.
- **A browser page from another site is refused.**
  Any page in the developer's browser can dial `ws://127.0.0.1`, whatever its origin.
  The endpoint refuses a request whose `Origin` is not a page served from this machine or one added with
  `--allow-origin`, on every route.
  Clients that send no `Origin` are unaffected and are held to authentication instead.
  The SurrealDB Studio desktop app, which sends the fixed origin `app://surrealdb-studio`, is allowed too:
  a web page cannot send an `app://` origin, and the match is on the whole value, never a pattern.
  The hosted Surrealist (`https://app.surrealdb.com`) is *not* allowed by default.
- **Administration is REST and CLI only.**
  `/api/db` sits behind the same authentication layer as every other route, and there is no MCP tool for it.
  A test fails if any tool's name mentions the database.
- **It is only for an embedded palace.**
  With a remote palace there is already a server, and exposing the daemon's root session on it would hand out more access
  than the operator gave Studio.
  The request is refused with `memcastle::db::unavailable`.

## Alternatives rejected

- **A standalone database server process launched by `memcastle db`.**
  It cannot open the palace while the daemon runs, because of the file lock, so it would only work with no daemon,
  which is not "the live database".
  It would also be a third direct user of `store`, after `serve` and `migrate`, and a change to invariants 1 and 4.
- **Running `surreal start` against the SurrealKV directory.**
  Two processes on one storage directory is the thing the lock exists to prevent.
- **Implementing `RpcProtocol` from `surrealdb-core` over a `Datastore` MemCastle owns.**
  It would give full protocol fidelity, including live queries.
  But `Surreal<Any>` cannot wrap a datastore we build, and SurrealKit's `Sync` needs a `Surreal<Any>`,
  so it would mean giving up the schema ownership of invariant 5 or the remote backend.
- **A read-only mode.**
  The embedded datastore runs with authentication off, so the SDK has no read-only switch,
  and filtering statements by hand cannot be made sound against SurrealQL.
  An option that looks like a guarantee and is not is worse than none; the safety comes from the listener's scope.
- **Allowing the hosted Surrealist by default.**
  It would make the first connection work, and it would let a page served by a third party open a writable console
  onto a developer's palace.
  Naming it with `--allow-origin` is one flag.
- **A per-connection opaque token from `signin`.**
  It would avoid handing a token back, but it needs somewhere to keep and expire issued tokens,
  and it would outlive a revocation.
  The client already holds the token it presented.
- **Starting the endpoint from configuration.**
  A setting that turns it on would make "the daemon exposes its database" a property of a file that was edited once.
  An explicit command, which can be stopped, is the issue's requirement and the safer default.

## Consequences

- `surrealdb-core` and `surrealdb-rpc` become direct dependencies.
  Both are documented as internal API with no SemVer promise, so they are pinned to `=` the version `surrealdb` uses
  and are bumped together with it.
  Neither adds a storage engine, which `tests/dependencies.rs` still checks.
- Plain WebSocket carries the token in cleartext, as the REST API does.
  A remote endpoint needs a TLS-terminating proxy or a tunnel in front of it.
- The endpoint is a console onto the whole database.
  It bypasses the application layer, so a write from Studio can break an invariant the application keeps,
  and the migration watermark and the `auth_state` verifier row are in reach.
  The documentation says so.
- Whoever holds the token has full read and write access, with no read-only variant.
- Live queries and transactions are unsupported in this version.
  A `LIVE SELECT` inside `query` answers with an id that never delivers.
- Revoking a token does not close connections that already authenticated; it applies to the next sign-in.
- Stopping the endpoint, or the daemon, ends every Studio session; Studio has to reconnect.

## Amendment: `db start`, and a repeated start is not an error

The command was first called `db serve`.
It was renamed `db start` because it does not serve the database: it starts the adapter inside the daemon, and the daemon
is what serves.
There is no alias for the old name.

A start that finds the endpoint already listening now succeeds instead of failing with a conflict,
so a script or a person can run `memcastle db start` without checking first.
The decision lives in the application layer, so REST and any future dashboard behave the same as the CLI:
`POST /api/db` answers `200` with the open endpoint's details and `already_running: true`,
and the CLI prints `already running on` the URL followed by the usual details.

The answer is only honest when the request does not contradict the open endpoint.
A different `bind`, a `port` other than the one in use (`0` means any free port, so it never conflicts),
or an origin the endpoint does not allow still answers `409` with `memcastle::db::already_running`,
because reporting the old endpoint would leave the caller connecting to the wrong place.
The remedy is `memcastle db stop`, then a start with the new settings.
`allow_remote` is not compared: it only gates a new bind.
