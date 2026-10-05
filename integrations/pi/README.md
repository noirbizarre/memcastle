# MemCastle for Pi

A [Pi extension](https://pi.dev) that decides *when* to call MemCastle.
Everything it asks of MemCastle is an MCP call to the daemon, and it never touches the database, the job code or the
admin endpoint (AGENTS.md invariant 8, enforced by the `integrations-http-only` hook).

**Status: scaffold.**
The connection, memory-mode, discovery and failure foundations are real and tested against a real daemon.
Wake-up, search-before-answer and checkpointing (interval, manual and emergency) are implemented; the other capability modules are
registered but empty, and each names the issue that fills it in.

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

The variables below share their names with the MemCastle CLI's where it has one, but not always their values:
`MEMCASTLE_MODE` takes this integration's labels (`full`, `read-only`, `off`), whereas the CLI's `--mode` and `MEMCASTLE_MODE`
take the daemon's (`full`, `read_only`, `disabled`).
A shell that exports one set therefore breaks the other tool, so set the integration's mode in its own settings when you
also use the CLI.
The wake-up and checkpoint variables, and `MEMCASTLE_FORCE_MEMORY_RECALL`, belong to this integration alone: the CLI does not read them.

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
| `MEMCASTLE_WING`, `MEMCASTLE_ROOM` | none | The project's wing and room, overriding `.config/memcastle.toml` (see [Project context](#project-context)) |
| `MEMCASTLE_FORCE_MEMORY_RECALL` | `sometimes` | `off`, `sometimes` or `always`: how hard to push the model to search first |
| `MEMCASTLE_CHECKPOINT` | `true` | Whether the interval review runs; `/memcastle-checkpoint` works either way |
| `MEMCASTLE_CHECKPOINT_INTERVAL` | `10` | How many exchanges separate two interval reviews |
| `MEMCASTLE_CHECKPOINT_MODE` | `silent` | `silent` reviews in the background; `blocking` makes the agent wait and shows the result |
| `MEMCASTLE_CHECKPOINT_MODEL` | none | `provider/id` of the model that reviews the conversation; none means the session's own |

The daemon is found through its registry file, then the configured address, and each candidate is checked with
`GET /api/health` because the file is only a hint.
A mode the extension cannot parse **fails closed**: it tells the user why and does nothing, rather than treating a
typo as `full`.
An `off` session opens no connection at all.

### Project context

A project declares its memory scope in `.config/memcastle.toml`, and `MEMCASTLE_WING` and `MEMCASTLE_ROOM` override it.
The extension reads both itself, from Pi's working directory, and passes the result to MemCastle as ordinary `wing` and `room` arguments:
the daemon is not asked to resolve anything.
The file, the discovery rules and the precedence are in [Project configuration](../../docs/project-config.md).

- **Wake-up** asks about the project's wing under the default `project` source; `user`, `custom` and `none` still win.
- **Checkpoints** file `project` and `diary` items with no wing of their own under the project's wing,
  and never move a `preference` or a `general` item.
- **Search** is preceded by one line telling the model which wing and room to pass.
- A broken file or variable is reported once, and the session carries on without a project scope.
- An `off` session reads no project file.

### Memory modes

`MEMCASTLE_MODE` is chosen once, when the Pi session starts, and holds for the whole session.
The extension translates the label to the daemon's wire value (`off` is `disabled`, `read-only` is `read_only`) and
selects it on the session's MCP connection before any other call, again after every reconnect.
Other Pi sessions, in this process or another, are other MCP sessions and keep their own mode.

| | `full` | `read-only` | `off` |
| --- | --- | --- | --- |
| Connection | opened | opened | **none** |
| Wake-up | injected | injected | nothing |
| Search-before-answer reminder | added to the system prompt | added | nothing |
| Interval review | runs | skipped, no model call | nothing |
| `/memcastle-checkpoint` | reviews and saves | says the session is read-only, no model call, no write | says MemCastle is not active |
| `/memcastle-wake-up` | shows the briefing | shows the briefing | says MemCastle is not active |

- **`read-only` skips writes instead of attempting them.**
  The daemon would refuse them with `memcastle::mode::forbidden`, but only after a review had paid for a model call
  whose result must be thrown away, and a rejected call is noise a client that knows its own mode has no reason to make.
  The command reports the refusal as information, with the way out (`MEMCASTLE_MODE=full`), not as a failure.
- **`off` is not "refused", it is absent.**
  No manager exists, so there is no connection, no health check, no notification and no message in the model's context.
  The commands still exist and answer `MemCastle is not active in this session.`, which is the reason for the silence
  and carries nothing from the palace.
  `integrations/common/test/off-isolation.test.ts` proves this on the wire against a real daemon holding a marker drawer:
  no request is made and no marker reaches the user or the model, through every path listed in
  `tests/fixtures/integration/off-isolation.json`.
- **A skill copied by hand is out of reach.**
  The extension loads skills from `skills/` itself and loads none in `off`.
  A copy a user installed under `.agents/skills`, or a separate MCP entry for MemCastle in Pi's own configuration,
  is Pi's to load, and an integration that registered nothing cannot stop it.
  Remove those copies when a project must be free of MemCastle.
- **A mode that cannot be parsed fails closed:** the user is told, and the extension does nothing.
- **Mixed sessions:** `integrations/common/test/mixed-modes.test.ts` runs Pi and OpenCode sessions in `full`,
  `read-only` and `off` against one daemon and checks each against the requests it made.

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

### Search before answering

Every turn, the extension adds the shared [`search-before-answer`](../../skills/search-before-answer/SKILL.md) skill to
the system prompt, so the model searches MemCastle before it answers a question about past work, decisions, people or
preferences, and quotes what it finds verbatim.
The text is read from `skills/` in the repository, byte for byte, and is never copied into this package.

The level is named after `pi-palace`'s `forceMemoryRecall.level`.
It is a client policy: MemCastle itself never forces a search, and no daemon setting changes that.

- **`sometimes`**, the default, injects the skill as written, which asks for a search when a question may depend on
  something said or decided before and skips it for self-contained questions.
- **`always`** injects the same text and adds one line, which says to search before every answer.
  The line lives in the extension and the shared skill is not edited, because the skill's own advice to skip the search
  is what every other client reads.
- **`off`** injects nothing.
- **Every turn, never accumulating.**
  The text is appended to the system prompt in `before_agent_start`, which Pi rebuilds for each model call.
  It is not a message, so it does not pile up in the session history, and it is still there after a compaction.
- A `read-only` session gets it, because searching is allowed there.
  An `off` session has no manager and gets nothing.
- An unreadable skill file, which means the extension was installed away from the repository checkout, is reported once
  per session and the turn carries on without the reminder.
- A mistyped level falls back to `sometimes` rather than breaking the session, because it only changes what the model
  is told.

### Checkpointing

Every `MEMCASTLE_CHECKPOINT_INTERVAL` exchanges, the extension asks its own model what in the conversation is worth
keeping, and submits the answer to MemCastle as a checkpoint.
`/memcastle-checkpoint` does the same on demand.
The settings are named after `pi-palace`'s `piPalace.interval`, `piPalace.mode` and `piPalace.model`.

Classification happens here and never in MemCastle: the daemon stores what it is handed and decides nothing.
The reviewing model is given the shared [`checkpoint-instructions`](../../skills/checkpoint-instructions/SKILL.md) skill,
read from `skills/` and never copied, and replies with a payload of items, each tagged `preference`, `project`, `diary`
or `general`.
The extension validates that reply, stamps each item with `source.agent` (the `agentIdentity`, default `pi`), and
submits it with `memcastle_checkpoint`.

- **An exchange is a prompt and the agent's whole answer to it**, counted at `agent_end`.
  A tool-heavy answer is one exchange, not one per model turn.
- **Each review reads only what the last one did not.**
  Tool output, summaries and the injected wake-up are not part of what it reads.
  A review that fails before the daemon has the items is retried over the same exchanges.
- **`silent`**, the default, never makes the agent wait: the review runs in the background, and only a failure is shown.
  **`blocking`** shows a status while it runs, waits for the job to complete and says what was saved.
  `MEMCASTLE_CHECKPOINT_MODE` is not `MEMCASTLE_MODE`: the first is whether the agent waits, the second is what a
  session may do to memory.
- **`MEMCASTLE_CHECKPOINT_MODEL`** is the model that reviews, as `provider/id`.
  A model Pi does not know is reported, naming the setting; it is never replaced silently.
  The review is a model call the user pays for, so a cheaper model is a reasonable choice.
- **`/memcastle-checkpoint [hint]`** forces a review now, whatever `MEMCASTLE_CHECKPOINT` says, and always shows its
  result.
  Text after the command is passed to the reviewing model as the user's own words about what to keep.
  It postpones the next interval review, so the same exchanges are not reviewed twice.
- **Nothing worth keeping is a result, not an error.**
  Nothing is submitted, because the daemon refuses an empty payload.
- **A failure says what to do.**
  A job that failed carries the daemon's reason and `memcastle_job_retry`; a reply the model got wrong says nothing was
  saved and suggests a more capable model; a daemon that is down carries how to start it.
  None of them is shown as "nothing worth keeping".
- **A failure is shown at the level it deserves** (see [Failures](#failures)).
- A `read-only` session never reviews on its own, since the daemon would refuse the write after a paid model call.
  `/memcastle-checkpoint` there says why nothing was saved.
  An `off` session has no manager, so nothing is reviewed and the command says MemCastle is not active.
- The review never emits a fact mutation: that needs entity and relationship ids that no MCP tool lets a client obtain,
  so `fact` is always `null`, as the shared skill says.
- A mistyped checkpoint value falls back to its default rather than breaking the session.

### Emergency checkpoint

Pi compacts a conversation by replacing it with a summary, so whatever was not kept yet is about to be lost.
The extension reviews the conversation at `session_before_compact` and submits it with `emergency: true`,
which makes the daemon's job Critical priority: it is claimed before any queued background work, such as mining.

- **It runs whatever `MEMCASTLE_CHECKPOINT` says.**
  That setting switches the interval review off, and losing the conversation is the one moment a save is still wanted.
- **It does not wait for the job.**
  The review's own model call is awaited, because Pi runs the handler before it compacts, but the job is only queued.
- **It never holds the compaction up for long.**
  After 30 seconds the compaction carries on, and a review still running submits and reports for itself.
- **It never cancels or replaces the compaction.**
  A failure is shown as a notification, and Pi compacts as it would have anyway.
- **A successful save is silent.**
  It postpones the next interval review, so the same exchanges are not reviewed twice.
- **A `read-only` session, an `off` session and a daemon that never connected do nothing**,
  and cost no model call.

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
src/recall-core.ts            search-before-answer without a host: the level and the text to inject (the same file as OpenCode's)
src/skill-text.ts             reads a shared skill from `skills/` and strips its frontmatter (the same file as OpenCode's)
src/search-before-answer.ts   injects that skill into the system prompt at every `before_agent_start`
src/checkpoint-core.ts        checkpointing without a host: settings, the review, the payload, submission (the same file as OpenCode's)
src/checkpoint-agent.ts       the interval review (counts `agent_end`) and the emergency one (`session_before_compact`): reads Pi's transcript, asks Pi's model
src/checkpoint-tool.ts        `/memcastle-checkpoint`: the manual save
src/daily-mine.ts             background mining on the extension's own schedule (#26, scheduling only), empty
test/                         bun tests against a real `memcastle serve`; they read tests/fixtures/integration/
```

`modes`, `failures`, `settings`, `daemon-client`, `persistent-mcp-client`, `wake-up-core`, `recall-core`,
`checkpoint-core` and `skill-text` are a deliberate copy of the small client
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
| `session-mode` | Implemented: label translation, mode selected on every connect, `read-only` never attempts a write, `off` opens nothing and injects nothing, proved on the wire against a real daemon | #27, done |
| `wake-up` | Implemented: fetched at session start, injected as a message before the first (`sync`) or first-ready (`async`) response, never blocks on a down daemon | #22, done |
| `recall` | Implemented: the shared `search-before-answer` skill is appended to the system prompt every turn, at the `forceMemoryRecall` level | #25, done |
| `checkpoint` | Implemented: an interval review by Pi's own model and `/memcastle-checkpoint`, both submitting a classified payload and reporting a failed job with how to retry it | #23, done |
| `emergency-checkpoint` | Implemented: the same review submitted with `emergency: true` at `session_before_compact`, whatever the interval setting says, bounded by a 30 second deadline, never cancelling the compaction; a `read-only` or `off` session does nothing | #24, done |
| `persistent-session` | Implemented: one connection per Pi session, kept alive, replaced with its mode re-selected when the daemon forgets it | #29, done |
| `skills` | Implemented: `search-before-answer` is injected and `checkpoint-instructions` instructs the reviewing model, both read from `skills/` and never copied; `off` sessions get nothing | #25, #23, done |
| `project-context` | Implemented: `.config/memcastle.toml` and `MEMCASTLE_WING` / `MEMCASTLE_ROOM` resolved from Pi's directory, used for the wake-up wing, checkpoint defaults and the search instruction | #183, done |
| `background-mining` | Not yet | #26 |
| `failure-reporting` | Implemented: the five classes plus `unexpected`, each shown as a Pi notification at the severity the shared fixture promises, with `help` | #30, done |
| `audit-repair` | Not yet | #28 |

### Failures

Every failure is shown as a Pi notification that starts with `MemCastle:`, says what went wrong, and says what to do.
The class decides the level, and `tests/fixtures/integration/failure-classes.json` is the one place both Pi and OpenCode read it from.

| Class | Level | What the user reads |
| --- | --- | --- |
| `daemon_unavailable` | warning | The daemon cannot be reached at the endpoint tried, then `memcastle daemon start` (or `memcastle serve`) and `memcastle status` |
| `unauthorized` | warning | A token is needed, and where to set it |
| `mode_rejected` | info | The session's own memory mode refused the operation, which is its choice at work and not a fault |
| `invalid_input` | warning | The request or the model's payload was malformed, with the daemon's `help` |
| `job_failed` | warning | The kind of job and the daemon's own `Job.error`, and that `memcastle_job_retry` retries it |
| anything else | error | The daemon's own fault, or a bug, with whatever the daemon said |

A daemon that is down is reported once at session start and then each command says why it did nothing,
so the user is not told the same thing at every prompt.
The session always carries on without MemCastle: a failure never stops Pi from answering.

### Gaps

None are declared yet.
Only `background-mining` and `audit-repair` may be gaps, each recorded as three lines:
**Missing**, **Fallback** and **Effect**.
`emergency-checkpoint` is not one: Pi has a `session_before_compact` event.
