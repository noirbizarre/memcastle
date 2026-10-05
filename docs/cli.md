# CLI reference

The `memcastle` binary is one daemon plus a set of thin clients.
`serve` runs the daemon in the foreground, `daemon start` runs it in the background,
and `migrate` talks to storage directly.
Every other subcommand is an HTTP call to a running daemon, so it fails with `memcastle::client::not_running`
(and points you at `memcastle daemon start`) when none is running.
Seven things differ:
`status` reports a stopped daemon instead of failing, `daemon restart` starts a daemon when none is running,
`completions` prints a script locally and needs neither a daemon nor a configuration file,
the local `source` commands (`init`, `build`, `test`, `package`, `index` and `keygen`) work on a project directory or on
archives with no daemon at all,
the `integration` commands (`list`, `install`, `update` and `remove`) work on files and the agent's own commands with no
daemon at all,
`note` reads the project directory to choose a wing and room before it calls the daemon,
and the reserved command (see [Not implemented yet](#not-implemented-yet)) fails with `memcastle::cli::not_implemented`
without contacting a daemon.
Run `memcastle <command> --help` for the authoritative text of any flag.
There is no `help` subcommand: `--help` is the one way to ask.

Client commands print the daemon's JSON answer, so the output pipes into `jq`.
`status`, `db start`, `db stop` and `db status` are the exceptions: they print a readable report,
and `--json` gives the same report as JSON (`db stop` prints the report only).
`job list`, `sources`, `note`, every daemon-side `source` command (`search`, `install`, `update`, `list`, `show`,
`enable` and `disable`), and the `wing`, `room` and `drawer` commands
(`list`, `show`, `create` and `delete`, and `drawer history`) are the others:
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

`repair --apply`, `auth generate`, `auth revoke`, `job cancel`, `source remove` and the `delete` commands of `wing`, `room`
and `drawer` ask for confirmation, with a prompt on standard error that defaults to "no".
`--yes` (or `-y`) skips the question.
When standard input or standard error is not a terminal, a script or CI job for instance,
they never ask and proceed, because the caller has already decided.
Declining the prompt exits with `memcastle::cli::aborted` and changes nothing.

`source install` and `source update` are the exception, because what they ask about is code that will run with
permissions: when a package asks for any, they ask in a terminal, and without a terminal they refuse
(`memcastle::source::consent_required`, or exit `1` for `update`) unless `--yes` or `--consent <digest>` is given
(see [`source`](#source)).

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
                 [--as-of <WHEN> | --from <WHEN> --until <WHEN> | --include-historical] [--expand]
```

Search drawer content, lexically, by meaning or both.
`--ranking auto` (the default) is hybrid when the daemon has an [embedding provider](configuration.md#embeddings) and
lexical otherwise.
Lexical matching returns drawers containing every word of the query and, if there are none, drawers containing any of them.
`--limit` defaults to 10 and is capped at 200.
`--wing` restricts results to a wing, `--room` to a room directly, `--tag` (repeatable) to drawers carrying every tag,
and `--source-kind` to `file`, `manual`, `transcript`, `note` or `other`.
Only memory valid now is searched, and four options choose another time (they cannot be combined):
`--as-of` searches what was true at one instant, `--from` with `--until` what was true at some moment of the window
`[from, until)`, and `--include-historical` every version, superseded ones too.
An instant (`<WHEN>`) is an RFC 3339 timestamp such as `2026-01-31T12:00:00Z`, or a date such as `2026-01-31` meaning midnight
UTC at the start of that day.

```sh
memcastle search "database we use" --as-of 2026-01-01
memcastle search "database we use" --from 2026-01-01 --until 2027-01-01
```

Both ends of an interval are required, and `--until` is exclusive and must be after `--from`,
so the second command is exactly 2026.
The rules, and how a boundary behaves, are in [Searching](mcp-and-api.md#searching).
`--expand` appends drawers related to the hits through the knowledge graph.
The option is `--ranking` because `--mode` is the [memory mode](memory-modes.md).
The output is JSON, and the fields of each hit are described in [Searching](mcp-and-api.md#searching).

### `recall`

```sh
memcastle recall <QUERY> [--limit <N>] [--wing <WING>] [--ranking <…>] [--tag <TAG>]... [--source-kind <KIND>]
                 [--as-of <WHEN> | --from <WHEN> --until <WHEN> | --include-historical] [--expand]
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

### `note`

```sh
memcastle note [TEXT]... [--file <PATH>] [--edit] [--wing <WING>] [--room <ROOM>]
```

Captures a thought as it comes, without asking you where it goes.
The note is stored verbatim as a drawer of source kind `note`, so it is found by `search` and `recall`,
embedded, deduplicated and, when an [extraction provider](configuration.md#extraction) is configured,
read for the entities and relationships it names, exactly like any other memory.
There is no separate note store, and no tag or folder to choose.

The text comes from, in order:

1. the words on the command line (several words are joined with a space, so quoting is optional);
1. `--file <PATH>`, where `-` reads standard input;
1. `--edit`, which opens `$VISUAL` (else `$EDITOR`, either may carry arguments such as `code --wait`) on a scratch file,
   starting from the words when you gave some;
1. piped standard input;
1. with none of these and a terminal on standard input, the editor.

Text from a file, standard input or an editor loses its trailing whitespace, since a final newline is how the text was
made and not part of the thought; words given on the command line are kept exactly.
A note with nothing in it is refused.
Text that starts with a dash needs `--` first: `memcastle note -- "- buy milk"`.

The note is filed under the current project, resolved from the directory you run the command in, for the wing and
the room separately:

1. the `--wing` and `--room` flags;
1. `MEMCASTLE_WING` and `MEMCASTLE_ROOM`;
1. the nearest [`.config/memcastle.toml`](project-config.md): `[memcastle] wing` (else `[project] name`) and `room`;
1. the working directory's name for the wing, and `notes` for the room.

With both flags given the project is not read at all.
A project file or variable that cannot be used is an error naming it (`memcastle::project::invalid`),
not a silent fallback, because a wrong scope would file the note somewhere you did not mean.
The rules, including how the project is found, are on the [project configuration](project-config.md) page.

Each note records when it was captured (`created_at` and `valid_from`), the directory it was captured in
(`source.uri`) and the channel it came through (`provenance.requested_by`, `cli`).
Writing the same note into the same room again stores nothing and reports the existing one.

The confirmation is a line in a terminal and JSON when piped, with the drawer's `id`, which is the stable handle for
`memcastle drawer show <wing>/<room>/<id>`:

```sh
memcastle note "ask Ada about the migration order"
# Saved note 0198f4c2-... in memcastle/notes

memcastle note "Standup:" "- ship the CLI" "- write docs"    # words are joined with spaces
git log -5 --oneline | memcastle note                         # piped, longer text
memcastle note --edit                                         # write it in $EDITOR
memcastle note --file draft.md                                # from a file
memcastle note --wing ideas --room inbox "a CLI for tide tables"
MEMCASTLE_WING=release memcastle note "freeze on Friday"     # one-off scope
memcastle note "..." | jq -r .id                              # the stable identifier
```

### `mine`

```sh
memcastle mine <PATH> [--wing <WING>] [--full]
memcastle mine --source <NAME> [--locator <WHERE>] [--wing <WING>] [--full]
```

Submits a job that reads a source into drawers: a directory (one drawer per file, or several for a long one), or a named
source such as `pi` or `opencode`, a coding agent's session history (installed sources: see
[Mining sources](mining-sources.md#pi) and [`opencode`](mining-sources.md#opencode)).
Mining is incremental and idempotent: the daemon remembers where each source's last run stopped,
so mining it again reads only what changed and files nothing twice.
`--wing` defaults to the wing the directory's [project file](project-config.md#mining) declares, else the directory's
name, or to the source's own default.
`--full` reads the source again from the beginning; unchanged documents are still skipped, so nothing is duplicated.
`--locator` names where within a source to read, when it needs more than its default; for `pi` it is a sessions
directory, and it must be an absolute path.
For `opencode` it is only a name for the history (OpenCode decides where its own database is), and is rarely needed.
The command returns the job immediately; follow it with `memcastle job show <id>`.
See [Mining sources](mining-sources.md) for the model,
and [Storage and data](storage.md#what-mining-reads) for which files a directory mine reads.

### `sources`

```sh
memcastle sources
```

Lists the sources the daemon can mine, built in and installed, with each one's state and the permissions an installed
one was given, and, for each one that has been mined, how many documents it holds, when it last ran and which job did.
In a terminal this is two tables; piped, it is JSON with `adapters` and `sources`.
`memcastle source list` is the same command.

### `source`

```sh
memcastle source init <NAME> [--template rust|typescript|python|cli] [--parent <DIR>]
memcastle source build [<PATH>]
memcastle source test [<PATH>] [--no-build]
memcastle source package [<PATH>] [--no-build] [--output <FILE>]
memcastle source keygen <FILE>
memcastle source index <ARCHIVE>... [--output <FILE>] [--base-url <URL>] [--sign <KEYFILE>] [--name <NAME>]

memcastle source search [<QUERY>] [--registry <LOCATION>]
memcastle source install <FILE | DIR | NAME[@VERSION]> [--registry <LOCATION>] [--enable] [--yes | --consent <DIGEST>]
memcastle source update [<NAME>] [--check] [--yes]
memcastle source list
memcastle source show <NAME>
memcastle source enable <NAME>
memcastle source disable <NAME>
memcastle source remove <NAME> [--yes]
```

Develop, publish, find and install mining sources: WebAssembly components that read an origin and hand MemCastle
documents to file.
[Writing a mining source](writing-sources.md) is the guide to making one,
[Publishing and installing sources](publishing-sources.md) the guide to distributing one; this is the reference.

The first six are **local**: they work on a project directory or on archives, need no daemon, no palace and no
configuration file, and never touch the network.
`init` creates `<NAME>` (in the current directory, or in `--parent`) from a template and refuses to write into a
directory that is not empty.
The default template is `rust`; `cli` wraps a command-line program, and `typescript` and `python` need their own
toolchains (see the guide).
`build` runs the command in the manifest's `[build]` section and checks that the result is a component;
the component is placed in `dist/source.wasm`.
`test` builds, then runs the manifest's conformance cases against the component in the same sandbox the daemon uses,
printing `PASS` or `FAIL` for each and exiting non-zero when one fails.
`package` builds, then writes `dist/<name>-<version>.tar.gz`, or `--output`, and prints the archive's SHA-256 (also written
to `<archive>.sha256`), the component's digest and the permissions it asks for.
`--no-build` uses the component already in `dist/`.
`keygen` writes a new ed25519 signing key to `<FILE>`, readable by you alone and never overwriting a file, and prints its
id and the public key users put under `mining.trusted_keys`.
`index` adds the archives to a registry index (`memcastle-index.json`, or `--output`), creating it or extending the one
there.
Each package's URL is `--base-url` plus the file name, or just the file name when the archives will sit beside the index.
`--sign` signs each archive with a key from `keygen`.
Publishing the same archive again refreshes its entry; a different archive under a version already listed is refused.

The rest talk to the daemon, which is the one that reads registries and downloads packages.
`search` lists what the bundled sources and the configured registries offer, whose name or description contains
`<QUERY>`, with the version `install` would take and what is installed already; a registry that cannot be read is a
warning, so it does not hide the others.
`install` takes a package file, a source project directory (it is built and packaged first), or a name,
optionally pinned as `name@1.2.0`, resolved from the bundled sources and then the configured registries in order.
Write `./name` for a path that looks like a name.
`--registry` consults only that location (a URL, or an absolute path) instead of the usual ones, and the trust policy
still applies to what it serves.
`install` shows the permissions of the package it fetched before installing it: in a terminal it asks, and without
one it refuses unless `--yes` agrees to them or `--consent` carries the digest of the permissions you reviewed, so a script
never consents on your behalf.
A package that asks for nothing is installed without asking.
`--enable` turns the source on once installed; otherwise it is `installed` and `memcastle source enable <NAME>` is needed
before it can be mined.
Installing a name that is already installed replaces it and keeps its state.
`update` installs the newest version of the sources that came from the bundle or a registry, from the same place, and
keeps their state; a source installed from a file has no upstream and is left alone.
A version that asks for permissions the installed one did not is not installed until you agree to them (a prompt, or
`--yes`); without either the command says what is waiting and exits non-zero.
`--check` only lists what has an update.
`disable` keeps the files and `remove` deletes them; what a source mined stays in the palace.
`list` and `show` are reads, so they take `--mode` and a `disabled` session cannot use them;
the other commands are administrative: `--mode` is accepted (it is a global flag) but ignored by them,
and no MCP tool exists for any of them.

### `integration`

```sh
memcastle integration list [--json] [--assets-dir <DIR>]
memcastle integration install <AGENT> [--json] [--assets-dir <DIR>]
memcastle integration update <AGENT> [--json] [--assets-dir <DIR>]
memcastle integration remove <AGENT> [--json]
```

Install, update and remove the integrations MemCastle ships for coding agents: `pi` and `opencode`.
[Integrations](integrations.md) is the guide; this is the reference.

All four are **local**: they read files and run the agent's own command, need no daemon, no palace and no network, and are
not available to MCP clients.
`<AGENT>` is an integration id as `list` shows it, which is the agent's name.

`list` shows each shipped integration with the version shipped and the version installed, the agent's version,
and a state: `not installed`, `installed`, `outdated` (the package ships a different version or different files),
`modified` (a file changed, or the agent no longer knows the copy), `incompatible` (your MemCastle or agent is outside the
supported range) or `unavailable` (the shipped files are incomplete).
It ends with the assets root it used and how it was chosen.
`install` copies the integration and the shared skills to `~/.local/share/memcastle/agents/<AGENT>/`, registers them with
the agent, checks the result and reports every change.
It changes nothing, and says so, when the integration is already installed and current.
`update` is `install` for an integration that is already installed, and refuses one that is not.
`remove` unregisters the integration and deletes the copy; it needs no assets, so it works after the package is gone.

`--assets-dir` names the directory that holds `integrations/` and `skills/`, overriding `assets.dir` and
`MEMCASTLE_ASSETS_DIR`: a checkout of the repository, or an unpacked package (see
[Where integrations come from](integrations.md#where-integrations-come-from)).
`--json` prints the report or the outcome as JSON, with the same fields in both.
An error exits non-zero and names a `memcastle::integration::*` code, listed in
[Troubleshooting](integrations.md#troubleshooting).

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
memcastle drawer history <WING>/<ROOM>/<DRAWER>
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
A path that breaks these rules is refused locally, before the daemon is contacted, with `memcastle::palace::path_invalid`.
The same rules apply to a new wing named by `mine --wing`, a checkpoint item or a diary write,
which the daemon refuses at submission with the same code.
A wing that already exists is always accepted, whatever its name.

The `list`, `show`, `create` and `delete` commands of `wing`, `room` and `drawer`, and `drawer history`, print a table
or a readable view in a terminal, and JSON when standard output is a pipe or a file.
`drawer supersede` and `drawer mention` always print JSON.
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
The old drawer keeps its content and stays reachable by id and by `--as-of` or `--from`/`--until` searches, but no longer
appears in a current search, diary read or wake-up, and the two drawers are linked so `drawer history` can follow the change.
`drawer history <wing>/<room>/<name or UUID>` shows how a piece of knowledge evolved:
every version of the drawer's supersession chain, oldest first, each with its validity period, provenance and content.
Any version works as the starting point, and a superseded version has no name left, so give its UUID
(a search hit carries it).
It is a table of versions in a terminal and JSON when standard output is piped, and a read, see
[History](mcp-and-api.md#history).
`drawer mention` records that a drawer mentions an entity, creating the entity if needed, so `search --expand` can reach
related drawers through it.
A name that is only a different spelling of an entity the graph already knows (another case, other punctuation, a recorded
alias or a unique typo) links to that entity instead of creating a second one, see [Deduplication](deduplication.md).
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

### `extract`

```sh
memcastle extract [--wing <WING>]
```

Submits a job that reads every mined drawer and note not yet read and adds the entities and relationships it names to the
knowledge graph, see [Extraction](configuration.md#extraction).
It needs an `[extraction]` provider and is refused with `memcastle::extract::not_configured` without one.
You rarely run it: the daemon queues the same job after a mining job completes, after a note is written and once at
startup, so this is for backfilling after you configure a provider and for forcing a sweep.
It only adds graph records and never changes a drawer, and running it again does nothing.
It is a write, so it is refused in `read_only` and `disabled` modes.
Read the result with `GET /api/entities`, see [the knowledge graph](mcp-and-api.md#the-knowledge-graph).

### `audit`

```sh
memcastle audit [--wing <WING>]
```

Submits a read-only consistency report job.
`--wing` restricts only the embedding counts to one wing; orphan and dangling-reference findings are always palace-wide.
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
