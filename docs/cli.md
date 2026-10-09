# CLI reference

The `memcastle` binary is one daemon plus a set of thin clients.
`serve` runs the daemon in the foreground, `daemon start` runs it in the background,
and `migrate` talks to storage directly.
`doctor` checks local configuration without a daemon and uses read-only HTTP requests for runtime checks when one is available.
Most other subcommands are HTTP calls to a running daemon, so they fail with `memcastle::client::not_running`
(and points you at `memcastle daemon start`) when none is running.
Other commands differ:
`status` reports a stopped daemon instead of failing, `doctor` reports offline checks without failing,
`daemon restart` starts a daemon when none is running,
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

Every command that has an answer to give follows one rule:
it prints a readable rendering when standard output is a terminal, and JSON when it is not (or when `--json` is given).
`memcastle mine .` in a terminal prints the queued job and how to follow it,
and `memcastle mine . | jq .id` prints the job as JSON.
See [Output, colour and prompts](#output-colour-and-prompts).

## Output, colour and prompts

What the CLI shows depends on where its output goes, so a script needs no flag:
a terminal gets a readable rendering with colour, and a pipe or a file gets plain JSON.
The readable rendering is not meant to be parsed and may change between releases, whereas the JSON is the contract.
`--json` forces the JSON form on a terminal, for `memcastle search auth --json | jq` or to see the same data a script
would receive.
On a colour-capable terminal JSON is indented and syntax-highlighted; `NO_COLOR` leaves it indented but plain.
In a pipe or a file JSON stays indented and free of escape codes, even with `FORCE_COLOR`.
It is a global flag, accepted before or after any subcommand, and it never changes a command's exit code.

This applies to `status`, `doctor`, `db`, `integration`, `migrate`, `daemon stop`, `mine`
and the other commands that queue a job
(`audit`, `embed`, `extract`, `repair`, `checkpoint`), `job` (all subcommands), `search`, `recall`, `wake-up`, `diary`,
`note`, `sources`, `miner`, every daemon-side `source` command, and the `wing`, `room` and `drawer` commands.
Four groups of commands have no answer to render and print the same text on every stream:

- `daemon start` and `daemon restart` print the `started:` or `restarted:` line.
- The local `source` commands (`init`, `build`, `test`, `package`, `index` and `keygen`) print their progress.
- `auth generate` prints the token and nothing else, so it can be captured.
  A secret is never put in JSON.
- `completions` prints a script.

`--json` is accepted on these too and changes nothing.

| Setting | Effect |
|---|---|
| `NO_COLOR` set to anything | No colour anywhere, even on a terminal. |
| `FORCE_COLOR=1` | Colour even in piped text (help, diagnostics and progress), but never in piped JSON. |
| `FORCE_COLOR=0` | Disable colour. |
| `TERM=dumb` | Disable automatic colour detection; `FORCE_COLOR=1` overrides it. |

Colour is added around words and never replaces them.
It colours `--help`, `status` and `db status` reports (healthy in green, degraded or unavailable in red,
things that need attention in yellow), the table of `job list`, the card of a job, and the diagnostics printed on failure.
JSON is also highlighted when requested on a terminal; piped JSON never receives escape codes, even with `FORCE_COLOR=1`.
`NO_COLOR` takes precedence when both colour variables are set.
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
(`memcastle::source::consent_required`, or exit `1` for `update`) unless `--yes` is given.
For an unattended install, `source install --consent <digest>` agrees only to the displayed permissions digest;
`source update` accepts `--yes` but not `--consent` (see [`source`](#source)).

## Global flags

These flags are accepted by every subcommand, before or after its name.

| Flag | Environment variable | Meaning |
|---|---|---|
| `-v`, `-vv` | none | Log MemCastle at `debug` (`-v`) or `trace` (`-vv`), and print the full cause chain of an error. |
| `--json` | none | Print JSON even on a terminal, see [Output, colour and prompts](#output-colour-and-prompts). |
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
| `memcastle status` | Report whether the daemon is running, where, which palace, and whether the datastore is healthy. |
| `memcastle tui` | Open the interactive operations console for mining jobs and maintenance; see [Terminal operations console](tui.md). |
| `memcastle doctor` | Diagnose configuration, local prerequisites, and available daemon/runtime state without changing anything. |
| `memcastle migrate [--check \| --status]` | Apply, or just inspect, the palace's migrations without a daemon. |
| `memcastle completions <SHELL>` | Print a shell completion script, see [Shell completion](#shell-completion). |

`--bind`, `--port` and `--assets-dir` only exist on `serve`, `daemon start` and `daemon restart`;
client commands find the daemon through its registry file instead.
`--assets-dir` names a directory of runtime assets that outranks the installed and built-in ones,
see [Runtime assets](configuration.md#runtime-assets).
`tui` requires interactive standard input and output and refuses `--json`;
the ordinary CLI commands continue to provide JSON for scripts.
See [Running the daemon](daemon.md) for the details of each, and [Migrations and upgrades](migrations.md) for `migrate`.

### Doctor

Run `memcastle doctor` to check the effective configuration (defaults, file, environment and CLI overrides),
unknown file keys, directory prerequisites, configured providers, enabled miner and trigger prerequisites,
and the running daemon's storage and migration state when it answers.
An absent default config file is normal; an explicitly named missing config file is an error.
Unknown TOML keys are warnings, because normal loading currently ignores them.
Checks that depend on invalid configuration or a stopped daemon are marked `SKIPPED`.
A stopped daemon is a warning: offline checks can succeed without starting one.
The command never initializes a palace, opens the database, executes a provider, contacts a provider/model or source registry,
or writes to the filesystem; actual write access and provider/model availability remain unverified.
It prints no credential values or raw sensitive settings.

Example terminal output (a new palace with no daemon):

```text
MemCastle doctor

Configuration
  [OK] file: No default file; built-in defaults apply.
  [OK] effective: Effective configuration passes validation.

Paths
  [SKIPPED] palace.path: Optional directory does not exist yet.
  [SKIPPED] embedded database: Optional directory does not exist yet.
  [SKIPPED] mining.sources_dir: Optional directory does not exist yet.
  [SKIPPED] write access: Write access is not tested without creating files.

Providers
  [OK] embeddings: No external provider is selected.
  [OK] extraction: No external provider is selected.

Daemon and storage
  [WARNING] daemon: No daemon is reachable for this palace.
        Fix: Start it with `memcastle daemon start` to run online checks.
  [SKIPPED] storage and migrations: Requires an authenticated running daemon.

Sources and miners
  [SKIPPED] runtime prerequisites: Requires an authenticated running daemon.
```

`memcastle doctor --json` (also the default when piped) returns an object with `findings` in presentation order.
An excerpt showing one finding:

```json
{"findings":[{"area":"Daemon and storage","check":"daemon","status":"warning","summary":"No daemon is reachable for this palace.","remediation":"Start it with `memcastle daemon start` to run online checks."}]}
```

Each finding has `area`, `check`, `status` (`ok`, `warning`, `error`, `skipped`), `summary`,
and an optional `remediation`.
Exit status is `0` when there are no blocking `error` findings (warnings and skipped checks alone do not fail),
or `1` when at least one check is an error; the full report is printed on standard output either way.

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
| `memcastle db start [--bind <IP>] [--port <PORT>] [--allow-remote] [--allow-origin <ORIGIN>]...` | Ask the running daemon to open its database admin endpoint, then return. Succeeds, reporting the same details, when it is already open. |
| `memcastle db status` | Report whether the endpoint is open, and where. |
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
it prints `already running on` the URL instead of `listening on`, followed by the same details, and exits `0`
(as JSON, `already_running` is `true`).
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
In a terminal each hit is a numbered block with its score, where it came from and the first lines of its content;
the full content is in `memcastle drawer show` and in the JSON.
Piped, or with `--json`, the output is JSON, and the fields of each hit are described in
[Searching](mcp-and-api.md#searching).

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
memcastle mine <SOURCE> [PLACE] [KEY=VALUE]... [--wing <WING>] [--full]
memcastle mine <PATH> [KEY=VALUE]... [--wing <WING>] [--full]
```

```sh
memcastle mine directory /some/path
memcastle mine /some/path                                        # the same: a path is the directory shorthand
memcastle mine opencode since=2026-09 dir=/path/to/workspace
memcastle mine pi /backups/pi/sessions since=2026-09-14 --full
```

Submits a job that reads a source into drawers: a directory (one drawer per file, or several for a long one), or a named
source such as `pi` or `opencode`, a coding agent's session history (installed sources: see
[Mining sources](mining-sources.md#pi) and [`opencode`](mining-sources.md#opencode)).
The first word says what to read.
`directory` is the built-in source and takes the path to read as its one `PLACE`.
Any other word is the name of a source (`memcastle sources` lists them with the options each accepts).
A word that cannot be a source name is always a directory: one with a `/`, one that starts with `.` or `~`, or one with
a capital letter, so `memcastle mine .` and `memcastle mine ~/project` keep working as the shorthand for
`memcastle mine directory <path>`.
A plain word that is not a source but is a directory in the current directory is a directory too, as it always was;
a source of that name wins, so write `./pi` for a directory called `pi`.
Anything else is refused with the list of sources.
After it come at most one bare word, the `PLACE` to read within the source (`pi`'s sessions directory; `directory`'s
path), and any number of `KEY=VALUE` options in any order.
A word is an option when its key is lowercase letters, digits, `-` or `_` and starts with a letter,
so a path that contains an `=` (`./a=b`) is still a path.
An option's key may be given once.
Each source declares the options it accepts, and the daemon refuses any other key before it queues the job,
naming the ones it accepts:

| Source | Option | Meaning |
| --- | --- | --- |
| `directory` | `since=DATE` | Only files modified at or after `DATE`. |
| `pi` | `since=DATE` | Only sessions modified at or after `DATE`. |
| `pi` | `dir=PATH` | Only sessions started in this working directory, or in directories matching a pattern. |
| `opencode` | `since=DATE` | Only sessions updated at or after `DATE`. |
| `opencode` | `dir=PATH` | Only sessions of this project directory, or of directories matching a pattern. |

`DATE` is `2026-09` (the first of the month), `2026-09-14` or an RFC 3339 time, in UTC.
A `dir` selects a different slice of the same history, so each directory has a cursor of its own;
`since` only narrows, so it shares the source's cursor, and `--full` is how to read back past it.
A `path` option is made absolute against the shell's working directory before the daemon sees it, like the path of `directory`.

A `dir` is a directory or a pattern in which `*` matches any run of characters, `/` included:
`dir=/work/app` is one project and `'dir=/work/*'` is every project under `/work`, however deeply nested.
Quote a pattern, because a shell expands an unquoted star before `memcastle` sees it (zsh refuses with "no matches found").
It is compared the way the tools record a working directory, so how it is spelled does not matter:
the CLI makes it absolute, removes `.`, `..` and trailing or doubled slashes, and resolves links in the part that exists
(the part before the first `*` in a pattern), and the source applies the same clean-up to the value it compares with.
Only `*` is a wildcard; `?` and `[` are ordinary characters.
Mining is incremental and idempotent: the daemon remembers where each source's last run stopped,
so mining it again reads only what changed and files nothing twice.
`--wing` defaults to the wing the directory's [project file](project-config.md#mining) declares, else the directory's
name, or to the source's own default.
`--full` reads the source again from the beginning; unchanged documents are still skipped, so nothing is duplicated.
The `PLACE` names where within a source to read, when it needs more than its default; for `pi` it is a sessions
directory.
It is made absolute for a source that reads it as a directory.
For `opencode` it is only a name for the history (OpenCode decides where its own database is), and is rarely needed.
The `--source` and `--locator` flags of earlier versions are gone: the source is the first word and the locator is the `PLACE`.
The command returns the job immediately; follow it with `memcastle job show <id>`.
In a terminal it prints the queued job as a short card (kind, status, source, wing, progress) and that hint;
piped, or with `--json`, it prints the job as JSON, whose `id` is what `job show` takes.
The other commands that queue a job (`audit`, `embed`, `extract`, `repair`, `checkpoint` and `job demo`)
print the same way.
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
memcastle source auth <NAME>
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
`search` lists what the configured registries offer (the [official one](publishing-sources.md#the-official-registry)
unless `mining.registries` says otherwise), whose name or description contains `<QUERY>`, with the version `install` would
take and what is installed already, bundled sources included; a registry that cannot be read is a warning, so it does not
hide the others.
`install` takes a package file, a source project directory (it is built and packaged first), or a name,
optionally pinned as `name@1.2.0`, resolved from the configured registries in order.
A name the release already ships (`pi`, `opencode`) is installed from the start: `install` refuses it with
`memcastle::source::bundled`, and `memcastle source enable <NAME>` is what it needs.
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
`update` installs the newest version of the sources that came from a registry, from the same place, and keeps their
state; a source installed from a file has no upstream and is left alone, and a bundled source is updated with MemCastle
(`update <NAME>` for one is refused with `memcastle::source::bundled`, and a bare `update` skips it).
A version that asks for permissions the installed one did not is not installed until you agree to them (a prompt, or
`--yes`); without either the command says what is waiting and exits non-zero.
`--check` only lists what has an update.
`enable` and `disable` turn a source on or off, and a bundled source needs no consent to be enabled: it ships with
MemCastle, and `show` prints what it may do.
`disable` keeps the files and `remove` deletes them, along with the source's stored credentials; a bundled source cannot
be removed, only disabled.
What a source mined stays in the palace.
`list` and `show` are reads, so they take `--mode` and a `disabled` session cannot use them;
the other commands are administrative: `--mode` is accepted (it is a global flag) but ignored by them,
and no MCP tool exists for any of them.

#### `source auth`

```sh
memcastle source auth <NAME>
```

Signs an installed source in with OAuth, for a source that cannot be reached with a static token.
Only a source whose manifest declares `[permissions.oauth]` ([Writing a mining source](writing-sources.md#signing-in-with-oauth))
can be signed in, and `source show` and `source list` say which are, and whether they are.
It is not `memcastle auth`, which manages the daemon's own bearer token.

The daemon runs the flow and keeps the result, so this command only tells you what to do and waits.
A source that declares a device authorization endpoint uses the device flow:
the command prints a page and a code, you open the page on any device and type the code,
and the command returns when you have finished.
Otherwise it uses the browser flow with PKCE: the command opens your browser (or prints the address when it cannot),
you agree, and the provider redirects to a port the daemon opened on its own machine,
so the browser must be on the machine the daemon runs on.
In a terminal the browser is opened for you; without one, the instructions are printed and nothing is opened.
Instructions and progress go to standard error, so standard output holds only the result, which carries no token:
`{"source", "signed_in", "expires_at", "scopes", "stored_in"}`, where `stored_in` is `keyring` or `file`.
The command waits up to ten minutes (the device code's own lifetime when the provider sets a shorter one) and fails with
`memcastle::credential::flow_failed` when you decline, the code expires, or the provider refuses.

There is no `login`, `logout` or `status` subcommand: the source name is enough to choose the flow,
`source show` is the status, and the daemon renews the credential itself.
Run `source auth` again to sign in as someone else, which replaces the stored credential and any sign-in still waiting.
`source remove` forgets the credential, and so does a provider that revokes it, in which case the next run fails with
`memcastle::credential::required` and the command to run.
The tokens are kept in the platform credential store when there is one, and in an owner-only file otherwise
(see [Credentials](configuration.md#credentials)).
Like installing a source, it is administrative: `--mode` is ignored and no MCP tool exists for it.

### `miner`

```sh
memcastle miner list
memcastle miner get <NAME>
memcastle miner set <NAME> [--source <SOURCE>] [--locator <WHERE>] [--wing <WING>]
                           [--credential-env <VAR> | --credential-file <PATH> | --credential-oauth]
                           [--option <KEY=VALUE>]... [--unset-option <KEY>]...
                           [--unset locator|wing|credential]... [--disabled] [--allow-broaden]
memcastle miner enable <NAME>
memcastle miner disable <NAME>
memcastle miner remove <NAME> [--yes]
memcastle miner reload
memcastle miner run <NAME> [KEY=VALUE]... [--full] [--allow-broaden]
```

Manages the `[[miners]]` of the configuration file: named, persistent definitions of what to mine,
described in [Configuration](configuration.md#miners).
Every command is a call to the daemon's `/api/miners` routes, which are the rules MCP's read-only
`memcastle_miner_list` and `memcastle_miner_get` follow too, so there is one model.

`set` creates the miner when there is none (it needs `--source`), and otherwise changes only what it is given:
everything not named stays as it is, and a repeated command changes nothing.
`--option groups=MemCastle,Ops` sets a saved source option; `--option window=30` takes JSON when the value parses as
JSON, and a string otherwise.
A field is cleared by name with `--unset`, and an option key with `--unset-option`.
`--disabled` creates the miner switched off, or switches it off.
An enabled miner is checked before anything is written (its source usable, its credential resolving, and for
`directory` an absolute `--locator`), and a change that might expand what is mined is refused unless `--allow-broaden` is
given.
`--credential-env` and `--credential-file` say where the credential is read from; the secret itself is never an argument.
`--credential-oauth` says the source signs in with OAuth, so the daemon uses the sign-in `source auth` made;
it is refused for a source that does not declare one.
`miner run NAME key=value` replaces or adds that source option for this run only; the saved definition is unchanged.
Like `mine`, a declared `path` option is resolved on the CLI machine before it reaches the daemon.
If the override might mine more than the saved options, repeat with `--allow-broaden`.
A miner for a source that signs in is not ready until the source is signed in, whatever its `credential` says, and the
reason names `memcastle source auth <source>`.
The file is edited in place: its comments and the other tables are kept.
`enable` and `disable` are `set` for the enabled state alone, and `enable` runs the same checks.
`remove` deletes the definition and asks first in a terminal, `--yes` skips it; what the miner mined and its source's
cursor stay.
`reload` reads the file again now and says what changed; the daemon also notices an edited file on its own.
`run` submits the miner's mining job and prints it, like `mine`, continuing from the cursor its source has;
it refuses a disabled miner.
The miner's saved options are passed to the source as the run's options,
exactly as `memcastle mine <source> key=value` would:
a scalar is its text, a list of strings is comma-joined,
a key the source does not declare makes the miner not runnable, and a nested table is refused.

`list` and `get` are reads, so they take `--mode` and a `disabled` session cannot use them, and `run` is a write;
the others are administrative: `--mode` is accepted (it is a global flag) but ignored by them,
and no MCP tool exists for any of them.
`miner set` and the rest print the miner as it is now when standard output is a terminal, and JSON when it is not.

### `trigger`

```sh
memcastle trigger list
memcastle trigger get <NAME>
memcastle trigger set <NAME> [--miner <MINER>] [--type schedule|poll|webhook|watch]
                             [--credential-env <VAR> | --credential-file <PATH>]
                             [--setting <KEY=VALUE>]... [--unset-setting <KEY>]...
                             [--unset credential] [--enable]
memcastle trigger enable <NAME>
memcastle trigger disable <NAME>
memcastle trigger remove <NAME> [--yes]
memcastle trigger reload
memcastle trigger fire <NAME>
```

Manages the `[[triggers]]` of the configuration file: what asks for a mining run on its own, described on the
[Triggers](triggers.md) page ([ADR-043](adr/043-source-triggers.md)).
A trigger only decides *when*; it asks for the same run `miner run` does.
Every command is a call to the daemon's `/api/triggers` routes, the same rules MCP's read-only `memcastle_trigger_list`
and `memcastle_trigger_get` follow.

`set` creates a trigger when there is none (it needs `--miner` and `--type`), and otherwise changes only what it is
given.
**A trigger is created disabled**, and `set` never enables one unless `--enable` is given, so writing a definition starts
nothing.
`--setting every=1d`, `--setting at=03:30`, `--setting path=/notes` and `--setting debounce=2s` take JSON when the value
parses as JSON and a string otherwise.
A webhook's shared secret is a reference, `--credential-env` or `--credential-file`, never an argument.
Changing `--type` drops the settings of the old type.
`enable` checks every prerequisite first (the miner exists and can run, the source supports the type, the webhook
listener is on and its secret resolves, the watched path exists), and when one is missing it is refused with
`memcastle::trigger::not_activatable`, says what to set up, and writes nothing.
`get` shows the same list under `to enable:` for a disabled trigger.
`disable` stops it and keeps what it did; `remove` deletes the definition and what the daemon remembered about it,
and asks first in a terminal (`--yes` skips it).
`reload` reads the file again now; the daemon also notices an edited file within a few seconds.
`fire` asks for a run through the trigger now, the way it would on its own (joining a run already waiting); it is
refused while the trigger is disabled, and is a write, so `--mode read_only` is refused.

`list` and `get` are reads and `fire` is a write; the others are administrative like `miner`'s, and no MCP tool exists
for any of them.
Output is the trigger as it is now in a terminal, and JSON when standard output is not one.

### `integration`

```sh
memcastle integration list [--assets-dir <DIR>]
memcastle integration install <AGENT> [--assets-dir <DIR>]
memcastle integration update <AGENT> [--assets-dir <DIR>]
memcastle integration remove <AGENT>
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
`install` copies the integration and the skills its manifest names to `~/.local/share/memcastle/agents/<AGENT>/`,
registers it with the agent, checks the result and reports every change.
`list --json` carries the skills each integration exposes, by name, in `skills`.
It changes nothing, and says so, when the integration is already installed and current.
`update` is `install` for an integration that is already installed, and refuses one that is not.
`remove` unregisters the integration and deletes the copy; it needs no assets, so it works after the package is gone.

`--assets-dir` names the directory that holds `integrations/` and `skills/`, overriding `assets.dir` and
`MEMCASTLE_ASSETS_DIR`: a checkout of the repository, or an unpacked package (see
[Where integrations come from](integrations.md#where-integrations-come-from)).
A terminal gets the report or the outcome as text, and a pipe (or `--json`) gets it as JSON, with the same fields in both.
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

### `fact history`

```sh
memcastle fact history <RELATIONSHIP_UUID> [--as-of <RFC3339-OR-DATE>]
```

Reads a graph assertion and its linked confirmations, contradictions and explicit corrections from the daemon.
The result includes the validity interval, evidence, lifecycle state, decision reasons and any ranking preference among
unresolved claims; both sides of a conflict remain visible.
`--as-of` evaluates that state at the given instant, while history still preserves the supporting assertions.
Output is readable at a terminal and JSON when piped or when `--json` is set.
See [the knowledge graph](mcp-and-api.md#the-knowledge-graph) for the shared REST and MCP contract.

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
`job show` prints one job as a card in a terminal (its parameters, progress, timestamps, and its error or result when it
has one) and as JSON otherwise.
`pause`, `resume`, `cancel` and `retry` print one sentence in a terminal and the daemon's answer as JSON otherwise.
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
