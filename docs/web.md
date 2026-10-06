# Web dashboard

MemCastle can serve a web dashboard for inspecting and managing a daemon.
It is a client of the [REST API](mcp-and-api.md#rest-api), as the CLI is: it reads and writes nothing but through the
routes you could call yourself, and it respects the [memory modes](memory-modes.md).
It is off by default.

The decision, and what was rejected, is in [ADR-035](adr/035-web-dashboard.md).

## Turning it on

```toml
[web]
enable = true
```

Or `MEMCASTLE_WEB_ENABLE=true`, then restart the daemon (`memcastle daemon restart`).
Open `http://127.0.0.1:8420/ui/` (the daemon's own address, see
[Configuration](configuration.md#the-listener-address-and-port)).

A release carries the dashboard in `share/memcastle/web/`, so a package or an unpacked release tarball needs nothing else.
A standalone binary does not carry it: with `web.enable` it still starts, logs a warning, and answers `/ui` with a `503`
page that says how to install the files.
The REST API and MCP are unaffected.

## What it shows

| Page | What you can do |
|---|---|
| Overview | The palace and daemon at a glance: health, address, pid, start time, datastore and migration state, drawer count, queued, running and paused jobs, and how the newest 200 jobs ended. |
| Palace | Browse wings, rooms and drawers, open a drawer to read it verbatim with its source, validity, history and likely duplicates. |
| Search | The retrieval an agent gets, with its options (wing, room, ranking, `as_of`, historical memory, graph expansion), each hit's score and signals. |
| Graph | The knowledge graph: an overview, or the neighbourhood of one entity up to three hops, with each fact's provenance and the drawers that mention an entity. |
| Diary | Read an agent's diary by identity and wing, and write an entry. |
| Jobs | Active jobs and history, filtered by state and kind, with progress, parameters and results; pause, resume, cancel and retry. |
| Launch | Submit a mine job (a directory or an installed source, with the options that source declares, such as `since`), an extract job or an embed job. |
| Maintenance | Run an audit or a repair (a dry run first), and see the last report of each. |
| Settings | The configuration in effect, from `GET /api/config`: no secret, and nothing read from the files. |

The dashboard does not poll, and it updates by itself.
The daemon pushes small "something changed" notices over [`GET /api/events`](mcp-and-api.md#the-event-stream),
and the Overview, Palace, Jobs and Maintenance pages read again, quietly, a moment after a change they show:
a job's progress bar moves and a new note appears without a button press.
The sidebar says whether updates are **Live**, **Connecting** or **Manual**.
The notices carry identifiers and never content,
so what a page shows still comes through the routes that apply the memory mode,
and the stream is closed and opened again when you change the mode, sign out or the token is refused.
It reconnects by itself if the connection drops, and reads again once it is back.

Each page that shows something that changes also keeps its **Refresh** button, with the time of the last answer beside
it, and reads again when you press it (and after an action of yours, such as submitting or controlling a job).
It is the fallback when there is no stream (it was refused, or a proxy in front of the daemon buffers it),
and the way to see what another daemon wrote to a [shared remote palace](adr/006-job-leases.md),
whose changes this daemon cannot announce.
A failed read keeps the last good data on screen and shows the error with a retry.
Behind a reverse proxy, turn response buffering off for `/api/events` or the updates arrive late.

It does not shut the daemon down, install sources or manage tokens.
Those stay in the CLI and the REST API, deliberately: see [Authentication](authentication.md) and
[Mining sources](mining-sources.md).

## Signing in

With [authentication](authentication.md) off, there is nothing to sign in to.
With it on, the page shows a login that works like the [database console's](database-access.md):
the user is `memcastle` and the password is the daemon's token (`memcastle auth generate`, or `MEMCASTLE_AUTH_TOKEN`).
Nothing is stored in an account: the token is sent as `Authorization: Bearer` with every request
and checked by the same layer as the rest of the API.
It is kept in the browser tab's `sessionStorage`, so closing the tab signs out,
and "Sign out" in the sidebar forgets it at once.
If you rotate or revoke the token, the next request is refused and the page returns to the login.

The page's own files (`/ui/...`) need no token, because a browser cannot send one when it loads a page.
They hold no data.
Every call the page makes to `/api` does need it.

The token is sent in cleartext over plain HTTP, as it is for the API.
A dashboard reachable beyond a trusted network belongs behind a TLS-terminating proxy
(see [Exposing the daemon](authentication.md#exposing-the-daemon)).

## The memory mode

The sidebar chooses the mode the dashboard sends with each request: **full access** or **read only**.
Read only is the [`read_only` mode](memory-modes.md): the daemon refuses what writes memory
(mining, embedding, extraction, an applied repair, the diary),
and the page disables those controls and shows the refusal if one is attempted.
An audit and a repair dry run still work, because they only read.
Pausing, resuming and cancelling a job are daemon operations and are not gated by the mode.
The choice lasts for the tab.

## Developing the dashboard

The source is `web/` (Vue 3 and [OpenVue](https://openvue.dev), with Vite and TypeScript), its own bun package.
It needs bun and node.

```sh
mise run web:build                      # web/ -> web/dist
memcastle serve --assets-dir "$PWD"     # with web.enable, from a checkout
```

In this repository `mise.toml` already sets `MEMCASTLE_ASSETS_DIR` to the checkout and `MEMCASTLE_WEB_ENABLE` to `true`,
so with mise active `mise cli serve` is enough, and serves the dashboard at `http://127.0.0.1:8420/ui/` once it is built
(see [Development](development.md#running-the-daemon-locally)).

A checkout has the same layout as a package (`web/dist/index.html`), so `--assets-dir <checkout>` is the whole development
setup, and the daemon finds the files by the same lookup it uses for an installed package.
For hot reload, run the daemon, then:

```sh
mise run web:dev
```

The dev server serves the page at `http://localhost:5173/ui/` and proxies `/api` to the daemon, which it finds through the
registry file of the default palace; set `MEMCASTLE_URL` (for example `http://127.0.0.1:8420`) to point it elsewhere.
With authentication on you sign in as usual.

`mise run web:check` typechecks, tests and builds it, checks the package tree that a release would ship,
and serves it from a real daemon, from an installed prefix and from the checkout.
The tests are in `web/test/`: the client, the login, the route guard and the loader run under vitest with no daemon,
and `web/test/daemon` runs the same client and login against a real `memcastle serve`.
The brand is `docs/images/icon.svg`, imported from there (alias `@brand`), so a new logo changes the documentation and the
dashboard together.
`web/` must stay a client of HTTP: the `web-http-only` hook fails on any path to the database, the store or the jobs.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `/ui` answers `401` or `404` | `web.enable` is not set, or the daemon was not restarted after setting it. |
| `/ui/` answers a page saying the files are missing | The daemon found no `web/dist/index.html` in its [assets](configuration.md#runtime-assets). `mise run web:build` and `--assets-dir <checkout>`, or install a package. `GET /api/config` shows where the assets came from. |
| The login refuses a token you are sure of | The user must be `memcastle`, and the password the token itself, with no "Bearer" in front. A token that was revoked or replaced stops working at the next request. |
| The dev server cannot reach the daemon | `MEMCASTLE_URL`, or start the daemon before the dev server so its registry file exists. |
