# MemCastle for Pi

A [Pi extension](https://pi.dev) that decides *when* to call MemCastle.
Everything it asks of MemCastle is an MCP call to the daemon, and it never touches the database, the job code or the
admin endpoint (AGENTS.md invariant 8, enforced by the `integrations-http-only` hook).

**Status: scaffold.**
The connection, memory-mode, discovery and failure foundations are real and tested against a real daemon.
Wake-up is implemented; the other capability modules are registered but empty, and each names the issue that fills it in.

It deliberately does **not** port `pi-palace`'s workaround of routing every write through a daemon queue to avoid
lock contention.
MemCastle's daemon is the only writer and already decides, per operation, whether something is a job (checkpoint) or a
direct call (diary), so the extension calls whichever operation fits and has no routing of its own.

## Use it

The extension needs a running daemon (`memcastle daemon start`) but does not need one to load.
Loading only registers handlers; the connection is opened in the background when a session starts,
so Pi starts normally when the daemon is down and the user is told once how to start it.

```sh
pi -e /path/to/memcastle/integrations/pi/src/extension.ts          # try it for one run
pi install /path/to/memcastle/integrations/pi                       # or install the package
```

Pi supplies `@earendil-works/pi-coding-agent` to extensions, so it is a peer dependency here and is never bundled.

### Configuration

The environment variables are the ones the MemCastle CLI already reads.

| Environment | Default | Meaning |
| --- | --- | --- |
| `MEMCASTLE_MODE` | `full` | `full`, `read-only` or `off`; anything else disables the extension |
| `MEMCASTLE_AUTH_TOKEN` | none | Bearer token, when the daemon requires one |
| `MEMCASTLE_PALACE_PATH` | `$XDG_DATA_HOME/memcastle/default` | Locates the daemon's registry file |
| `MEMCASTLE_BIND`, `MEMCASTLE_PORT` | `127.0.0.1`, `8420` | Used when no registry file names a live daemon |
| `MEMCASTLE_WAKE_UP` | `true` | Whether a session start fetches and injects the wake-up |
| `MEMCASTLE_WAKE_UP_MODE` | `async` | `sync` makes the first response wait for it; `async` never makes a response wait |
| `MEMCASTLE_WAKE_UP_SOURCE` | `project` | The wing to ask about: `user`, `project`, `custom` or `none` |
| `MEMCASTLE_WAKE_UP_WING` | none | The wing for `custom` |

The daemon is found through its registry file, then the configured address, and each candidate is checked with
`GET /api/health` because the file is only a hint.
A mode the extension cannot parse **fails closed**: it tells the user why and does nothing, rather than treating a
typo as `full`.
An `off` session opens no connection at all.

### Wake-up

At the start of a new session the extension asks MemCastle for `memcastle_wake_up` and puts the answer in front of the
model as established context: the agent's latest diary entry and the most recent highlights earlier sessions
checkpointed, quoted verbatim.
The request starts at `session_start`, so the daemon has a head start on the first prompt.

The wake-up settings are named after `pi-palace`'s `injectWakeUp.*`, and `MEMCASTLE_WAKE_UP_MODE` is not
`MEMCASTLE_MODE`: the first is when the briefing arrives, the second is what a session may do to memory.

- **`sync`** makes the first response wait for the wake-up, for at most the call timeout.
  After that wait a slow daemon is not waited for again; a later prompt picks the briefing up when it arrives.
- **`async`**, the default, never makes a response wait.
  The briefing goes in with the first prompt that finds it already in, so on a healthy daemon that is usually the first.
- **`source`** picks the wing, because the daemon only knows a wing name or no wing.
  `user` is the `preferences` wing, which is where checkpoints file a preference by default.
  `project` is the working directory's name made acceptable as a wing name, which is the wing mining a directory creates.
  `custom` is `MEMCASTLE_WAKE_UP_WING`, and falls back to `project` when no wing is set.
  `none` asks about no wing: the diary is skipped, and highlights come from every wing.
  A wing needs a diary written under the same `agentIdentity` (default `pi`) for a diary entry to appear.
  Highlights are not filtered by identity.
- **Injected once.**
  The briefing is a Pi message in the session history, so it stays in context without being sent again.
  A resumed, reloaded or forked session already holds it and does not fetch again.
- **Empty is normal.**
  A new palace, or a wing with nothing in it, injects nothing and says nothing.
- **A down daemon never blocks the session.**
  The user is told once, with the daemon's `help`, and the session carries on without it.
  An `off` session injects nothing.
- A mistyped wake-up value falls back to its default rather than breaking the session, because wake-up only reads.

`/memcastle-wake-up` shows what a session start would inject, for the current directory, without adding it to the
conversation.
It works whatever `MEMCASTLE_WAKE_UP` says, because asking for it is the opt-in.

### Connection lifecycle

Pi runs one agent session at a time, so the extension holds one MCP connection.
It is opened in the background on `session_start`, closed in `session_shutdown`, and replaced if the session is started
again (a reload).
Between the two it is one MCP session, so the memory mode chosen at the start holds for every call.

- **A forgotten session is replaced.**
  The daemon drops a session after five idle minutes or a restart and answers HTTP 404.
  The next call opens a new session, selects the mode again, and is sent once more, so a `read-only` session never
  becomes `full`.
- **An idle session pings.**
  A ping every two minutes keeps the daemon from reaching its idle limit.
  `keepAliveMs` in the settings changes the interval, and `0` turns it off.
- **A daemon that stops answering is reported**, as `daemon_unavailable`, and the next call reconnects, finding the
  daemon again through its registry file in case it moved.
  The failed call is not retried, because it may already have reached the daemon.

## Layout

The file names follow `pi-palace`'s map of the pieces, wired to MemCastle instead of MemPalace.

```text
src/extension.ts              the entry point: handlers only; opens nothing until `session_start`
src/settings.ts               environment to typed settings; the token never reaches a log
src/daemon-client.ts          discovery: registry file, then configured address, verified by /api/health
src/persistent-mcp-client.ts  one MCP session; selects the mode on every (re)connect before anything else
src/mcp-manager.ts            owns that session for the Pi session; reports failures with the daemon's `help`
src/modes.ts                  client labels (full, read-only, off) to wire values (full, read_only, disabled)
src/failures.ts               the five failure classes, each with the daemon's `help`
src/wake-up-core.ts           wake-up without a host: settings, wing, rendering, the in-flight request
src/wake-up.ts                wake-up on session start: fetch at `session_start`, inject at `before_agent_start`
src/wake-up-cli.ts            `/memcastle-wake-up`: show what a session start would inject
src/checkpoint-agent.ts       interval review by the extension's own model (#23), empty
src/checkpoint-tool.ts        the manual checkpoint (#23) and the pre-compaction one (#24), empty
src/daily-mine.ts             background mining on the extension's own schedule (#26), empty
test/                         bun tests against a real `memcastle serve`; they read tests/fixtures/integration/
```

`modes`, `failures`, `settings`, `daemon-client`, `persistent-mcp-client` and `wake-up-core` are a deliberate copy of the small client
the OpenCode integration carries, not a shared package: the two ecosystems differ in how many sessions share a process.
Both suites replay the same fixtures, which is what keeps the copies honest
(see [ADR-022](../../docs/adr/022-integrations-are-bun-packages-tested-against-a-real-daemon.md)).

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
| `session-mode` | Foundation: label translation, mode selected on connect, `off` opens nothing | #27 |
| `wake-up` | Implemented: fetched at session start, injected as a message before the first (`sync`) or first-ready (`async`) response, never blocks on a down daemon | #22, done |
| `recall` | Not yet | #25 |
| `checkpoint` | Not yet | #23 |
| `emergency-checkpoint` | Not yet | #24 |
| `persistent-session` | Implemented: one connection per Pi session, kept alive, replaced with its mode re-selected when the daemon forgets it | #29, done |
| `skills` | Not yet | #25 |
| `background-mining` | Not yet | #26 |
| `failure-reporting` | Foundation: the five classes with `help`, shown as Pi notifications | #30 for the rest |
| `audit-repair` | Not yet | #28 |

### Gaps

None are declared yet.
Only `emergency-checkpoint`, `background-mining` and `audit-repair` may be gaps, each recorded as three lines:
**Missing**, **Fallback** and **Effect**.
Pi has a `session_before_compact` event, so a gap is not expected for `emergency-checkpoint`.
