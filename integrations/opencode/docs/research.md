# OpenCode: extension mechanisms mapped to MemCastle operations

Research spike for issue #31, written so that the wake-up, checkpoint, memory-mode and recall issues of the OpenCode epic
can be implemented against this page without re-deriving OpenCode's mechanisms.

OpenCode does not have Pi's lifecycle, and this page does not pretend it does.
Where OpenCode has no clean equivalent the verdict is **gap** or **partial**, and that is a documented finding,
not something to route around at any cost.

## How this was established

Every claim carries one of three evidence labels.

- **verified**: observed by a probe plugin loaded into OpenCode 1.18.34 and driven with `opencode run`.
- **types**: read from the shipped typings of `@opencode-ai/plugin` and `@opencode-ai/sdk` 1.18.32.
  This is the ground truth for hook names and shapes, but it does not prove when a hook fires.
- **docs**: stated by opencode.ai/docs (plugins, config, mcp-servers, skills) and not otherwise confirmed.

Nothing below was verified against a long session, so a compaction, an idle timer or a multi-session server is
**types** or **docs** at best.
The first implementation issue that relies on such a claim should confirm it with a probe before building on it.

## The extension mechanisms

| Mechanism | What it is | Evidence |
| --- | --- | --- |
| Plugin | A module exporting `Plugin = (input, options?) => Promise<Hooks>`, or a `PluginModule { id?, server }`. | types |
| Plugin loading | `.opencode/plugins/` (project), `~/.config/opencode/plugins/` (global), or an npm package listed in `opencode.json` under `plugin`. | docs |
| Plugin options | An entry of `plugin` may be `[name, options]`, and the factory receives `options` as its second argument. | types |
| Plugin dependencies | A `package.json` in the config directory; OpenCode runs `bun install` at startup. | docs |
| Native MCP client | `mcp.<name>` in `opencode.json`, `"type": "remote"`, with `url` and `headers`. Tools are prefixed with the server name. | docs |
| Skills | `SKILL.md` directories under `.agents/skills/`, `.claude/skills/` or `.opencode/skills/`, loaded through the `skill` tool. | docs |
| Custom tools | A plugin's `tool` map; `execute(args, context)` receives the calling `sessionID`. | types |
| SDK client | `input.client`, a generated client for OpenCode's own HTTP server (sessions, messages, TUI, logging). | types |

### What a plugin receives

