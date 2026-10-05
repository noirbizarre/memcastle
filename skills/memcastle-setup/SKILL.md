---
name: memcastle-setup
description: Detect, install, start and connect MemCastle, the local long-term memory server for agents. Use when the user asks to set up or enable MemCastle, when `memcastle_*` tools are missing or fail to connect, or before relying on any other MemCastle skill.
license: MIT
compatibility: Needs a shell on the user's machine. Does not need a running MemCastle daemon, which is what it sets up.
metadata:
  memcastle-version: ">=0.2.0"
---

# Set up MemCastle

MemCastle is a daemon that serves one memory palace over MCP and HTTP.
This skill gets an agent from "maybe nothing is installed" to "the `memcastle_*` tools answer".
It only prepares the connection: every runtime capability stays in the `memcastle` binary and its MCP server.
Ask the user before installing software or changing their client configuration.

## 1. Detect the binary

```sh
memcastle --version
```

If the command is not found, install it (step 2).
If it prints a version, compare it with the `memcastle-version` range in the frontmatter of this skill.
The range is the MemCastle versions these instructions are written for, and `>=0.2.0` means 0.2.0 or any later release.
A newer binary is fine, because MemCastle keeps backward compatibility where it can.
A binary older than the range was released before the tools these skills name, so offer to upgrade it (step 2),
or to install the skills from the tag that matches the binary, and do not guess at tools that may be missing.
Then go to step 3.

## 2. Install the binary

Pick what fits the user's system and ask which they prefer when several apply.

- macOS: `brew install noirbizarre/homebrew-tap/memcastle`
- Arch Linux: `paru -S memcastle-bin`
- Debian, Ubuntu, Fedora and other Linux: a `.deb` or `.rpm` from the GitHub release page, or the raw
  `memcastle_<version>_<platform>` binary renamed to `memcastle` and put on `PATH`
- From source, with Rust 1.90 or newer, in a checkout of the repository: `cargo install --path .`

Run `memcastle --version` again to confirm.
The installation page of the MemCastle documentation lists every method, including checksum and attestation checks.

## 3. Check and start the daemon

```sh
memcastle status
```

The exit code says what to do:

- `0`: running and healthy, go to step 4.
- `3`: not running, start it with `memcastle daemon start` and check again.
- `1`: running but degraded, or an error.
  Read the printed diagnostic and its `help` line, and do not restart blindly.

Where the package installed a systemd user unit, `systemctl --user enable --now memcastle` keeps it running across logins.
`memcastle serve` runs it in the foreground, which is only useful for watching its log.
The daemon listens on `127.0.0.1:8420` by default, and the `mcp` line of `memcastle status` prints the exact URL in use.

To verify health without the CLI, `GET /api/health` answers `{"status":"ok"}` and needs no credentials.

## 4. Connect the MCP server

MemCastle speaks streamable HTTP at `http://127.0.0.1:8420/mcp`.
There is no stdio mode, so a client that can only spawn a local command cannot connect.

- Claude Code: `claude mcp add --transport http memcastle http://127.0.0.1:8420/mcp`,
  with `--scope user` for every project, then `claude mcp list` to check.
- OpenCode: add an `mcp.memcastle` entry with `"type": "remote"`, the URL and `"enabled": true` to `opencode.json`,
  then `opencode mcp list` to check.
  Skip this if the user installs the MemCastle OpenCode plugin: it connects by itself, and both would show every tool twice.
- Any other client: register a remote server of type streamable HTTP with that URL.

When authentication is enabled, the client sends `Authorization: Bearer <token>`.
The user creates a token with `memcastle auth generate`, which shows it once.
Never ask for the token to be pasted into a conversation, and never write it into a file the user commits.

## 5. Verify end to end

Once the client has reloaded its MCP servers, call `memcastle_status`.
It reports the version, the palace and the session's memory mode.
A reply means the setup is done.

## When something fails

- `memcastle::client::not_running`: start the daemon (step 3).
- `memcastle::server::bind_failed`: the port is taken, so pick another with `MEMCASTLE_PORT` or the configuration file
  and update the client URL.
- `memcastle::auth::unauthorized`: authentication is on and the client sent no valid token (step 4).
- Tools missing in the client: it connected before the daemon started, so restart the client or reload its MCP servers.

## Rules

- Do not edit the palace directory or the database by hand, and do not open it while a daemon owns it.
- A memory mode other than `full` is the user's choice, so do not change it here.
- Do not describe features this skill did not verify against `memcastle --help` or the documentation.
