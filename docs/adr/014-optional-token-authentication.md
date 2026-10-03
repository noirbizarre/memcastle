# ADR-014: Authentication is an optional bearer token, checked at one layer, and never an MCP capability

## Status

Accepted, amended by [ADR-017](017-daemon-lifecycle-commands-live-under-daemon.md)
(the `restart` this record names is now `memcastle daemon restart`)

## Context

The daemon answered anyone who could reach its listener.
[ADR-011](011-split-bind-address-and-port.md) kept that safe by defaulting to loopback, and deferred authentication.
0.1 needs a way to run the daemon beyond loopback, or behind a supervisor, without leaving the palace open.

The constraints come from how the daemon is used:

- The standalone local workflow must stay simple, so authentication has to be optional and off by default.
- The CLI is a thin client of the daemon's API (invariant 1 in `AGENTS.md`),
  so it cannot have a private path for managing credentials, and there is no local socket or privileged channel.
- Secrets arrive from secret managers as environment variables, so a secret must be usable without being written to a file.
- Agents reach the daemon over MCP, and an agent must not be able to mint, rotate or revoke the credential that guards it.
- OAuth/OIDC and per-user authorization are wanted later, for server deployments, and must not require rework of the
  daemon's request handling.

## Decision

- **A single shared bearer token, sent as `Authorization: Bearer <token>`.**
  It is enabled by `auth.enabled` (`MEMCASTLE_AUTH_ENABLED`), which defaults to false.
  The setting is read once, at start, from configuration only.
  A stored verifier alone never turns authentication on, so generating a token cannot change what the next restart does.
- **Two credential sources, either of which is accepted.**
  A configured shared secret (`auth.token`, or `MEMCASTLE_AUTH_TOKEN`, which is the recommended way),
  and the token made by `memcastle auth generate`.
  The CLI presents the configured secret, from the same two settings, and never takes it as an argument.
- **Generated tokens are 256 bits from the operating system, stored as a SHA-256 digest.**
  The format is `mc_` and 64 hex digits.
  A token that is high-entropy by construction has nothing to brute-force, so a plain cryptographic hash is a sufficient
  verifier and a password KDF would only add cost.
  The row, `auth_state:token`, also records the algorithm and a version, so a later scheme is recognisable as such.
  A verifier from a scheme this build does not know matches nothing.
- **Verification is constant-time and fail-closed.**
  Digests are compared with `subtle`.
  A missing, malformed or wrong token is a 401 with `WWW-Authenticate: Bearer` and the ordinary error body, code
  `memcastle::auth::unauthorized`.
  A store error while reading the verifier also refuses the request.
  401 is used, not 403, so an unidentified caller is distinguishable from an identified one refused by its memory mode,
  and from a daemon that is not there.
- **One layer guards everything.**
  A middleware on the merged router, outside both the REST routes and `/mcp`, runs before any handler or rmcp.
  The only exemption is `GET /api/health`, which `restart`, supervisors and the client's own probe rely on
  and which answers nothing but `{"status":"ok"}`.
  An unknown path is refused with 401 rather than 404, so the route table cannot be probed without a token.
- **Token administration is REST and CLI only.**
  `POST /api/auth/token` generates, replacing any previous token, and `DELETE /api/auth/token` revokes.
  While authentication is disabled they are open, which is the whole bootstrap: start without authentication, generate,
  store the token, enable authentication, restart.
  Once it is enabled they need a valid token like every other route.
  There is no MCP tool for either, and a test fails if any tool's name mentions credentials.
- **The verifier is read on every request.**
  That is one read of a single row, and it makes rotation and revocation take effect immediately, without a restart.
- **The daemon refuses to start when it could never authenticate anyone.**
  `auth.enabled` with neither a configured secret nor a stored verifier is `memcastle::auth::not_configured`,
  raised after migrations and before the scheduler starts.
- **Secrets have a type that cannot be logged.**
  `config::Secret` has a redacted `Debug` and is skipped when the configuration is serialised.
  The daemon hashes a configured secret at startup and keeps only the digest.
  The plaintext is never logged, never part of `status`, and never written to the registry file or any generated file.
  `status` reports only whether authentication is enabled.
- **The boundary is provider-agnostic.**
  The HTTP layer asks `AppServices::authenticate` whether the credential a request carried is acceptable.
  Another provider replaces that method's body, and adds credentials and roles behind it, without touching a route.

## Alternatives rejected

- **A local Unix socket for bootstrap.**
  It would let the CLI generate a token without the API, but it is a second channel to secure and document,
  it does not exist on every platform MemCastle builds for,
  and it breaks the rule that the CLI only calls the daemon's API.
- **Enabling authentication implicitly when a secret or verifier exists.**
  Fewer settings, but running `auth generate` would silently change how the daemon starts next time,
  and the disabled-then-enable bootstrap would have no moment at which to store the token safely.
- **Argon2 or another password KDF for the verifier.**
  Right for passwords a human chose, wasted on 256 random bits.
  It would also add a per-request cost to the layer that guards every call.
- **Authenticating `/api/health` too.**
  `restart` decides whether to ask a daemon to shut down by probing it, and a refused probe reads as "not running".
  Supervisors and container health checks would need the secret.
- **Leaving `/api/status` open.**
  Friendlier to monitoring, but it tells any network client the palace path, the datastore location and the bind address.
- **Only `auth generate`, with revocation as "turn authentication off".**
  Rotation fits, but revoking a leaked token would mean disabling the protection it leaked from.
- **Caching the verifier in memory.**
  Saves a read per request, and makes a revoked token keep working, and a shared remote palace's other daemons
  disagree about it, until a restart.
- **Putting the token in the registry file or the configuration by `auth generate`.**
  Would make the CLI "just work", at the cost of a plaintext credential on disk that the issue forbids.

## Consequences

- Plain HTTP carries the token in cleartext.
  TLS on the daemon's listener is still a non-goal, so a daemon exposed beyond a trusted network needs a TLS-terminating
  proxy in front of it.
- MCP clients on another host must send the header, and rmcp's default `Host` allow-list accepts only loopback names,
  which is independent of authentication.
  [Authentication](../authentication.md#mcp-clients) says how to configure a client and what the allow-list means.
- Enabling authentication with only a generated token, then revoking it, leaves a daemon nothing can authenticate to,
  and it cannot be asked to stop by the CLI.
  Recovery is to restart it with authentication off, or with a configured secret; the guide says so.
- A shared remote palace has one stored token for all its daemons, and revoking it on one revokes it on all.
  A configured secret is per daemon.
- There is one credential and no notion of who presented it.
  Users, roles, scopes and OAuth/OIDC are future server-mode work, and the layer above is where they would attach.
- The `status` report gained an `auth_enabled` field, additive and defaulted, so older clients and daemons interoperate.