`PluginInput` carries `client`, `project`, `directory`, `worktree`, `serverUrl`, `experimental_workspace` and `$`
(Bun's shell). **verified**: these are exactly the keys the probe saw, with `serverUrl` set to `http://localhost:4096/`
in a one-shot `opencode run`.

The plugin factory runs once per OpenCode process and its closure lives as long as the process.
That closure is the only place a plugin can keep per-process state such as a map of MCP connections.
**verified**: `dispose` fired once at the end of the run, so open connections can be closed there.

### Hooks that matter to MemCastle

| Hook | Shape | Evidence |
| --- | --- | --- |
| `event` | `({ event })`, notification only, covers `session.created`, `session.idle`, `session.compacted`, `session.deleted`, `message.updated`. | verified for `session.created` and `session.idle`; types for the rest |
| `chat.message` | `(input: { sessionID, agent?, model?, messageID? }, output: { message, parts })`. Fires for each new user message and can edit `parts`. | verified that it fires once per user turn |
| `experimental.chat.system.transform` | `(input: { sessionID?, model }, output: { system: string[] })`. Appending to `system` adds to the system prompt. | verified that it fires and carries `sessionID` |
| `experimental.session.compacting` | `(input: { sessionID }, output: { context: string[], prompt? })`. Runs before the model writes the continuation summary. | types and docs; not triggered by the probe |
| `experimental.compaction.autocontinue` | Runs after compaction succeeds and can suppress the synthetic "continue" turn. | types |
| `tool.execute.before` | `(input: { tool, sessionID, callID }, output: { args })`; throwing blocks the tool. | types and docs |
| `tool.definition` | `(input: { toolID }, output: { description, parameters })`; has no `sessionID`. | verified that it fires once per tool |
| `command.execute.before` | Runs before a configured slash command, with `sessionID` and `arguments`. | types |
| `dispose` | Called when the plugin is torn down. | verified |

Two behaviours observed with the probe matter for design.

1. `session.created` arrives before the first `chat.message`, and the first system transform comes after both.
   A wake-up started on `session.created` therefore has a real head start on the first model request.
2. `experimental.chat.system.transform` ran **twice** for a single user turn (once for the title model and once for the
   main model), each with the same `sessionID`.
   Anything injected from it must be computed once per session and cached, never fetched per call.

Everything prefixed `experimental.` can change or disappear without notice, as OpenCode's own documentation says.

## Mapping each lifecycle point to a MemCastle operation

Verdicts: **clean** (a native hook does exactly this), **partial** (it can be done, with a stated limit) and **gap**
(no native mechanism; document it as Missing, Fallback and Effect).

| Capability | OpenCode mechanism | MemCastle operation | Verdict |
| --- | --- | --- | --- |
| Session start | `event` with `session.created`, then `experimental.chat.system.transform` | `memcastle_wake_up` | clean |
| Recall | The `search-before-answer` skill, plus the system transform to re-state it | `memcastle_recall`, `memcastle_search` | clean |
| Checkpoint | No interval hook; count turns on `session.idle`, review through `client.session`, submit | `memcastle_checkpoint` | partial |
| Before compaction | `experimental.session.compacting` | `memcastle_checkpoint` with `emergency: true` | clean, but experimental |
| Background mining | No scheduler hook; a timer in the plugin closure, or a stamp checked on `session.created` | `memcastle_mine` | partial |
| Session mode | No native concept; chosen by the plugin | `memcastle_set_mode` | partial |
| Failure reporting | `client.tui.showToast`, `client.app.log` | error bodies, `Job.error` | clean |
| Skills and `off` mode | `tool.execute.before` can refuse the `skill` tool | none | partial |

### Session start, to `wake_up`

- Start the fetch on `session.created`, keyed by `sessionID`, and keep the promise in a map.
- Inject the result from `experimental.chat.system.transform` by pushing onto `output.system`.
  The transform is the only hook that both knows the `sessionID` and runs before the model request.
- "Sync" means the transform awaits the promise (with a timeout), so the first response waits.
  "Async" means the transform only injects when the promise has already settled, and a later turn picks it up.
- An empty palace gives an empty answer, which is not an error and injects nothing.
- A down daemon must not block the session: time out, report one failure toast, and carry on without memory.

### Recall, to `recall` and `search`

OpenCode scans `~/.agents/skills`, so the shared `skills/search-before-answer` skill reaches it with no copy.
OpenCode's `skill` tool lists skills by name and description and loads one on demand, so the model decides when to load it.
If that proves too weak, the system transform can append the one-line reminder each turn, as the contract page allows.

### Checkpoint, to `checkpoint`

OpenCode has no "every N minutes" or "every N turns" hook.
The available signals are `session.idle` (the agent finished a run) and `message.updated`.
Counting turns on `session.idle` is the closest equivalent and is **partial** because it only fires between runs.

Classification into `preference`, `project`, `diary` or `general` needs a model.
`input.client` exposes `session.create`, `session.prompt` and `session.messages`, so a plugin can read the transcript
and ask a child session to classify it.
This costs a model call that the user pays for, and it must never recurse into the same plugin's hooks.
A manual checkpoint is a plugin custom tool the model calls, or a configured slash command.

### Before compaction, to an emergency checkpoint

`experimental.session.compacting` is the one hook that fires before context is lost, and it carries the `sessionID`.
Read the transcript with `client.session.messages`, classify, and submit with `emergency: true`.
The hook is `experimental`, so the `emergency-checkpoint` row is implementable today but carries a stability risk.
If the hook changes, the documented gap form applies: Missing is the pre-compaction hook, Fallback is
`memcastle checkpoint --emergency`, and Effect is that context is lost without a last checkpoint.

`session.compacted` also exists as an event, but it fires after the loss, so it cannot be the trigger.

### Background mining, to `mine`

There is no scheduler hook, but the plugin closure outlives sessions, so it can own a timer.
Start the timer only after the first `session.created`, never in the factory body, because OpenCode runs plugins in
processes (such as `opencode mcp list`) that never start a session.
"Never twice for one event" is the plugin's job: keep a last-run stamp on disk, because several OpenCode processes
may run side by side and the daemon does not deduplicate.

### Session mode, to `set_mode`

This is the central design finding of this spike.

MemCastle's mode belongs to one **MCP connection**.
OpenCode's native MCP client opens **one connection per OpenCode process**, shared by every session in it.
Configuring `mcp.memcastle` in `opencode.json` therefore gives all sessions in a process the same mode, and a reconnect
silently resets it to `full`.
Two sessions that need different modes cannot be served by it.

The consequence for every later issue: a plugin that has to honour per-session modes must own its **own MCP connection per
OpenCode `sessionID`**, created lazily, with the mode selected immediately after `initialize`, and closed on
`session.deleted` or `dispose`.
The plugin exposes MemCastle to the model through its `tool` map, whose `execute` receives the calling `sessionID` in its
context and so can pick the right connection.

Costs of this design, to be accepted knowingly:

- Tool names come from the plugin (for example `memcastle_search`), not from OpenCode's server prefix
  (`memcastle_memcastle_search`), so `docs/mcp-clients.md` describes the plain-MCP setup, not the plugin.
- The plugin has to declare each tool's schema itself, with zod, instead of inheriting it from the daemon.
- The user must not also enable `mcp.memcastle`, or the model sees two copies of every tool.
  The plugin README must say so.

There is nothing native to carry a mode choice either.
The choice comes from plugin options (`[name, options]` in `opencode.json`) or the `MEMCASTLE_MODE` environment
variable, and a change mid-session goes through a custom tool or slash command.

### Skills and an `off` session

A disabled session must see no MemCastle-derived context from any source, and a skill that carries it counts.
The shared skills carry no palace content, but the contract still forbids loading one into an `off` session.
`tool.definition` has no `sessionID`, so it cannot hide the `skill` tool from one session.
`tool.execute.before` does have it, and throwing there refuses the call, so the plugin can refuse to load any MemCastle
skill when the session's mode is `off`.
This is **partial**: the skill still appears in the tool's listing, and only loading it is refused.

### Failure reporting

`client.tui.showToast` shows a message and `client.app.log` writes a structured log line.
Both exist in the SDK typings.
Neither is verified under `opencode run`, where there is no TUI, so the plugin must fall back to the log.

## Gaps and risks

| Item | Missing | Fallback | Effect |
| --- | --- | --- | --- |
| Interval checkpoints | A timer or per-turn-count hook | Count `session.idle` events, or the manual checkpoint tool | A checkpoint can lag a long single run |
| Pre-compaction | A stable (non-`experimental`) hook | Use `experimental.session.compacting`; else `memcastle checkpoint --emergency` | Context may be lost without a last checkpoint if the hook changes |
| Hiding skills per session | A per-session tool or skill filter | Refuse loading in `tool.execute.before` | An `off` session still sees that the skill exists |
| Per-session mode over native MCP | One connection per session in the built-in client | A plugin-owned connection per `sessionID` | Tool names differ from the plain-MCP setup |
| TUI-only toasts | A TUI under `opencode run` or `serve` | Structured log through `client.app.log` | The failure is in the log, not on screen |

## Decisions for the follow-up issues

1. **Wake-up (#33)**: start on `session.created`, inject from the system transform, cache the answer per `sessionID`.
2. **Recall (#36)**: load the shared skill from `~/.agents/skills`; add the system-transform reminder only if needed.
3. **Checkpoint (#34)**: count turns on `session.idle`, classify through a child session, submit with a plugin tool for the
   manual path, and hook `experimental.session.compacting` for the emergency row.
4. **Memory mode (#35)**: one plugin-owned MCP connection per `sessionID`, mode selected right after `initialize`
   and re-selected on every reconnect.
5. **Foundation (#32)**: the `src/` layout follows these needs, not Pi's: a per-session connection registry, a
   mode translator, a failure classifier and the hook wiring in `src/index.ts`.
6. **Verify first**: confirm `experimental.session.compacting`, `client.tui.showToast` and the idle timing with a probe
   before building on them.
