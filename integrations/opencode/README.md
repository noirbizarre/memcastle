# MemCastle for OpenCode

An [OpenCode plugin](https://opencode.ai/docs/plugins/) that decides *when* to call MemCastle.
Everything it asks of MemCastle is an MCP call to the daemon, and it never touches the database, the job code or the
admin endpoint (AGENTS.md invariant 8, enforced by the `integrations-http-only` hook).

**Status: scaffold.**
The connection, memory-mode, discovery and failure foundations are real and tested against a real daemon.
The lifecycle hooks are wired but empty, and each names the issue that fills it in.
See [`docs/research.md`](docs/research.md) for how OpenCode's mechanisms map to MemCastle operations, and why.

## Use it

The plugin needs a running daemon (`memcastle daemon start`) but does not need one to load:
it connects lazily, so OpenCode starts normally when the daemon is down.

Load it from a project or user plugin directory with a one-line module:

```ts
// .opencode/plugins/memcastle.ts   (or ~/.config/opencode/plugins/memcastle.ts)
export { default } from "/path/to/memcastle/integrations/opencode/src/index.ts"
```

Do **not** also add `mcp.memcastle` to `opencode.json`.
OpenCode shares one MCP connection across every session in a process, while MemCastle's memory mode belongs to a
connection, so the plugin opens its own connection per OpenCode session
(see [Session mode](docs/research.md#session-mode-to-set_mode)).
Enabling both would show the model two copies of every tool.

### Configuration

Plugin options, as `["path-or-package", { ... }]` in `opencode.json`'s `plugin` list, win over the environment.
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

The daemon is found through its registry file, then the configured address, and each candidate is checked with
`GET /api/health` because the file is only a hint.
A mode the plugin cannot parse **fails closed**: it logs why and does nothing, rather than treating a typo as `full`.

## Layout

```text
src/index.ts      the plugin: hook wiring only; the one default export OpenCode loads
src/settings.ts   options and environment to typed settings; the token never reaches a log
src/daemon.ts     discovery: registry file, then configured address, verified by /api/health
src/session.ts    one MCP session; selects the mode on every (re)connect before anything else
src/registry.ts   one session per OpenCode sessionID, connected lazily
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
| `wake-up` | Not yet | #33 |
| `recall` | Not yet | #36 |
| `checkpoint` | Not yet | #34 |
| `emergency-checkpoint` | Not yet; planned on `experimental.session.compacting` | #34 |
| `persistent-session` | Implemented: one connection per session, mode re-selected on reconnect | done |
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
