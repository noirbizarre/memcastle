# ADR-035: The web UI is an opt-in Vue client in `web/`, served under `/ui` from the assets root, with a public shell

## Status

Accepted.
Amends [ADR-013](013-release-packaging-and-asset-resolution.md) (the assets root gains the consumer it was built for, and
its first embedded entry),
[ADR-034](034-agent-integration-distribution.md) (`web/` is a fourth tree under the one root, and it is not an agent
integration),
[ADR-014](014-optional-token-authentication.md) (a second public path, narrowly) and
[ADR-015](015-database-admin-endpoint.md) (the dashboard signs in the way the database console does).

## Context

Issue #63 asks for a web interface to manage and inspect a MemCastle instance: the palace overview and health,
wings, rooms and drawers, search, the knowledge graph, the diary, jobs, mining and maintenance, and basic configuration.
It must be a client of the same services as the CLI and the integrations, must follow the asset distribution of
sources and integrations (a configurable data root, packaged assets, a worktree in development),
must respect memory modes and privacy rules, and must not touch SurrealDB.

Five decisions follow, and each has an alternative that is easier for one of the forces and worse for the others.

- Where the files live, and what they are called.
- Whether the daemon serves them at all unless asked.
- How a browser authenticates when the only credential the daemon accepts is an `Authorization` header,
  which a browser cannot send when it navigates.
- What the dashboard may do, and what API it needs.
- How it is built and packaged without a second release process.

## Decision

- **The canonical name is `web`, and the layout is the same in a checkout and in a package.**
  The source is `web/` (a bun package), the build is `web/dist/`, and a package installs it at
  `<prefix>/share/memcastle/web/dist/`.
  The daemon asks `Assets::find("web/dist/<path>")`, so development is `--assets-dir <checkout>` after
  `mise run web:build`, exactly as for integrations (ADR-034), and a package is found through the executable's prefix
  with no flag.
  There is one lookup, one data root and no `web_dir` setting.
  `web/dist/` is ignored by git and produced by `packaging/web/build.sh`, the script the release and `mise run web:package`
  both run.
- **It is opt-in: `web.enable` (`MEMCASTLE_WEB_ENABLE`), false by default.**
  A daemon that was not asked for a UI answers no `/ui`: the routes are not merged, so the authentication layer
  answers an anonymous request with 401 and an authenticated one with 404, as for any path that does not exist.
  There is no CLI flag: `daemon start` already passes the environment and the file on.
- **Enabled but not installed is a 503 with the remedy, never a failed start.**
  `Assets::EMBEDDED` gains one entry, `web/dist/index.html`: a small page saying the dashboard was enabled and its files
  were not found, with the three ways to fix that.
  A built `web/dist/index.html` in the chosen directory outranks it (the rule of ADR-013), so the page exists exactly when
  the files do not.
  The REST API and MCP are unaffected, and the startup log warns.
  Embedding the dashboard itself stays rejected (ADR-013): it would bloat a headless binary and tie every UI fix to a
  daemon release.
- **The static shell is public, and nothing else is.**
  With `web.enable`, `GET` and `HEAD` of `/ui` and anything under `/ui/` pass the authentication layer without a token.
  The files hold no data and are the same for everyone;
  without this a browser could never load the page that asks for the token.
  The layer still wraps the merged router, `is_public` is still the one place that names an exception,
  `/uix` and every `/api` and `/mcp` route are guarded as before,
  and a daemon with the dashboard off keeps `/ui` guarded.
  The static handler adds a restrictive `Content-Security-Policy` (own origin only, no framing), `nosniff` and a
  `no-referrer` policy, caches only the content-hashed files under `assets/`, and never serves a map.
- **The login is the database console's sign-in, over REST.**
  When the daemon requires a token, the SPA shows a login page with the user `memcastle` and the token as the password,
  the same identifier and the same credential as `signin` in ADR-015: no account, no new secret, no cookie and no new route.
  The token is checked by sending `Authorization: Bearer` to `GET /api/status`, which `AppServices::authenticate`
  answers like every request, so a token that is rotated or revoked stops working on the next call
  and the page returns to the login.
  A refusal says "The user or password was not accepted" and never which half was wrong or what was typed.
  The token lives in `sessionStorage` (closing the tab signs out), never in the URL or `localStorage`.
  The database console's five-refusals limit is per WebSocket connection and has no REST counterpart, so there is none.
  Whether to show the login is learned from the first `GET /api/status`: 200 means no token is needed, 401 means one is.
- **The dashboard is a client and carries a memory mode.**
  Every request carries `X-MemCastle-Mode` (`full` or `read_only`, chosen in the sidebar),
  and a refusal from the daemon is shown, not swallowed.
  The interface hides or disables what the daemon would refuse (applying a repair, mining, embedding, extracting, writing),
  but the daemon is what refuses.
  The first version submits jobs, controls them, writes the diary and notes, and reads everything else.
  It has no shutdown, no source installation and no token management: those stay CLI and REST, as invariants 6 and 10 require.
