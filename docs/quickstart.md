# Quickstart

This walks from a fresh install to a daemon holding memories you can search, and shows that they survive a restart.
It uses the defaults: a palace in `~/.local/share/memcastle/default` and a daemon on `127.0.0.1:8420`.
You need MemCastle installed, see [Installation](installation.md).

## 1. Start the daemon

`memcastle serve` runs in the foreground, so give it its own terminal:

```sh
memcastle serve
```

The first start creates the palace and prepares its database, then logs a line such as
`memcastle daemon listening bind=127.0.0.1:8420`.
Leave it running.

## 2. Check it

In another terminal:

```sh
memcastle status
```

```text
MemCastle is running
  version    0.1.0 (pid 2528880, up 3s)
  endpoint   http://127.0.0.1:8420 (from the daemon's registry file)
  mcp        http://127.0.0.1:8420/mcp
  palace     default (/home/alice/.local/share/memcastle/default)
  datastore  ok - embedded /home/alice/.local/share/memcastle/default/db, migrations 2/2
  drawers    0
  jobs       0 queued, 0 running, 0 paused
  mode       full
  auth       disabled
Restart with `memcastle restart`, stop with `memcastle stop`.
```

The `mcp` line is the URL to give your agent, see [Connect an MCP client](mcp-clients.md).

## 3. Give it something to remember

Mining reads a directory into the palace, one memory per file.
It runs as a background job, so the command returns immediately:

```sh
memcastle mine ./my-project --wing my-project
```

The output is the queued job, including its `id`.
Watch it finish:

```sh
memcastle jobs list
```

In a terminal this is a table whose `STATUS` column reads `completed` once the job is done.
Piped (`memcastle jobs list | jq`), it is JSON, where a completed job has `"status": "completed"` and a `result` such as
`{"files_considered": 2, "limit": 2000, "truncated": false}`.
Without `--wing`, memories are filed under a wing named after the directory.
[What mining reads](storage.md#what-mining-reads) says which files are skipped.

You can also write memories directly.
A diary entry belongs to an agent identity, so it is a natural place for "what I did today":

```sh
memcastle diary write --agent-identity me --wing my-project "Switched the build to release mode."
```

Or save a batch with a checkpoint.
The payload is JSON that has already been sorted into buckets by whoever produced it (see the
[checkpoint payload](mcp-and-api.md#checkpoint-payload)):

```sh
echo '{"items":[{"destination":"preference","content":"Always run the formatter before committing.","tags":["workflow"],"source":{"kind":"manual","uri":null,"agent":"me"}}]}' \
  | memcastle checkpoint
```

## 4. Find it again

```sh
memcastle search "formatter"
memcastle search "release mode" --wing my-project
memcastle diary read --agent-identity me --wing my-project
memcastle wake-up --agent-identity me --wing my-project
```

`search` is full-text: it matches the words you stored, and returns JSON with each drawer's content verbatim.
`wake-up` builds the context an agent would load at the start of a session.
Pipe any of them through `jq` to pick out what you need, for example `memcastle search formatter | jq '.[].content'`.

## 5. Stop, restart, and find everything still there

```sh
memcastle stop
memcastle serve
```

Then run the same `search` again in the other terminal.
The palace is stored on disk, so your memories are still there, and any job that was in progress resumes.
A newer release migrates the palace before serving, see [Migrations and upgrades](migrations.md).

## Where next

- [Connect an MCP client](mcp-clients.md) so your agent can use the palace.
- [Running the daemon](daemon.md) to keep it up under systemd or launchd, and to change its port.
- [Configuration](configuration.md) to move the palace or change settings.
- [Memory modes](memory-modes.md) to make a session read-only.
- [Troubleshooting](troubleshooting.md) if a step did not work as shown.
