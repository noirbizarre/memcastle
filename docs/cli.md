# CLI reference

The `memcastle` binary is one daemon plus a set of thin clients.
`serve` (alias `daemon`) runs the daemon, and `migrate` talks to storage directly.
Every other subcommand is an HTTP call to a running daemon, so it fails with `memcastle::client::not_running`
(and points you at `memcastle serve`) when none is running.
Run `memcastle <command> --help` for the authoritative text of any flag.

Client commands print the daemon's JSON answer, so the output pipes into `jq`.
`status` is the exception: it prints a readable report, and `--json` gives the same report as JSON.

## Global flags

These flags are accepted by every subcommand, before or after its name.

| Flag | Environment variable | Meaning |
|---|---|---|
| `-v`, `-vv` | none | Log MemCastle at `debug` (`-v`) or `trace` (`-vv`), and print the full cause chain of an error. |
| `--config <FILE>` | `MEMCASTLE_CONFIG` | Config file to read. It must exist when named explicitly. |
| `--palace <PATH>` | none | Absolute palace directory, overriding `MEMCASTLE_PALACE_PATH` and `palace.path`. |
| `--mode <MODE>` | `MEMCASTLE_MODE` | Run the command as a session in `full` (the default), `read_only` or `disabled` mode. |

`--mode` makes a client command behave like an agent session in that mode: the daemon rejects what the mode forbids.
It is a way to check what a restricted session can do, see [Memory modes](memory-modes.md).
See [Configuration](configuration.md) for how these flags combine with the config file and environment variables.

## Daemon lifecycle

| Command | What it does |
|---|---|
| `memcastle serve [--bind <IP>] [--port <PORT>]` | Run the daemon in the foreground. `daemon` is an alias. |
| `memcastle restart [--bind <IP>] [--port <PORT>]` | Stop the running daemon, start a detached new one and wait until it serves. |
| `memcastle stop` | Ask the running daemon to shut down gracefully. |
| `memcastle status [--json]` | Report whether the daemon is running, where, which palace, and whether the datastore is healthy. |
| `memcastle migrate [--check \| --status]` | Apply, or just inspect, the palace's migrations without a daemon. |

`--bind` and `--port` only exist on `serve` and `restart`;
client commands find the daemon through its registry file instead.
See [Running the daemon](daemon.md) for the details of each, and [Migrations and upgrades](migrations.md) for `migrate`.

### Exit codes of `status`

| Code | Meaning |
|---|---|
| `0` | A daemon is running and its datastore is healthy. |
| `1` | A daemon answered but is degraded (datastore unreachable or migrations pending), or an error occurred. |
| `3` | No daemon is running. |

Every other command exits `0` on success and `1` on any failure.
Failures print a diagnostic with a `memcastle::<module>::<kind>` code and what to do about it.

## Memory

### `search`

```sh
memcastle search <QUERY> [--limit <N>] [--wing <WING>] [--room <ROOM>]
```

Full-text (BM25) search over drawer content.
`--limit` defaults to 10 and is capped at 200.
`--wing` restricts results to a wing, `--room` to a room directly.

### `recall`

```sh
memcastle recall <QUERY> [--limit <N>] [--wing <WING>]
```

The recall-oriented counterpart to `search`.
It returns matching drawers verbatim, with the same limit rules.

### `wake-up`

```sh
memcastle wake-up --agent-identity <ID> [--wing <WING>] [--max-items <N>] [--max-bytes <N>]
```

Builds the session-start context for an agent identity:
its most recent diary entry (only when `--wing` is given) plus recent checkpoint-originated highlights.
`--max-items` defaults to 10 and `--max-bytes` to 8192; highlights are dropped whole, never cut mid-content.
`wake_up` is accepted as an alias.

### `diary`

```sh
memcastle diary write --agent-identity <ID> --wing <WING> <CONTENT>
memcastle diary read  --agent-identity <ID> --wing <WING> [--limit <N>]
```

Writes an entry to an agent's journal in a wing, or reads the newest entries back.
`--limit` defaults to 20.
MemCastle stores and returns the agent identity exactly as given, so use one consistent string per agent.

### `mine`

```sh
memcastle mine <PATH> [--wing <WING>]
```

Submits a job that reads a directory into drawers, one per file.
`--wing` defaults to the directory's name.
The command returns the job immediately; follow it with `memcastle jobs show <id>`.
See [Storage and data](storage.md#what-mining-reads) for which files are read.

### `checkpoint`

```sh
memcastle checkpoint [--payload <FILE>] [--emergency]
```

Submits a checkpoint job that durably persists an already-classified batch of memory writes.
The payload is JSON read from `--payload`, or from standard input when omitted;
its shape is described in [MCP tools and REST API](mcp-and-api.md#checkpoint-payload).
`--emergency` raises the job to the highest priority, for save-before-crash situations only.

## Maintenance

### `audit`

```sh
memcastle audit [--scope <WING>]
```

Submits a read-only consistency report job.
`--scope` restricts only the embedding counts to one wing; orphan and dangling-reference findings are always palace-wide.
Read the report with `memcastle jobs show <id>`.

### `repair`

```sh
memcastle repair [--apply] [--based-on-job <JOB_ID>]
```

Submits a repair job.
Without `--apply` it is a dry run that only reports what it would remove.
With `--apply` it deletes orphan drawers, which is a write and is refused in `read_only` and `disabled` modes.
`--based-on-job` narrows the repair to what a previous audit found; a live scan still decides what is removed.

## Jobs

```sh
memcastle jobs list [--status <STATUS>]
memcastle jobs show <ID>
memcastle jobs pause <ID>
memcastle jobs resume <ID>
memcastle jobs cancel <ID>
memcastle jobs retry <ID>
memcastle jobs demo [--steps <N>]
```

`--status` is one of `queued`, `running`, `paused`, `completed`, `failed` or `cancelled`.
Pausing and cancelling are requests: a running job stops at its next unit of work, not instantly.
`retry` only applies to a failed job, and `resume` only to a paused one.
`demo` submits a synthetic job (5 steps by default) that touches no palace content,
which is a quick way to check the daemon end to end.
The states a job moves through are in [Architecture](architecture.md#job-lifecycle).

## Not implemented yet

`wings`, `rooms`, `drawers` and `maintenance` are reserved names.
They exist so the command surface is stable, and they return a `not_implemented` error today.
