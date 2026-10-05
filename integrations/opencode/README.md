# MemCastle for OpenCode

An [OpenCode plugin](https://opencode.ai/docs/plugins/) that decides *when* to call MemCastle,
for both OpenCode 1 and OpenCode 2 from one package.
Everything it asks of MemCastle is an MCP call to the daemon, and it never touches the database, the job code or the
admin endpoint (AGENTS.md invariant 8, enforced by the `integrations-http-only` hook).

**Status: scaffold.**
The connection, memory-mode, discovery and failure foundations are real and tested against a real daemon.
Wake-up, the shared skills and checkpointing (interval, manual and emergency) are implemented; the other lifecycle
hooks are wired but empty, and each names the issue that fills it in.
See [`docs/research.md`](docs/research.md) for how OpenCode's mechanisms map to MemCastle operations, and why.

## Use it

The plugin needs a running daemon (`memcastle daemon start`) but does not need one to load:
it connects lazily, so OpenCode starts normally when the daemon is down.

```sh
memcastle integration install opencode    # copies the bundled plugin and writes the one-line module below
```

See [OpenCode integration](../../docs/integrations-opencode.md) for what it changes and how to update and remove it.
To work on the plugin itself, load it from the sources with a one-line module in a project or user plugin directory:

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
The plugin packages are imported as types only, except for one case: OpenCode 1's `tool` helper, which the checkpoint tool
needs and which is imported when the plugin loads.
A host that does not ship `@opencode-ai/plugin` still loads the plugin, loses only that tool, and the log says so.
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
The variables below share their names with the MemCastle CLI's where it has one, but not always their values:
`MEMCASTLE_MODE` takes this integration's labels (`full`, `read-only`, `off`), whereas the CLI's `--mode` and `MEMCASTLE_MODE`
take the daemon's (`full`, `read_only`, `disabled`).
A shell that exports one set therefore breaks the other tool, so set the integration's mode in its own settings when you
also use the CLI.
The wake-up and checkpoint variables, and `MEMCASTLE_FORCE_MEMORY_RECALL`, belong to this integration alone: the CLI does not read them.

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
| none | `MEMCASTLE_WING`, `MEMCASTLE_ROOM` | none | The project's wing and room, overriding `.config/memcastle.toml` (see [Project context](#project-context)) |
| `forceMemoryRecall.level` | `MEMCASTLE_FORCE_MEMORY_RECALL` | `sometimes` | `off`, `sometimes` or `always`: how hard to push the model to search first |
| `checkpoint.enabled` | `MEMCASTLE_CHECKPOINT` | `true` | Whether the interval review runs; the tool, the command and the emergency checkpoint work either way |
| `checkpoint.interval` | `MEMCASTLE_CHECKPOINT_INTERVAL` | `10` | How many exchanges (idle events) separate two interval reviews |
| `checkpoint.mode` | `MEMCASTLE_CHECKPOINT_MODE` | `silent` | `silent` reviews in the background; `blocking` makes the idle hook wait and logs the result |
| `checkpoint.model` | `MEMCASTLE_CHECKPOINT_MODEL` | none | `provider/id` of the model that reviews the conversation; none means the session's own |

The daemon is found through its registry file, then the configured address, and each candidate is checked with
`GET /api/health` because the file is only a hint.
A mode the plugin cannot parse **fails closed**: it logs why and does nothing, rather than treating a typo as `full`.

### Project context

A project declares its memory scope in `.config/memcastle.toml`, and `MEMCASTLE_WING` and `MEMCASTLE_ROOM` override it.
The plugin reads both itself, from each session's own directory, so sessions of one process in different projects keep their own scope, and passes the result to MemCastle as ordinary `wing` and `room` arguments:
the daemon is not asked to resolve anything.
The file, the discovery rules and the precedence are in [Project configuration](../../docs/project-config.md).

- **Wake-up** asks about the project's wing under the default `project` source; `user`, `custom` and `none` still win.
- **Checkpoints** file `project` and `diary` items with no wing of their own under the project's wing,
  and never move a `preference` or a `general` item.
- **Search** is preceded by one line telling the model which wing and room to pass.
- A broken file or variable is reported once, and the session carries on without a project scope.
- An `off` session reads no project file.

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
  The failure is reported once, as a warning toast and log line with the daemon's `help`, and the session carries on
  without it.
  An `off` session registers no hooks, so nothing is injected.
- A mistyped wake-up value falls back to its default rather than breaking the session, because wake-up only reads.

### Shared skills

The plugin reuses the repository's [`skills/`](../../skills/README.md) and keeps no copy of them.
Two things happen, and both read the files where they are:

- **Native discovery.**
  The plugin makes `skills/` visible to OpenCode's own `skill` tool, so `search-before-answer` and
  `checkpoint-instructions` are listed and loaded on demand like any other skill.
  OpenCode 1 gets it from the `config` hook, which adds the directory to `skills.paths`.
  OpenCode 2 gets it from `ctx.skill.transform`, which adds each skill with its path and body.
  A skill of the same name the user already installed, say under `.agents/skills`, is never shadowed.
  The whole directory is exposed, which is the same as copying it into a client location as
  [the skills page](../../docs/skills.md) describes, so `wake-up`, `diary` and `memcastle-setup` are listed too.
- **A reminder on every request.**
  A skill that is only listed is loaded when the model decides to, which is weaker than a habit.
  So the body of `search-before-answer` is also added to the system prompt of every request, as Pi does, at the level
  `forceMemoryRecall.level` says.
  `sometimes` adds the skill as written, `always` adds one line saying to search before every answer, and `off` adds
  nothing.
  The settings have the same names and meaning as the [Pi extension's](../pi/README.md#search-before-answering), so the
  two agents use the same MemCastle operations (`memcastle_search`, then `memcastle_recall`) with the same semantics.
  The reminder does not depend on the wake-up, so a disabled or failing wake-up never removes it.
  A subagent's request is left alone, like its wake-up.
  OpenCode 1 also runs the system transform for the title model, which therefore sees the reminder too.
- An `off` session registers no hooks, so nothing is injected and nothing is listed.
- An unreadable skill file is logged once as a warning and requests carry on without the reminder.

### Checkpointing

Three things save a conversation to MemCastle, and all of them classify client-side: the daemon stores what it is handed
and decides nothing.
The reviewing model is given the shared [`checkpoint-instructions`](../../skills/checkpoint-instructions/SKILL.md) skill,
read from `skills/` and never copied, and replies with items each tagged `preference`, `project`, `diary` or `general`.
The plugin validates the reply, stamps each item with `source.agent` (the `agentIdentity`, default `opencode`), and
submits it with `memcastle_checkpoint`.
The code is [the Pi extension's](../pi/README.md#checkpointing) `checkpoint-core.ts`, the same file, so the two agents
agree on what a checkpoint is and which failures they report.

- **Interval.**
  OpenCode has no timer or per-turn hook, so the plugin counts `session.idle` events, which fire when the agent finishes
  a run, and reviews every `checkpoint.interval` of them.
  This is **partial**: a long single run is one exchange, so a checkpoint can lag it.
  Each review reads only what the last one did not, and a review that fails before the daemon has the items is retried
  over the same exchanges.
- **Manual.**
  The plugin registers the `memcastle_checkpoint` tool, which the shared skills name, and `/memcastle-checkpoint [hint]`.
  The tool with no `payload` reviews the session's conversation itself, using `note` as the user's own words; with a
  `payload` of items the model classified, it validates them and saves exactly those.
  OpenCode 1 commands are prompt templates, so the command asks the model to call the tool and repeat its answer;
  OpenCode 2's command runs the review itself and shows the answer in the session.
  A command you defined with the same name is not replaced.
  The tool is why the plugin has to be loaded without `mcp.memcastle`: the model must see one `memcastle_checkpoint`.
- **Emergency.**
  Before OpenCode summarises a session, the plugin reviews what has not been kept and submits it with `emergency: true`,
  which is a Critical job that is claimed before anything else queued.
  It does not wait for the job, and it waits for the review at most 30 seconds, so a slow model never holds up the
  compaction, and a failure is logged and never stops it.
  OpenCode 1's hook, `experimental.session.compacting`, is experimental and can change without notice:
  if it does, run `memcastle checkpoint --emergency` by hand, and the context is otherwise lost without a last checkpoint.
  OpenCode 2's `compaction` hook is not experimental, and hands the plugin the messages, so nothing is read back.
  `checkpoint.enabled` does not turn this off, because it is context that is about to be lost.

- **The reviewer is not a conversation.**
  OpenCode 1's review asks its question in a child session, which the plugin claims before it can run, so none of its
  hooks (counting, wake-up, the reminder, compaction) ever acts on it; it is deleted afterwards, and the checkpoint tool
  is switched off in it and refuses it, so a reviewer cannot save without being validated.
  OpenCode 2 uses a one-off `generate.text` request with no session.
  Subagent sessions are never reviewed: their parent holds the conversation.
- **The reviewing model** is `checkpoint.model` as `provider/id`.
  Without it, OpenCode 1 uses the model of the session's last user message, and OpenCode 2 uses its default model,
  because `generate.text` takes none from a session.
  The review is a model call the user pays for, so a cheaper model is a reasonable choice.
- **`silent`, the default,** never makes anything wait: the review runs in the background and only a failure is logged.
  **`blocking`** makes the idle hook wait for the review and logs what was saved.
  There is no agent to hold up at idle, so a result is only logged; a failure is also shown as a toast
  (see [Failures](#failures)).
- **Nothing worth keeping is a result, not an error.**
  Nothing is submitted, because the daemon refuses an empty payload.
- **A failure says what to do.**
  A failed job carries the daemon's reason and `memcastle_job_retry`, an unusable reply says nothing was saved, and a
  daemon that is down says how to start it.
  Through the tool the failure is thrown, so OpenCode marks the call failed with that text.
  Everywhere else it is reported as described in [Failures](#failures).
- A `read-only` session never reviews on its own and the tool says why it saved nothing; an `off` session registers no
  hooks (see [Memory modes](#memory-modes)).
- The review never emits a fact mutation, because that needs ids no MCP tool hands out: `fact` is always `null`.
- A mistyped checkpoint value falls back to its default rather than breaking the session.

OpenCode 2 evidence is types only, as for the rest of the OpenCode 2 mapping in [the research](docs/research.md#opencode-2).

### Failures

A failure is logged at the level its class deserves and, where OpenCode has a screen to show it, shown as a toast titled
`MemCastle`.
`tests/fixtures/integration/failure-classes.json` is the one place both this plugin and Pi read the classes from.

| Class | Toast and log | What the user reads |
| --- | --- | --- |
| `daemon_unavailable` | warning | The daemon cannot be reached at the endpoint tried, then `memcastle daemon start` (or `memcastle serve`) and `memcastle status` |
| `unauthorized` | warning | A token is needed, and where to set it |
| `mode_rejected` | info | The session's own memory mode refused the operation, which is its choice at work and not a fault |
| `invalid_input` | warning | The request or the model's payload was malformed, with the daemon's `help` |
| `job_failed` | warning | The kind of job and the daemon's own `Job.error`, and that `memcastle_job_retry` retries it |
| anything else from the daemon | error | The daemon's own fault, with whatever it said |

- **OpenCode 1** shows the toast through `client.tui.showToast`.
  Under `opencode run` or a server there is no TUI, so the call has nowhere to go and the log line, written either way,
  is the only record.
- **OpenCode 2** has no toast for a server plugin: `Toast.show` belongs to the TUI plugin context, which this plugin
  cannot reach.
  A failure is logged to the console at its level, and the `/memcastle-checkpoint` command writes its answer, failure
  included, into the session.
- **The same toast is shown at most once a minute**, so a daemon that stays down is not announced at every idle event.
  The log line is written every time.
- **A tool call's failure is the call's own**: `memcastle_checkpoint` throws it, and OpenCode shows it on the call,
  so it is not toasted a second time.
- A hook that throws something that is not a classified MemCastle failure is a warning, so a bug in the plugin is
  noticed without being mistaken for a fault of the daemon.

### Memory modes

OpenCode has no per-session settings, so the mode comes from where OpenCode keeps configuration:
the plugin option `mode` in `opencode.json` (a project's own `opencode.json` makes it a per-workspace choice),
or `MEMCASTLE_MODE`, with the option winning.
It is read once, when the plugin loads, and applies to every OpenCode session in that process.
Each session still has its own MCP connection, with the daemon-side mode selected on it before any other call and again
after every reconnect, so two OpenCode processes, or an OpenCode process and a Pi session, keep different modes against
one daemon without affecting each other.
The label is translated to the daemon's wire value (`off` is `disabled`, `read-only` is `read_only`).

| | `full` | `read-only` | `off` |
| --- | --- | --- | --- |
| Connection | one per session, lazily | one per session, lazily | **none** |
| Wake-up | added to every request's system prompt | added | nothing |
| Search-before-answer reminder | added to every request | added | nothing |
| Native skills | registered | registered | nothing |
| Interval review, emergency checkpoint | run | skipped, no model call | nothing |
| `memcastle_checkpoint` tool and `/memcastle-checkpoint` | review or save | refuse, with the way out; no write is sent, even for a payload the model wrote | not registered |

- **`read-only` skips writes instead of attempting them.**
  The daemon would refuse them with `memcastle::mode::forbidden`, but only after a rejected call, and after a review
  had paid for a model call whose result must be thrown away.
  The tool throws a failure of class `mode_rejected` that says to start with `MEMCASTLE_MODE=full`.
- **`off` is absent, not refused.**
  `createCore` returns nothing, so V1 returns no hooks and V2 registers none: there is no connection, no health check,
  no system transform, no compaction hook, no skills path, no tool and no command.
  `integrations/common/test/off-isolation.test.ts` proves it on the wire against a real daemon holding a marker drawer:
  no request is made and no marker reaches the model, through every path listed in
  `tests/fixtures/integration/off-isolation.json`.
- **A skill copied by hand is out of reach.**
  The plugin registers the shared skills itself and registers none in `off`.
  Refusing a skill's load from `tool.execute.before` would need the plugin to stay active in `off` just to guard a hook,
  and would only be partial, because the skill still appears in the `skill` tool's listing.
  A copy a user installed under `.agents/skills`, or `mcp.memcastle` in `opencode.json`, is OpenCode's to load,
  so remove those when a project must be free of MemCastle.
- **A mode that cannot be parsed fails closed:** the plugin logs why and does nothing.
- **Mixed sessions:** `integrations/common/test/mixed-modes.test.ts` runs OpenCode and Pi sessions in `full`,
  `read-only` and `off` against one daemon and checks each against the requests it made.

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
src/recall-core.ts   search-before-answer without a host: the level and the text to inject (the same file as Pi's)
src/skill-text.ts    reads a shared skill from `skills/` and strips its frontmatter (the same file as Pi's)
src/project-core.ts  the project context: `.config/memcastle.toml`, `MEMCASTLE_WING` and `MEMCASTLE_ROOM` (the same file as Pi's)
src/skills.ts        the shared skills as OpenCode registers them: a `skills.paths` entry (V1), a skill list (V2)
src/checkpoint-core.ts  checkpointing without a host: settings, the review, the payload, submission (the same file as Pi's)
src/checkpoint.ts    checkpoints per OpenCode session: interval counting, emergency before compaction, the tool, the command
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

| Capability | Status | Where it lands |
| --- | --- | --- |
| `session-mode` | Implemented: label translation, per-session connection with the mode selected on every connect, `read-only` never attempts a write, `off` registers nothing, proved on the wire against a real daemon | #35, done |
| `wake-up` | Implemented: fetched on `session.created`, added to the system prompt of the first (`sync`) or first-ready (`async`) request, never blocks on a down daemon | #33, done |
| `recall` | Implemented: the shared `search-before-answer` skill is added to the system prompt of every request, at the `forceMemoryRecall` level | #36, done |
| `checkpoint` | Implemented, partly: a review every N `session.idle` events, `/memcastle-checkpoint` and the `memcastle_checkpoint` tool, all submitting a classified payload; a long single run can lag the interval | #34, done |
| `emergency-checkpoint` | Implemented: a review submitted with `emergency: true` on `experimental.session.compacting` (V1, experimental) and the `compaction` session hook (V2) | #34, done |
| `persistent-session` | Implemented: one connection per OpenCode session, kept alive, replaced with its mode re-selected when the daemon forgets it | #124, done |
| `skills` | Implemented: `search-before-answer` and `checkpoint-instructions` are discovered natively from `skills/`, never copied, and `checkpoint-instructions` also instructs the reviewing model | #36, #34, done |
| `project-context` | Implemented: `.config/memcastle.toml` and `MEMCASTLE_WING` / `MEMCASTLE_ROOM` resolved per session directory, used for the wake-up wing, checkpoint defaults and the search instruction | #183, done |
| `background-mining` | Not yet | no issue yet |
| `failure-reporting` | Implemented: the five classes plus `unexpected`, shown as a toast on OpenCode 1 and logged at their severity on both; OpenCode 2 has no toast | #126, done |
| `audit-repair` | Not yet | no issue yet |

### Gaps

None are declared.
The interval checkpoint is partial and the V1 pre-compaction hook is experimental, as described under
[Checkpointing](#checkpointing), but both are implemented, so neither is a gap.
Only `emergency-checkpoint`, `background-mining` and `audit-repair` may be gaps, each recorded as three lines:
**Missing**, **Fallback** and **Effect**.
The [research](docs/research.md#gaps-and-risks) lists where OpenCode is likely to need one, so that a gap is documented
when it is found and not discovered by a user.
