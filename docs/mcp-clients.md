# Connect an MCP client

The daemon serves MCP at `/mcp` on its normal port, over streamable HTTP.
Any client that supports a URL-based (remote) MCP server can connect to it, and several clients can share one daemon,
which is the point: they all see the same palace.
There is no stdio mode, so no client has to spawn MemCastle.

The URL is `http://127.0.0.1:8420/mcp` with the default settings.
If you changed the port, use the `mcp` line of `memcastle status`, which always prints the URL in use.

Start the daemon first ([Quickstart](quickstart.md)), because most clients only connect when they start.

## Check the endpoint

```sh
memcastle status
```

The MCP endpoint is not a web page: a plain `GET /mcp` answers `406 Not Acceptable`, which is expected.
To test it by hand, send the MCP `initialize` request:

```sh
curl -s -X POST http://127.0.0.1:8420/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
```

The answer lists the server's instructions and the names of all tools, and the `mcp-session-id` response header
identifies the session.

## Configure your client

### OpenCode

Add a remote server to `opencode.json` (in your project, or `~/.config/opencode/opencode.json` for every project):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "memcastle": {
      "type": "remote",
      "url": "http://127.0.0.1:8420/mcp",
      "enabled": true
    }
  }
}
```

Then check that it connects:

```sh
opencode mcp list
```

!!! warning
    This is the plain MCP setup.
    If you use the OpenCode integration (`memcastle integration install opencode`, see
    [Agent integrations](integrations.md)), do **not** also add
    `mcp.memcastle`: the plugin opens its own connection per session, and enabling both shows the model two copies of every
    tool.

OpenCode prefixes each tool with the server name, so the tools appear as `memcastle_memcastle_search` and so on.
See OpenCode's [MCP servers documentation](https://opencode.ai/docs/mcp-servers/) for more options.

### Claude Code

The lifecycle plugin is the preferred setup:

```sh
memcastle integration install claude-code
```

It registers a local Claude Code marketplace and native HTTP MCP server without MemCastle editing Claude settings.
See [Claude Code integration](integrations-claude-code.md) for configuration and its lifecycle limitations.

For a plain MCP connection without the plugin, use:

```sh
claude mcp add --transport http memcastle http://127.0.0.1:8420/mcp
```

This stores the server for you in the current project.
Add `--scope user` to make it available in every project, or `--scope project` to write a `.mcp.json` you can commit
(Claude Code asks you to approve a project-scoped server the first time).
The equivalent `.mcp.json` is:

```json
{
  "mcpServers": {
    "memcastle": {
      "type": "http",
      "url": "http://127.0.0.1:8420/mcp"
    }
  }
}
```

Run `claude mcp list` to check the connection, or `/mcp` inside a session.

### Any other client

Register a remote server of type **streamable HTTP** (some clients call it just `http`) with the URL above.
No headers or credentials are needed unless you turned on [authentication](authentication.md#mcp-clients),
in which case send `Authorization: Bearer <token>` as a header on the server.
A client that can only launch a local command (stdio) cannot connect to MemCastle directly.

## Use it

The client discovers 18 tools, all named `memcastle_*`, and the daemon sends it instructions on how to use them.
The [tool reference](mcp-and-api.md#mcp-tools) lists each one.
Typical use is to ask the agent to search the palace before answering, and to save decisions with `memcastle_checkpoint`
or `memcastle_diary_write`.
MemCastle stores what it is given: deciding what is worth remembering is the agent's job.
It does not enforce a search-before-answer habit: the shared [agent skills](skills.md) carry that behaviour,
along with setup, checkpoint, wake-up and diary guidance, and you install them in your client.
Writing an integration that does this for an agent, such as when to wake up, checkpoint or mine,
is covered by the [Integration contract](integration-contract.md).

To try it, ask your agent to run `memcastle_status`, then to search for something you stored in the
[Quickstart](quickstart.md).

### Restrict a session

A session can lower its own privileges by calling `memcastle_set_mode` with `read_only` or `disabled`,
for example at session start.
See [Memory modes](memory-modes.md).
A client can raise its mode again, and authentication identifies no individual client,
so this coordinates cooperating clients rather than restricting untrusted ones.

### Session lifetime

A session lives while the client uses it.
The daemon forgets one after five minutes without any request, when the daemon restarts, or when the client sends
`DELETE` with its `mcp-session-id`.
A request on a forgotten session gets HTTP 404 `Session not found`.
A client that sees it must `initialize` again, and the new session starts as `full`,
so a client that restricted its mode calls `memcastle_set_mode` again before anything else.
A client that wants to stay connected can send an MCP `ping` more often than every five minutes.
The Pi and OpenCode integrations do all of this for you.

## Sharing one palace between clients

Point every client at the same URL.
Everything one client mines, checkpoints or writes to its diary is visible to the others immediately,
and to the CLI too.
Jobs submitted by one client can be followed and controlled by another with the `memcastle_job_*` tools.

## If it does not connect

See [MCP clients in Troubleshooting](troubleshooting.md#mcp-clients).
