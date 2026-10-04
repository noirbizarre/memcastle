# CLI reference

The `memcastle` binary is one daemon plus a set of thin clients.
`serve` runs the daemon in the foreground, `daemon start` runs it in the background,
and `migrate` talks to storage directly.
Every other subcommand is an HTTP call to a running daemon, so it fails with `memcastle::client::not_running`
(and points you at `memcastle daemon start`) when none is running.
Four things differ:
`status` reports a stopped daemon instead of failing, `daemon restart` starts a daemon when none is running,
`completions` prints a script locally and needs neither a daemon nor a configuration file,
and the reserved command (see [Not implemented yet](#not-implemented-yet)) fails with `memcastle::cli::not_implemented`
without contacting a daemon.
Run `memcastle <command> --help` for the authoritative text of any flag.
There is no `help` subcommand: `--help` is the one way to ask.

Client commands print the daemon's JSON answer, so the output pipes into `jq`.
`status`, `db start` and `db status` are the exceptions: they print a readable report,
and `--json` gives the same report as JSON.
`job list`, and every `wing`, `room` and `drawer` command (`list`, `show`, `create` and `delete`), are the others:
they print a table or a readable view when standard output is a terminal, and JSON when it is not.
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
things that need attention in yellow), the table of `job list`, and the diagnostics printed on failure.
A job status has the same colour wherever it is shown:
queued yellow, running cyan, paused magenta, completed green, failed red and cancelled dim.
Standard output and standard error are decided separately:
with `memcastle status 2> errors.log` the report stays coloured and the log stays plain.

`repair --apply`, `auth generate`, `auth revoke`, `job cancel` and the `delete` commands of `wing`, `room` and `drawer`
ask for confirmation, with a prompt on standard error that defaults to "no".
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
Completion offers subcommands, flags and the values of `--mode` and `job list --status`.
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
                 [--ranking <auto|lexical|semantic|hybrid>] [--tag <TAG>]... [--source-kind <KIND>]
                 [--as-of <TIMESTAMP> | --include-historical] [--expand]
```

Search drawer content, lexically, by meaning or both.
`--ranking auto` (the default) is hybrid when the daemon has an [embedding provider](configuration.md#embeddings) and
lexical otherwise.
Lexical matching returns drawers containing every word of the query and, if there are none, drawers containing any of them.
`--limit` defaults to 10 and is capped at 200.
`--wing` restricts results to a wing, `--room` to a room directly, `--tag` (repeatable) to drawers carrying every tag,
and `--source-kind` to `file`, `manual`, `transcript` or `other`.
Only memory valid now is searched unless `--as-of` names an instant (RFC 3339, such as `2026-01-31T12:00:00Z`) or
`--include-historical` adds superseded memory.
`--expand` appends drawers related to the hits through the knowledge graph.
The option is `--ranking` because `--mode` is the [memory mode](memory-modes.md).
The output is JSON, and the fields of each hit are described in [Searching](mcp-and-api.md#searching).

### `recall`

```sh
memcastle recall <QUERY> [--limit <N>] [--wing <WING>] [--ranking <…>] [--tag <TAG>]... [--source-kind <KIND>]
                 [--as-of <TIMESTAMP> | --include-historical] [--expand]
```

The recall-oriented counterpart to `search`.
It returns matching drawers verbatim, with the same options and limit rules, except that it has no `--room`.

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
memcastle mine <PATH> [--wing <WING>] [--full]
memcastle mine --source <NAME> [--locator <WHERE>] [--wing <WING>] [--full]
```

Submits a job that reads a source into drawers: a directory (one drawer per file, or several for a long one), or a named
source such as `pi-sessions`, the Pi coding agent's session history.
Mining is incremental and idempotent: the daemon remembers where each source's last run stopped,
so mining it again reads only what changed and files nothing twice.
`--wing` defaults to the directory's name, or to the source's own default.
`--full` reads the source again from the beginning; unchanged documents are still skipped, so nothing is duplicated.
`--locator` names where within a source to read, when it needs more than its default; for `pi-sessions` it is a sessions
directory, and it must be an absolute path.
The command returns the job immediately; follow it with `memcastle job show <id>`.
See [Mining sources](mining-sources.md) for the model,
and [Storage and data](storage.md#what-mining-reads) for which files a directory mine reads.

### `sources`

```sh
memcastle sources
```

Lists the sources the daemon can mine and, for each one that has been mined, how many documents it holds, when it last
ran and which job did.
In a terminal this is two tables; piped, it is JSON with `providers` and `sources`.

### `checkpoint`

```sh
memcastle checkpoint [--payload <FILE>] [--emergency]
```

Submits a checkpoint job that durably persists an already-classified batch of memory writes.
The payload is JSON read from `--payload`, or from standard input when omitted;
its shape is described in [MCP tools and REST API](mcp-and-api.md#checkpoint-payload).
`--emergency` raises the job to the highest priority, for save-before-crash situations only.

## Wings, rooms and drawers

```sh
memcastle wing list
memcastle wing show <WING>
memcastle wing create <WING> [--description <TEXT>]
memcastle wing delete <WING> [--yes]

memcastle room list [--wing <WING>]
memcastle room show <WING>/<ROOM>
memcastle room create <WING>/<ROOM> [--description <TEXT>]
memcastle room delete <WING>/<ROOM> [--yes]

memcastle drawer list --room <WING>/<ROOM> [--limit <N>]
memcastle drawer show <WING>/<ROOM>/<DRAWER>
memcastle drawer create <WING>/<ROOM>/<NAME> [--content <TEXT> | --file <PATH>]
memcastle drawer supersede <WING>/<ROOM>/<DRAWER> (--content <TEXT> | --file <PATH> | --invalidate)
memcastle drawer mention <WING>/<ROOM>/<DRAWER> --name <NAME> --kind <KIND>
memcastle drawer delete <WING>/<ROOM>/<DRAWER> [--yes]
```

These manage the palace hierarchy, see [Storage and data](storage.md#the-data-model) for what the three levels are.
`wings`, `rooms` and `drawers` are aliases of the singular groups.

A wing or room is addressed by its name or its UUID.
A drawer is addressed by its name or its UUID within its room, so `work/project-x/context` and
`work/project-x/<uuid>` are the same drawer when it is named `context`.
A drawer's name may itself contain `/` (a mined file is named after its path, as in `files/src/main.rs`),
which is why everything after the second `/` is the drawer.
A name cannot be empty, cannot have leading or trailing whitespace, cannot contain control characters
and cannot look like a UUID.
A wing or room name cannot contain `/`, and no `/`-separated segment of a drawer name can be empty, `.` or `..`.
A path that breaks these rules is refused locally, before the daemon is contacted, with `memcastle::palace::invalid_path`.
The same rules apply to a new wing named by `mine --wing`, a checkpoint item or a diary write,
which the daemon refuses at submission with the same code.
A wing that already exists is always accepted, whatever its name.

Every `wing`, `room` and `drawer` command prints a table or a readable view in a terminal,
and JSON when standard output is a pipe or a file.
`wing show` prints the wing's totals and its rooms.
`drawer list` shows the newest drawers first with a preview of each, never the whole content,
and `drawer show` prints the content verbatim after a few lines of metadata.
`room list` without `--wing` lists the rooms of every wing.

`create` is idempotent for wings and rooms: creating one that exists succeeds and changes nothing,
and the JSON answer (what a pipe gets) says `"created": false`.
`room create` and `drawer create` also create the wing, and the room, when they do not exist yet,
as mining, checkpoint and diary writes do.
Content is immutable, so `drawer create` can only conflict on the name:
writing a name again with the same content is a no-op, and with other content it is refused with
`memcastle::palace::drawer_name_taken`.
The content comes from `--content`, from `--file` (`-` is standard input), or from standard input when neither is given.

`delete` removes the record and everything under it, permanently:
a wing takes its rooms and their drawers, a room takes its drawers.
In a terminal it first prints what is about to go, then asks:

```text
Wing: work
Rooms: 12
Drawers: 37

Delete this wing and all contained data? [y/N]
```

`--yes` skips the question, see [Output, colour and prompts](#output-colour-and-prompts).
Without a terminal it proceeds without asking, like every other command that confirms.
Deleting a wing or room is refused with `memcastle::palace::busy` while a mining job, a checkpoint job or a repair job
that applies is queued, running or paused, because such a job files into wings and rooms by name and would bring the
wing back.
Cancel the job or wait for it, then try again.
The check is coarse on purpose and is not atomic with the delete:
a job submitted in the instant between the two is not caught.

`drawer supersede` is how a drawer is corrected without rewriting history.
It ends the drawer's validity now and files a replacement with the new content (from `--content`, `--file` or standard
input) in the same room, taking over the old drawer's name; with `--invalidate` it only ends it.
The old drawer keeps its content and stays reachable by id and by `--as-of` searches, but no longer appears in a current
search, diary read or wake-up.
`drawer mention` records that a drawer mentions an entity, creating the entity if needed, so `search --expand` can reach
related drawers through it.
Both are writes.

There is no MCP tool for any of this, see [MCP tools and REST API](mcp-and-api.md#wings-rooms-and-drawers).
Looking is gated as a read and changing as a write, see [Memory modes](memory-modes.md).

## Maintenance

### `embed`

```sh
memcastle embed [--wing <WING>]
```

Submits a job that computes the embedding of every drawer that has none, so semantic search covers it.
It needs an [`[embeddings]` provider](configuration.md#embeddings) and is refused with `memcastle::embed::not_configured`
without one.
You rarely run it: the daemon queues the same job after anything that writes drawers, and once at startup, so this is for
backfilling after you configure a provider and for forcing a sweep.
It only fills each drawer's embedding and never changes its content, and running it again does nothing.
It is a write, so it is refused in `read_only` and `disabled` modes.

### `audit`

```sh
memcastle audit [--scope <WING>]
```

Submits a read-only consistency report job.
`--scope` restricts only the embedding counts to one wing; orphan and dangling-reference findings are always palace-wide.
Read the report with `memcastle job show <id>`.

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
memcastle job list [--status <STATUS>]
memcastle job show <ID>
memcastle job pause <ID>
memcastle job resume <ID>
memcastle job cancel <ID> [--yes]
memcastle job retry <ID>
memcastle job demo [--steps <N>]
```

`jobs` is an alias of the `job` group, like `wings`, `rooms` and `drawers` for the hierarchy commands.

`--status` is one of `queued`, `running`, `paused`, `completed`, `failed` or `cancelled`.
`job list` prints a table in a terminal, with the full id (copy it into `job show`), the kind, the coloured status,
the progress, when it was created and the detail: the error of a failed job, otherwise the latest progress message.
The detail column is only as wide as its text needs, and wraps onto further lines, never cut, when the terminal is narrower.
The other columns are never wrapped, so on a very narrow terminal the table overflows instead.
When standard output is a pipe or a file it prints the same jobs as a JSON array, so `memcastle job list | jq` works
without a flag.
`job cancel` asks for confirmation in a terminal, see [Output, colour and prompts](#output-colour-and-prompts).
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

`maintenance` is a reserved name.
It exists so the command surface is stable, and it returns a `not_implemented` error today.
