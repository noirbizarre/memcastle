# ADR-039: OAuth credentials for mining sources are the daemon's, and a source only asks for a token

## Status

Accepted.
Builds on [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) (a source has no ambient authority, and
installing one is administrative, so its host contract grows only through a manifest permission the user consented to)
and [ADR-037](037-persistent-miner-configuration.md) (a miner's `credential` is a reference, never a secret, and
handing a credential to a source needed a contract input that did not exist).
Builds on [ADR-014](014-optional-token-authentication.md) (MCP never touches credentials, and a secret is never logged,
serialised or persisted in plaintext) and [ADR-038](038-one-output-contract-for-every-command.md) (what a command prints).
Amends ADR-026's contract: the source contract is `0.3.0`, which adds one host function.

## Context

Issue #214 observes that not every source can be reached with a static token.
Some need an interactive sign-in, a browser, a refresh token and a credential that renews, and the ChatGPT (#87) and
Codex (#89) sources may be among them.
If each source implemented that itself, every one would carry its own sign-in code, its own place to keep tokens
and its own renewal, in a sandbox that is deliberately given nothing: no environment, no filesystem it was not granted,
no way to open a browser, nowhere safe to write a secret.
The runtime needs one mechanism, reusable by a source nobody has written yet, that keeps OAuth out of the ingestion
pipeline and gives a source a token without giving it the store.

The forces:

- **Where the sign-in runs.**
  The CLI is a thin HTTP client (invariant 1) and the daemon is the process that mines, so it is the one that needs the
  token months later, with no terminal and nobody present.
- **Where the tokens live.**
  The palace database holds only digests of secrets (ADR-014), and a refresh token is a secret that must be readable.
  A configuration file is for the user to write and to commit.
  An operating system has a credential store, and many machines MemCastle runs on (a headless Linux server, a
  container) have none.
- **How a source gets a token.**
  The sandbox grants exactly what the manifest lists and the user consented to; an environment variable the host
  fills in would hold the token for a whole call and for whatever the guest does with its environment,
  and could expire in the middle of a long discovery.
- **What a source may be able to do with a sign-in.**
  A sign-in lets installed code act on someone's account, which is as administrative as installing the code.
- **A browser flow needs somewhere to land.**
  The authentication layer has one public route, the liveness probe, and that is an invariant.

## Decision

- **The daemon runs the sign-in, keeps the tokens and renews them; a source only asks for a token.**
  `memcastle source auth <source>` is an HTTP call to two administrative routes
  (`POST /api/source-packages/{name}/auth`, then a long poll on `.../auth/{flow}/wait`).
  The daemon talks to the provider, finishes the flow on a task of its own (so it completes even if the command goes away),
  and the command only shows the code or the address, opens the browser when there is one, and waits.
  The CLI therefore stays a client of the daemon, with no new exception to invariant 1.
- **A source declares a sign-in as a permission, and asks for the result through one host function.**
  `[permissions.oauth]` names a public client (`client_id`), the endpoints and the scopes, with no client secret,
  and `capabilities.needs_credentials` must be set.
  Because it is a permission, it is shown at consent and part of the consent digest,
  so a source that changes its client, endpoints or scopes needs consent again,
  while a source that declares none keeps the digest it was consented under.
  The contract (`0.3.0`) adds `host.access-token`, which returns a current access token for the declared sign-in or an
  error saying why there is none.
  The host answers only a call that is granted its permissions, so `normalize` never gets one,
  and only for the source's own manifest.
  A source asks on every call that needs one, because the host renews a token that is about to expire,
  and the host holds the token only for as long as that call.
- **Flows follow from the endpoints.**
  A device authorization endpoint allows the device flow (RFC 8628),
  preferred because it needs nothing of the daemon's machine;
  an authorization endpoint allows the authorization code flow with PKCE (RFC 7636), with the redirect on a one-shot
  listener bound to `127.0.0.1` and an ephemeral port, outside the REST API, which accepts one request that carries the
  right `state` and then closes.
  The authentication layer's rule that only the liveness probe is public therefore stands, and the redirect is not a route.
  The browser flow needs a browser on the daemon's machine, and says so.
- **Tokens are kept in the platform credential store, and in an owner-only file where there is none.**
  `credentials.backend = "auto"` probes the platform store on first use (macOS Keychain, Windows Credential Manager, the
  Secret Service on Linux, through the `keyring` crate and its pure-Rust Linux backend) and falls back to
  `<credentials.dir>/<source>.json`, the directory `0700` and the file `0600`, written and renamed so a crash leaves no
  half-written token.
  `keyring` and `file` force one.
  What is kept is the refresh token, scopes and a fingerprint of the client, endpoints and scopes it was obtained under;
  the access token lives in the daemon's memory, and is written down only for a provider that issues no refresh token.
  A credential whose fingerprint is not the installed manifest's counts as missing.
  It is never in the palace database, the configuration file, a log, JSON or a REST or MCP answer.
- **Renewal is automatic, serialised and honest about why it failed.**
  A token with less than a minute left is renewed before it is handed out,
  by one caller per source while the others wait for its result,
  because a provider that rotates refresh tokens revokes the grant when an old one is used twice.
  A rotated refresh token is written back.
  A provider that says the grant is revoked (`invalid_grant` and its kin) removes the credential and fails the run with
  `memcastle::credential::required`, whose message names `memcastle source auth <source>`;
  an unreachable provider fails it with `memcastle::credential::refresh_failed` and keeps the credential for the next try.
  A run of a source that signs in checks that it is signed in before it touches a cursor or a drawer.
- **A miner's `credential = { type = "oauth" }` is a reference like the others.**
  It names nothing, is shown as `oauth` with whether the source is signed in,
  and is refused for a source that declares no sign-in.
  A miner for a source that signs in is unavailable until it is signed in whatever its `credential` says,
  because the source declared the need and a miner cannot decline it.
- **Signing a source in is administrative, and has no MCP tool.**
  An agent can read the miners (which say whether a source is signed in) and `source show`, but cannot start a sign-in,
  and `tests/in_process/auth.rs` holds both routes to authentication and every MCP tool name clear of `auth`, `credential`,
  `oauth` and `sign`.
  The one module that does all of this, `credential`, reaches no palace, no jobs and no WebAssembly runtime,
  and only `app` and the daemon's root name it (`tests/credential_isolation.rs`);
  `mining` and `jobs` see only the `AccessTokens` trait.
- **There is one command and no lifecycle subcommands.**
  `login`, `logout`, `status` and `remove` are not added: the source name chooses the flow,
  `source show` is the status, `source remove` forgets the credential, and running `source auth` again replaces it.

## Alternatives rejected

- **The CLI runs the flow and uploads the tokens.**
  It makes the CLI a client with a browser, a listener and an HTTP stack of its own, a second implementation for any
  other client such as the dashboard, and a REST route that accepts a refresh token.
  The daemon runs it, so a dashboard can do exactly what the CLI does.
- **Inject the token as an environment variable the manifest lists.**
  It needs no contract change, but the token is in the guest's environment for the whole call,
  copied into every program the source runs, and a token that expires in the middle of a long discovery cannot be replaced.
  A host function asked per call fixes both, and is gated by a permission the user consented to.
- **A source-specific store and renewal in each adapter.**
  What the issue exists to prevent, and impossible in a sandbox with nowhere to write.
- **Store tokens in the palace database.**
  The palace holds drawers a user may share, back up or point a dashboard at, and the verifier table holds digests only
  so that nothing in the database can authenticate anyone.
  A palace is also per palace, and a sign-in belongs to the installed source on a machine.
- **Only the keyring.**
  A headless server, which is where a long-running memory daemon often lives, has none, and failing there would make
  OAuth sources unusable exactly where they are most useful.
  **Only a file** is not the platform-appropriate store the issue asks for.
- **A client secret in the manifest.**
  A manifest is published, so it could not be secret; a public client with PKCE or the device flow needs none.
  A provider that insists on one cannot be signed in to yet.
- **A callback route on the REST API.**
  It would be a public route, and the authentication layer's only exception is the liveness probe.
- **A private ChatGPT interface in the generic layer.**
  Out of scope on purpose: whatever the ChatGPT or Codex sources do to acquire data is isolated behind their adapters,
  and this layer only provides a token.

## Consequences

- The source contract is `0.3.0`.
  Before `1.0` a minor bump breaks every installed source built for `0.2`:
  they become `unavailable` with a reason that says to rebuild,
  and the shipped `directory`, `pi` and `opencode` sources were rebuilt for it.
- A new dependency, `keyring`, and with it its platform backends; the Linux one is pure Rust, so no C library is linked.
  `reqwest` gains its `form` feature for the token requests.
- Credentials belong to the installed source on a machine and not to a palace,
  so two palaces on one machine that install a source of the same name share its sign-in.
  Removing the source forgets it.
- The browser flow works only with a browser on the daemon's machine; the device flow works anywhere,
  and a source that offers it is preferred.
- A source reaching an HTTPS API from the sandbox needs its own HTTP client, since the host offers raw sockets only and
  no TLS: this ADR gives a source a token, not a way to use it, and the next one to add a host HTTP client would be separate.
- Triggers and scopes still do not reach sources, so a source that needs both to be useful still waits for that work.
- Revoking a credential at the provider is noticed at the next renewal, and not before.
