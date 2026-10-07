# Pi integration

The Pi integration is an extension that loads MemCastle's wake-up context when a Pi session starts, reminds the model to
search before it answers, and checkpoints the conversation, and can audit the palace.
It is installed with [`memcastle integration`](integrations.md); this page is what is specific to Pi.

## Requirements

- Pi 0.99 or later, with the `pi` command on your `PATH`.
  The installer runs `pi --version` and refuses an older Pi, naming the version it found.
- MemCastle 0.2 or later.
- A running daemon when you use Pi, not when you install:
  the extension loads without one and tells you once how to start it.

## Install, update, remove

```sh
memcastle integration install pi
memcastle integration update pi
memcastle integration remove pi
```

Restart Pi after each, because it loads extensions when it starts.
[Integrations](integrations.md) describes what each command does and checks.

## What it changes

| Where | What |
|---|---|
| `~/.local/share/memcastle/agents/pi/` | `extension.js` (the bundled extension), `package.json`, `skills/` and the install receipt |
| Pi's settings | one package entry for that directory, written by `pi install` |

MemCastle runs `pi install <directory>` to register the copy, `pi list` to check it
and `pi remove <directory>` to undo it.
It never edits Pi's `settings.json` itself, so the packages you already have, and any other setting, are untouched.
Pi does not install dependencies for a local package, which is why the extension is shipped as one self-contained file.
There is no separate MemCastle entry to add to Pi's MCP configuration:
the extension speaks MCP itself, and a second entry would only duplicate it.

The five [MemCastle skills](skills.md) come with the integration, so no separate skill installation follows.
The extension offers the `skills/` directory beside it to Pi when a session starts, so Pi lists them and has a
`/skill:<name>` command for each, and it offers none in an `off` session.
A skill of your own with the same name, in `~/.agents/skills` for example, takes precedence.

## Auditing the palace

`/memcastle-audit [wing]` runs MemCastle's read-only audit and shows what it found.
When there are orphan drawers it then shows the dry-run repair plan and asks once, in a dialog, whether to apply it.
One question covers the whole plan, because the daemon cannot repair a subset of it.
Declining changes nothing.
After a confirmed repair the command writes a before and after summary to the diary.
A `read-only` session stops at the plan and is never asked, and an `off` session does nothing.

## Configuration

Pi reads the extension's settings from the environment of the `pi` process.
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
The complete list, and what each [memory mode](memory-modes.md) does to a session, is in the
[extension's README](https://github.com/noirbizarre/memcastle/blob/main/integrations/pi/README.md#configuration).

## Troubleshooting

- **Nothing happens in Pi.**
  Run `memcastle integration list`: the `STATE` must be `installed`.
  Then check that the daemon is running (`memcastle status`) and that Pi was restarted.
- **`pi install` is refused** (`registration_failed`).
  The message is Pi's own.
  Run `pi install ~/.local/share/memcastle/agents/pi` by hand to see it in full.
- **The wake-up is missing a skill reminder, or Pi lists no MemCastle skill.**
  Pi reports a missing skill file once per session and carries on.
  `memcastle integration install pi` restores the `skills/` directory beside the extension.
  A session in the `off` mode lists none on purpose.
- **Other failures.**
  See the [troubleshooting table](integrations.md#troubleshooting).
