# OpenCode integration

The OpenCode integration is a plugin that loads MemCastle's wake-up context when a session starts, reminds the model to
search before it answers, and checkpoints the conversation.
One plugin serves OpenCode 1 and OpenCode 2.
It is installed with [`memcastle integration`](integrations.md); this page is what is specific to OpenCode.

## Requirements

- OpenCode 1.18.29 or later (2.x included), with the `opencode` command on your `PATH`.
  The installer runs `opencode --version` and refuses an older release, naming the version it found.
- MemCastle 0.2 or later.
- A running daemon when you use OpenCode, not when you install: the plugin connects lazily and OpenCode starts normally
  without one.

## Install, update, remove

```sh
memcastle integration install opencode
memcastle integration update opencode
memcastle integration remove opencode
```

Restart OpenCode after each, because it loads plugins when it starts.
[Integrations](integrations.md) describes what each command does and checks.

## What it changes

| Where | What |
|---|---|
| `~/.local/share/memcastle/agents/opencode/` | `index.js` (the bundled plugin), `skills/` and the install receipt |
| `~/.config/opencode/plugins/memcastle.ts` | one marked line that re-exports `index.js` from there |

OpenCode loads every file in its `plugins/` directory, so the shim is the whole registration:
MemCastle does not open `opencode.json`, and every setting and other plugin you have is untouched.
`OPENCODE_CONFIG_DIR` and `XDG_CONFIG_HOME` move the directory, as they do for OpenCode.
The shim carries a comment saying MemCastle wrote it.
`remove` deletes the file only if that comment is there, and `install` refuses to overwrite a `memcastle.ts` without it
(`conflict`), so a plugin of your own with that name is safe.

The five [MemCastle skills](skills.md) come with the integration, so no separate skill installation follows.
The plugin adds the `skills/` directory beside it to OpenCode's skill paths, so they show in OpenCode's `skill` tool, and
it adds none in an `off` session.
A skill of your own with the same name wins.

Do not also add `mcp.memcastle` to `opencode.json`.
The plugin opens its own MCP connection for each session, so that a session's [memory mode](memory-modes.md) is its own,
and a second entry would show the model every tool twice.

## Configuration

The installed plugin is configured through the environment of the `opencode` process.
A default setup needs none.
The ones you are most likely to set:

| Variable | Default | Meaning |
|---|---|---|
| `MEMCASTLE_MODE` | `full` | `full`, `read-only` or `off` for this session |
| `MEMCASTLE_AUTH_TOKEN` | none | The bearer token, when the daemon [requires one](authentication.md) |
| `MEMCASTLE_BIND`, `MEMCASTLE_PORT` | `127.0.0.1`, `8420` | Where to look when no running daemon is found through its registry file |
| `MEMCASTLE_WAKE_UP` | `true` | Whether a session start injects the wake-up |
| `MEMCASTLE_CHECKPOINT` | `true` | Whether the interval review runs |

`MEMCASTLE_MODE` takes this integration's labels (`full`, `read-only`, `off`), which are not the daemon's
(`full`, `read_only`, `disabled`).
The complete list is in the
[plugin's README](https://github.com/noirbizarre/memcastle/blob/main/integrations/opencode/README.md#configuration).

The plugin also accepts options, which win over the environment, but only when it is listed in `opencode.json`'s
`plugin` (OpenCode 1) or `plugins` (OpenCode 2) list, and MemCastle does not edit that file.
To use options, point that entry at `~/.local/share/memcastle/agents/opencode/index.js`,
and remove the shim with `rm ~/.config/opencode/plugins/memcastle.ts` so the plugin is not loaded twice;
`memcastle integration update opencode` writes the shim again, so prefer the environment when you can.

## Troubleshooting

- **Nothing happens in OpenCode.**
  Run `memcastle integration list`: the `STATE` must be `installed`.
  Then check that the daemon is running (`memcastle status`) and that OpenCode was restarted.
- **`conflict`.**
  A `plugins/memcastle.ts` that MemCastle did not write is in the way.
  Move it aside, or rename it, and install again.
- **The `checkpoint` tool is missing.**
  The plugin imports OpenCode's `tool` helper from `@opencode-ai/plugin` at load time and logs when it cannot;
  every other feature keeps working.
- **Other failures.**
  See the [troubleshooting table](integrations.md#troubleshooting).
