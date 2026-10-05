# MemCastle web dashboard

A Vue 3 and [OpenVue](https://openvue.dev) application that is a client of the daemon's REST API.
The daemon serves its build under `/ui` when `web.enable` is set; see [docs/web.md](../docs/web.md) and
[ADR-035](../docs/adr/035-web-dashboard.md).

```sh
mise run web:build   # build into web/dist, then `memcastle serve --assets-dir "$PWD"` with web.enable
mise run web:dev     # dev server with hot reload, proxying /api to the running daemon (MEMCASTLE_URL to override)
mise run web:check   # typecheck, test, build, package, and serve from a real daemon
```

Layout: `src/api` is the typed client (the only place that calls `fetch`), `src/session.ts` the token and memory mode,
`src/views` one page per route, `dev/` the dev server's way of finding the daemon, `test/` the unit tests and `test/daemon`
the tests against a real `memcastle serve`.

It must reach MemCastle only over HTTP: no database, store or job code (the `web-http-only` hook).