- **Three read-only routes make it possible, and nothing else was added.**
  `GET /api/jobs` takes `kind` and `limit` (either makes the answer a bounded page, newest first, default 50, at most 200;
  without both it is unchanged).
  `GET /api/graph` answers entities and the open facts between them in one request, for one entity's neighbourhood
  (depth 1 to 3) or an overview, capped and saying when it stopped.
  `GET /api/config` answers the configuration in effect from an explicit list of fields, never a serialisation of `Config`,
  so a secret or an endpoint URL cannot appear by being added to the file format.
  The first two are reads under ADR-007; the third is daemon information like `status` and is not gated by the mode.
- **The code is Vue 3 with OpenVue, and `web/` is its own package.**
  OpenVue is the MIT continuation of PrimeVue; it is the component library, with its Aura theme and a light, dark or system
  mode.
  `web/` has its own `package.json` and `bun.lock` and does not import from an integration:
  it installs and builds alone, as ADR-022 decided for the integrations.
  The few lines that find a daemon for the dev server are copied from the integrations' client, not imported.
  Hash routing means the daemon needs no rewrite rule for a deep link.
- **Pages are read on demand, by a Refresh button, and neither poll nor push.**
  Each page that shows changing data reads it when it opens, when the button is pressed and after the user's own actions,
  and says when it last succeeded.
  A timer would be a request per page per interval whether or not anything changed.
  A WebSocket or server-sent stream would need an event bus out of the scheduler and the writers, a new route the
  authentication layer must admit (a browser WebSocket cannot send a header) and its own tests;
  that is a larger decision than the dashboard, and is left to issue #215.
- **A guard holds `web/` to HTTP, as one holds `integrations/`.**
  The `web-http-only` hook fails on `surrealdb`, `surrealkv`, `SurrealStore`, a `store` or `jobs` path or `/api/db`
  under `web/` (Markdown, `node_modules` and `dist` excepted): invariant 11.
- **Tests cover discovery, packaging, integration and the missing build.**
  `src/assets` tests prove a packaged layout and a worktree resolve `web/dist/index.html` through the same lookup and that
  without it the embedded page stands in.
  `tests/in_process/web.rs` and `auth.rs` prove the opt-in, the headers, the traversal refusals, the public shell, and that
  every `/api` route stays guarded.
  `tests/web_bundle.rs` runs the release's script, checks what the package holds,
  and serves it from an installed prefix and from the checkout.
  `web/test` holds the client, the login and the route guard (vitest) and runs the same client against a real daemon (bun).
  `mise run web:check` runs all of it and is in `check`; CI has a job for it and the packaging job lists the file.

## Alternatives rejected

- **`ui` or `dashboard` as the directory name.**
  The issue allows any of the three and asks for one.
  `web` says what it is to someone who has never seen the product, and it is the name of the setting.
- **A separate `web.dir` setting.**
  `assets.dir` already means where the files that ship with MemCastle are (ADR-034 rejected the same for integrations).
- **Embedding the built dashboard in the binary.**
  Rejected in ADR-013 and not reopened: the fallback page is a few hundred bytes.
- **Always serving the dashboard.**
  A daemon on a server that does not want a UI would answer one, and the one path that bypasses authentication would exist
  on every installation.
- **Keeping `/ui` guarded.**
  The dashboard would work only with authentication off, or behind a proxy that injects a header: no one who turned on
  authentication could use it.
- **A cookie or a token in the query string.**
  A cookie needs a login route that sets it, which is a new credential flow and a CSRF surface on a daemon that is otherwise
  stateless; a token in a URL lands in logs, history and `Referer`.
- **A login route that exchanges the token for a session.**
  It would give the database console's identifier a second meaning,
  and a revoked token would have to be tracked in two places.
- **Serving files with `tower-http`'s `ServeDir`.**
  It would not go through `Assets::find`, so the embedded page, the root-escape refusal and the one-lookup rule
  would each have to be rebuilt around it.
  The handler is a screenful.
- **Importing the integrations' daemon client.**
  It is MCP-oriented and brings `smol-toml` and the MCP SDK, and the web package would no longer install on its own.
- **Fanning out over `/api/entities` for the graph.**
  One request per entity per view is what a bounded server-side answer exists to avoid.

## Consequences

- A fourth tree, `web/`, sits under the assets root beside `sources/`, `integrations/` and `skills/`.
  A release gains `memcastle_<version>_web.tar.gz`, deb and rpm carry `/usr/share/memcastle/web`,
  and the AUR and Homebrew recipes fetch the new asset.
- Building a release needs node as well as bun, and the CI packaging job lists `web/dist/index.html`.
- There are two public paths on a daemon with authentication on and the dashboard enabled, and they are tested as such:
  the liveness probe and the static shell.
- `/api/config` and `/api/graph` are part of the REST contract and documented in `docs/mcp-and-api.md`;
  there is deliberately no MCP counterpart, since an agent has the tools it needs and a dashboard's views are for people.
- A token typed into the login page travels in cleartext over plain HTTP, as it does for the REST API and the database console:
  `web.enable` on a non-loopback bind belongs behind TLS (docs/authentication.md).
- Source maps are not built, so a browser's developer tools show minified code of a release.
