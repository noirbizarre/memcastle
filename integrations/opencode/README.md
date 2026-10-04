# MemCastle for OpenCode

An [OpenCode plugin](https://opencode.ai/docs/plugins/) that decides *when* to call MemCastle,
for both OpenCode 1 and OpenCode 2 from one package.
Everything it asks of MemCastle is an MCP call to the daemon, and it never touches the database, the job code or the
admin endpoint (AGENTS.md invariant 8, enforced by the `integrations-http-only` hook).

**Status: scaffold.**
The connection, memory-mode, discovery and failure foundations are real and tested against a real daemon.
Wake-up is implemented; the other lifecycle hooks are wired but empty, and each names the issue that fills it in.
See [`docs/research.md`](docs/research.md) for how OpenCode's mechanisms map to MemCastle operations, and why.

## Use it

The plugin needs a running daemon (`memcastle daemon start`) but does not need one to load:
it connects lazily, so OpenCode starts normally when the daemon is down.

Load it from a project or user plugin directory with a one-line module:

```ts
// .opencode/plugins/memcastle.ts   (or ~/.config/opencode/plugins/memcastle.ts)
export { default } from "/path/to/memcastle/integrations/opencode/src/index.ts"
```

The same module works under both majors, because its default export carries both entrypoints:
OpenCode 1 calls `server()` and OpenCode 2 calls `setup()`.

| | OpenCode 1 | OpenCode 2 |
| --- | --- | --- |
| Supported from | 1.18.29, the first release that accepts a `{ id, server }` object | the current 2.x line, typed against `@opencode/plugin` 2.0.22 |
| Config key | `plugin` | `plugins` |
| Local directory | `.opencode/plugins/` (or `plugin/`) | `.opencode/plugins/` |
| Entry | `server(input, options)` returning hooks | `setup(ctx)` registering hooks, returning a cleanup |
| Log | OpenCode's log, through `client.app.log` | the console, because the V2 context has no log API |

The two APIs are separate and nothing translates between them, so each has its own adapter over the shared behaviour
(see [Layout](#layout)).
The plugin packages are imported as types only, so loading it needs neither of them installed.
OpenCode releases older than 1.18.29 are not supported.

Do **not** also add `mcp.memcastle` to `opencode.json`.
OpenCode shares one MCP connection across every session in a process, while MemCastle's memory mode belongs to a
connection, so the plugin opens its own connection per OpenCode session
(see [Session mode](docs/research.md#session-mode-to-set_mode)).
Enabling both would show the model two copies of every tool.

### Configuration

Plugin options win over the environment.
OpenCode 1 takes them as `["path-or-package", { ... }]` in `opencode.json`'s `plugin` list,
and OpenCode 2 as `{ "package": "path-or-package", "options": { ... } }` in its `plugins` list.
The environment variables are the ones the MemCastle CLI already reads.

| Option | Environment | Default | Meaning |
| --- | --- | --- | --- |
| `mode` | `MEMCASTLE_MODE` | `full` | `full`, `read-only` or `off`; anything else disables the plugin |
| `token` | `MEMCASTLE_AUTH_TOKEN` | none | Bearer token, when the daemon requires one |
| `palacePath` | `MEMCASTLE_PALACE_PATH` | `$XDG_DATA_HOME/memcastle/default` | Locates the daemon's registry file |
| `bind`, `port` | `MEMCASTLE_BIND`, `MEMCASTLE_PORT` | `127.0.0.1`, `8420` | Used when no registry file names a live daemon |
| `endpoint` | none | none | An explicit `http://host:port`, which skips discovery |
| `agentIdentity` | none | `opencode` | Stored with diary and wake-up calls |
| `timeoutMs` | none | `5000` | How long to wait for the daemon |
| `keepAliveMs` | none | `120000` | Ping interval that keeps an idle connection open; `0` turns it off |
| `wakeUp.enabled` | `MEMCASTLE_WAKE_UP` | `true` | Whether a session start fetches and injects the wake-up |
| `wakeUp.mode` | `MEMCASTLE_WAKE_UP_MODE` | `async` | `sync` makes the first response wait for it; `async` never makes a response wait |
| `wakeUp.source` | `MEMCASTLE_WAKE_UP_SOURCE` | `project` | The wing to ask about: `user`, `project`, `custom` or `none` |
| `wakeUp.wing` | `MEMCASTLE_WAKE_UP_WING` | none | The wing for `custom` |

The daemon is found through its registry file, then the configured address, and each candidate is checked with
`GET /api/health` because the file is only a hint.
A mode the plugin cannot parse **fails closed**: it logs why and does nothing, rather than treating a typo as `full`.

### Wake-up

At the start of a session the plugin asks MemCastle for `memcastle_wake_up` and adds the answer to the system prompt as
established context: the agent's latest diary entry and the most recent highlights earlier sessions checkpointed,
quoted verbatim.
The request starts on `session.created`, which arrives before the first message, so the daemon has a head start.
The settings have the same shape as the [Pi extension's](../pi/README.md#wake-up), which is what keeps the two
agents behaving alike, and `wakeUp.mode` is not `mode`: the first is when the briefing arrives, the second is what a
session may do to memory.

- **`sync`** makes the first response wait for the wake-up, for at most `timeoutMs`.
  After that wait a slow daemon is not waited for again; a later response picks the briefing up when it arrives.
- **`async`**, the default, never makes a response wait.
  The briefing goes into the first request that finds it already in, so on a healthy daemon that is usually the first.
- **`source`** picks the wing, because the daemon only knows a wing name or no wing.
  `user` is the `preferences` wing, which is where checkpoints file a preference by default.
  `project` is the session's directory name made acceptable as a wing name, which is the wing mining a directory creates.
  `custom` is `wakeUp.wing`, and falls back to `project` when no wing is set.
  `none` asks about no wing: the diary is skipped, and highlights come from every wing.
  A wing needs a diary written under the same `agentIdentity` (default `opencode`) for a diary entry to appear.
  Highlights are not filtered by identity.
- **Added to every request.**
  OpenCode rebuilds the system prompt for each model request, so unlike a message in a transcript the briefing is added
  again each time.
  What is fetched once per session is the answer, so the daemon is not asked again.
  OpenCode 1 also runs the system transform for the title model, which therefore sees the briefing too.
- **Subagents are left out.**
  A session with a parent gets no briefing and no connection of its own, because the parent already has it.
- **A resumed session wakes up too.**
  It never fires `session.created`, so its first request starts the fetch, in the directory OpenCode was started in.
- **Empty is normal.**
  A new palace, or a wing with nothing in it, injects nothing and says nothing.
- **A down daemon never blocks the session.**
  The failure is logged once as a warning, with the daemon's `help`, and the session carries on without it.
  An `off` session registers no hooks, so nothing is injected.
- A mistyped wake-up value falls back to its default rather than breaking the session, because wake-up only reads.

### Connection lifecycle

This differs from the [Pi adapter](../pi/README.md#connection-lifecycle), which holds a single connection.
One OpenCode process hosts many sessions, and OpenCode's own MCP client shares one connection across all of them.
MemCastle's memory mode belongs to a connection, so a shared one could not give two sessions different modes
(see [Session mode](docs/research.md#session-mode-to-set_mode)).
The plugin therefore owns one connection per OpenCode `sessionID`, and the rest follows from that.

- **Lazy.**
  A connection is opened on a session's first MemCastle call and never at startup, so a session that does not use
  MemCastle costs nothing, and a missing daemon does not slow OpenCode down.
- **Closed with the session.**
  `session.deleted` closes that session's connection, and `dispose` (V1) or the cleanup returned by `setup` (V2)
  closes all of them.
  Both tell the daemon to forget the session.
- **Independent.**
  Each session is kept alive, and replaced if the daemon forgets it, on its own.
  Losing one never disturbs another, and each keeps the mode it was opened with.
- The recovery and keep-alive rules are the Pi adapter's: a 404 means a lost session, the call is sent once more on a
  new session with the mode re-selected, and a failure without a 404 is reported and the next call reconnects.

## Layout

```text
src/index.ts      the one default export OpenCode loads: `{ id, server, setup }`, checked against both module types
src/core.ts       what MemCastle does at each lifecycle point, independent of the OpenCode major
src/v1.ts         OpenCode 1 adapter: `server()` returning hooks by string key
src/v2.ts         OpenCode 2 adapter: `setup(ctx)` registering hooks and an event subscription
src/settings.ts   options and environment to typed settings; the token never reaches a log
src/daemon.ts     discovery: registry file, then configured address, verified by /api/health
src/session.ts    one MCP session; selects the mode on every (re)connect before anything else
src/registry.ts   one session per OpenCode sessionID, connected lazily
src/wake-up-core.ts  wake-up without a host: settings, wing, rendering, the in-flight request (the same file as Pi's)
src/modes.ts      client labels (full, read-only, off) to wire values (full, read_only, disabled)
src/failures.ts   the five failure classes, each with the daemon's `help`
test/             bun tests against a real `memcastle serve`; they read tests/fixtures/integration/ directly
docs/research.md  the OpenCode mechanism map this package follows
```

## Develop

```sh
cargo build                  # the tests start a real daemon from target/debug/memcastle (or $MEMCASTLE_BIN)
bun install --frozen-lockfile
bun run typecheck
bun run test
```

`mise run integrations:check` does all of it for every package under `integrations/`.
The tests never skip when the binary is missing, because a suite that passes without a daemon proves nothing.

## Conformance matrix

The contract is [`docs/integration-contract.md`](../../docs/integration-contract.md).
**Foundation** means the building block exists and is tested, but the lifecycle behaviour is not wired yet.

| Capability | Status | Where it lands |
| --- | --- | --- |
| `session-mode` | Foundation: label translation, per-session connection, mode selected on connect | #35 |
| `wake-up` | Implemented: fetched on `session.created`, added to the system prompt of the first (`sync`) or first-ready (`async`) request, never blocks on a down daemon | #33, done |
| `recall` | Not yet | #36 |
| `checkpoint` | Not yet | #34 |
| `emergency-checkpoint` | Not yet; planned on `experimental.session.compacting` (V1) and the `compaction` session hook (V2) | #34 |
| `persistent-session` | Implemented: one connection per OpenCode session, kept alive, replaced with its mode re-selected when the daemon forgets it | #124, done |
| `skills` | Not yet | #36 |
| `background-mining` | Not yet | no issue yet |
| `failure-reporting` | Foundation: the five classes with `help`; user-facing toasts are not wired | later |
| `audit-repair` | Not yet | no issue yet |

### Gaps

None are declared yet.
Only `emergency-checkpoint`, `background-mining` and `audit-repair` may be gaps, each recorded as three lines:
**Missing**, **Fallback** and **Effect**.
The [research](docs/research.md#gaps-and-risks) lists where OpenCode is likely to need one, so that a gap is documented
when it is found and not discovered by a user.
