# CLI reference

The `memcastle` binary is one daemon plus a set of thin clients.
`serve` runs the daemon in the foreground, `daemon start` runs it in the background,
and `migrate` talks to storage directly.
Every other subcommand is an HTTP call to a running daemon, so it fails with `memcastle::client::not_running`
(and points you at `memcastle daemon start`) when none is running.
Three things differ:
`status` reports a stopped daemon instead of failing, `daemon restart` starts a daemon when none is running,
and the reserved commands (see [Not implemented yet](#not-implemented-yet)) fail with `memcastle::cli::not_implemented`
without contacting a daemon.
Run `memcastle <command> --help` for the authoritative text of any flag.
There is no `help` subcommand: `--help` is the one way to ask.

Client commands print the daemon's JSON answer, so the output pipes into `jq`.
`status`, `db start` and `db status` are the exceptions: they print a readable report,
and `--json` gives the same report as JSON.
`jobs list` is the other: it prints a table when standard output is a terminal, and JSON when it is not.
See [Output, colour and prompts](#output-colour-and-prompts).

## Output, colour and prompts

What the CLI shows depends on where its output goes, never on a flag, so a script needs no change:
a terminal gets decoration, and a pipe or a file gets plain data.

| Setting | Effect |
|---|---|
| `NO_COLOR` set to anything | No colour anywhere, even on a terminal. |
| `CLICOLOR=0` | The same. |
| `CLICOLOR_FORCE=1` | Colour even when output is piped, for tools that render escape codes. |
| `TERM=dumb` | No colour. |

Colour is added around words and never replaces them.
It colours `--help`, `status` and `db status` reports (healthy in green, degraded or unavailable in red,
things that need attention in yellow), the table of `jobs list`, and the diagnostics printed on failure.
A job status has the same colour wherever it is shown:
queued yellow, running cyan, paused magenta, completed green, failed red and cancelled dim.
Standard output and standard error are decided separately:
with `memcastle status 2> errors.log` the report stays coloured and the log stays plain.

`repair --apply`, `auth generate`, `auth revoke` and `jobs cancel` ask for confirmation, with a prompt on standard error
that defaults to "no".
`--yes` (or `-y`) skips the question.
When standard input or standard error is not a terminal, a script or CI job for instance,
they never ask and proceed, because the caller has already decided.
Declining the prompt exits with `memcastle::cli::aborted` and changes nothing.

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
| `memcastle serve [--bind <IP>] [--port <PORT>] [--assets-dir <DIR>]` | Run the daemon in the foreground. |
| `memcastle daemon start [--bind <IP>] [--port <PORT>] [--assets-dir <DIR>]` | Start a detached daemon and wait until it serves. Fails if one is already running. |
| `memcastle daemon stop` | Ask the running daemon to shut down gracefully. |
| `memcastle daemon restart [--bind <IP>] [--port <PORT>] [--assets-dir <DIR>]` | Stop the running daemon, start a detached new one and wait until it serves. |
| `memcastle status [--json]` | Report whether the daemon is running, where, which palace, and whether the datastore is healthy. |
| `memcastle migrate [--check \| --status]` | Apply, or just inspect, the palace's migrations without a daemon. |
| `memcastle completions <SHELL>` | Print a shell completion script, see [Shell completion](#shell-completion). |

`--bind`, `--port` and `--assets-dir` only exist on `serve`, `daemon start` and `daemon restart`;
client commands find the daemon through its registry file instead.
`--assets-dir` names a directory of runtime assets that outranks the installed and built-in ones,
see [Runtime assets](configuration.md#runtime-assets).
See [Running the daemon](daemon.md) for the details of each, and [Migrations and upgrades](migrations.md) for `migrate`.

### Shell completion

```sh
memcastle completions <SHELL>
```

`<SHELL>` is `bash`, `zsh`, `fish`, `powershell` or `elvish`.
The script goes to standard output, and the command needs neither a daemon nor a working configuration file.
Completion offers subcommands, flags and the values of `--mode` and `jobs list --status`.
It does not complete job ids, which would need a running daemon.
Install it once, for your shell:

```sh
# bash (needs the bash-completion package)
memcastle completions bash > ~/.local/share/bash-completion/completions/memcastle

# zsh: any directory on your $fpath, before compinit runs
memcastle completions zsh > ~/.zfunc/_memcastle

# fish
memcastle completions fish > ~/.config/fish/completions/memcastle.fish

# PowerShell: add this line to your profile
memcastle completions powershell | Out-String | Invoke-Expression
```

The Homebrew formula, the `.deb` and `.rpm` packages and the AUR package install the bash, zsh and fish scripts for you.

### Database access

| Command | What it does |
|---|---|
| `memcastle db start [--bind <IP>] [--port <PORT>] [--allow-remote] [--allow-origin <ORIGIN>]... [--json]` | Ask the running daemon to open its database admin endpoint, then return. Succeeds, reporting the same details, when it is already open. |
| `memcastle db status [--json]` | Report whether the endpoint is open, and where. |
| `memcastle db stop` | Close the endpoint and its connections. |

These are client commands: the endpoint lives in the daemon, the only process that may open the embedded database,
so `db start` returns as soon as it is listening and never starts a second database process.
It listens on `127.0.0.1` port `8000` unless told otherwise, and prints the URL, the namespace and the database to give
SurrealDB Studio.
A non-loopback `--bind` is refused (`memcastle::db::unsafe_bind`) unless `--allow-remote` is given and the daemon has
authentication enabled.
`--allow-origin` may be repeated, and lets a browser page from that origin connect;
pages served from this machine and the SurrealDB Studio desktop app need no flag.
Running `db start` while the endpoint is already open is not an error:
it prints `already running on` the URL instead of `listening on`, followed by the same details, and exits `0`.
Flags that contradict the open endpoint
(another `--bind`, a `--port` other than the one it uses, or an `--allow-origin` it does not allow)
are refused with `memcastle::db::already_running`: run `db stop` first to change them.
`db status` prints the user to sign in with, which is `memcastle`.
Flags left out fall back to the `[db]` [settings](configuration.md#the-database-admin-endpoint).
See [Database access](database-access.md).

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
memcastle repair [--apply [--yes]] [--based-on-job <JOB_ID>]
```

Submits a repair job.
Without `--apply` it is a dry run that only reports what it would remove.
With `--apply` it deletes orphan drawers, which is a write and is refused in `read_only` and `disabled` modes.
In a terminal `--apply` asks for confirmation first, unless `--yes` is given.
`--based-on-job` narrows the repair to what a previous audit found; a live scan still decides what is removed.

## Jobs

```sh
memcastle jobs list [--status <STATUS>]
memcastle jobs show <ID>
memcastle jobs pause <ID>
memcastle jobs resume <ID>
memcastle jobs cancel <ID> [--yes]
memcastle jobs retry <ID>
memcastle jobs demo [--steps <N>]
```

`--status` is one of `queued`, `running`, `paused`, `completed`, `failed` or `cancelled`.
`jobs list` prints a table in a terminal, with the full id (copy it into `jobs show`), the kind, the coloured status,
the progress, when it was created and the detail: the error of a failed job, otherwise the latest progress message.
The detail column is only as wide as its text needs, and wraps onto further lines, never cut, when the terminal is narrower.
The other columns are never wrapped, so on a very narrow terminal the table overflows instead.
When standard output is a pipe or a file it prints the same jobs as a JSON array, so `memcastle jobs list | jq` works
without a flag.
`jobs cancel` asks for confirmation in a terminal, see [Output, colour and prompts](#output-colour-and-prompts).
Pausing and cancelling are requests: a running job stops at its next unit of work, not instantly.
`retry` only applies to a failed job, and `resume` only to a paused one.
`demo` submits a synthetic job (5 steps by default) that touches no palace content,
which is a quick way to check the daemon end to end.
The states a job moves through are in [Architecture](architecture.md#job-lifecycle).

## Authentication

```sh
memcastle auth generate [--yes]
memcastle auth revoke [--yes]
```

`auth generate` asks the daemon to make a high-entropy token, and prints it on standard output, once, and nothing else,
so it pipes straight into a secret manager.
The daemon keeps only a digest, so the token cannot be shown again.
The instructions for enabling authentication go to standard error.
Running it again replaces the previous token, which is how you rotate.
In a terminal both commands ask for confirmation first, on standard error so the token on standard output is unaffected,
and `--yes` skips it.
`auth revoke` removes the stored token, which stops working at once, and prints `{"revoked": true}`.

Both are administrative REST and CLI operations, and neither is available to MCP clients.
While the daemon has authentication disabled they need nothing, which is how the first token is made.
Once it is enabled they need a valid token like every other command.

With authentication enabled, every client command presents the token in `MEMCASTLE_AUTH_TOKEN` (or `auth.token`);
there is deliberately no flag for it, so it stays out of your shell history and the process list.
A daemon that refuses it answers `401` with `memcastle::auth::unauthorized`,
reported as `memcastle::client::remote_rejected`.
[Authentication](authentication.md) covers provisioning, rotation and revocation.

## Not implemented yet

`wings`, `rooms`, `drawers` and `maintenance` are reserved names.
They exist so the command surface is stable, and they return a `not_implemented` error today.
