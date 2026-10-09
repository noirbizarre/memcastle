# Troubleshooting

Start with `memcastle doctor` to inspect configuration and local prerequisites without changing the palace.
When the daemon is running, it also checks storage, migrations and configured source/miner/trigger readiness over HTTP.
See the [doctor command](cli.md#doctor) for statuses, offline behavior and exit codes.

Every MemCastle failure carries a diagnostic code of the form `memcastle::<module>::<kind>` and a `help:` line saying
what to do.
Codes are stable, so it is safe to search for or match on them.
When the daemon rejects a CLI request, the CLI reports it as `memcastle::client::remote_rejected`
and shows the daemon's own code in parentheses, such as `(403, memcastle::mode::forbidden)`;
that inner code is the one to look up below.
Add `-v` for the full cause chain of an error, and see [Logging](daemon.md#logging) for the daemon's own log.

For daemon-specific details, `memcastle status` says whether it is running, where, and whether its datastore is healthy.

## The daemon

### `memcastle::client::not_running`

No daemon answered.
Start one with `memcastle daemon start`, or `memcastle serve` to run it in the foreground.
If you believe one is running, it may serve a different palace or port:
client commands look it up by palace, so pass the same `--palace` (or set the same `XDG_DATA_HOME`) you started it with,
and check `memcastle status`, which prints the endpoint it tried and the registry file's state.

### `memcastle::server::bind_failed`

The daemon could not listen on its address, usually because something already uses the port
(possibly another MemCastle).
Nothing was created or migrated.
Stop the other process, or choose a free port with `--port`, `MEMCASTLE_PORT` or `server.port`.
A non-loopback `--bind` may also need privileges or an address that exists on this machine.

### `memcastle::store::backend_failed`

The palace could not be opened.
For an embedded palace this almost always means another process holds it:
`memcastle migrate` while the daemon runs, or a second `serve` on the same palace and a different port
(on the same port it fails earlier, with `memcastle::server::bind_failed`).
Check `memcastle status`, stop that process, and try again.
For a remote palace, check the URL and credentials in `[store]`.

### `memcastle::auth::unauthorized`

The daemon has [authentication](authentication.md) enabled and did not accept the token.
The CLI shows it as `memcastle::client::remote_rejected` with `(401, memcastle::auth::unauthorized)`,
and `memcastle status` exits `1`, which is how it differs from a daemon that is not running (`3`).
Export the token as `MEMCASTLE_AUTH_TOKEN` (or set `auth.token`) in the environment of the command;
an MCP client needs an `Authorization: Bearer` header.
A token that was rotated or revoked stops working at once, and the message does not say which case it is.

### `memcastle::auth::not_configured`

Authentication is enabled, but the daemon has no token to check: none in `MEMCASTLE_AUTH_TOKEN` or `auth.token`,
and none generated.
The palace was opened and migrated, but no job ran and nothing was served.
Either set a secret, or start once with authentication disabled, run `memcastle auth generate`, store the token,
and enable authentication.

### `memcastle::auth::entropy_unavailable`

`memcastle auth generate` could not read the operating system's random source.
MemCastle never falls back to a weaker one, so no token was generated and nothing was stored.
Check that the process can use `getrandom` (a restricted container or sandbox is the usual cause), and run it again.

### Everything is refused after `memcastle auth revoke`

Revoking the only credential leaves nothing that can authenticate, so even `memcastle daemon stop` is refused.
Stop the process with your supervisor (or `kill`), and start it again with authentication disabled
(unset `MEMCASTLE_AUTH_ENABLED`) or with a new `MEMCASTLE_AUTH_TOKEN`.
Nothing is lost, because authentication guards access and does not encrypt the palace.

### `memcastle::db::unsafe_bind`

`memcastle db start` was asked to listen beyond loopback and the daemon refused.
The [database admin endpoint](database-access.md) is a console onto the whole database,
so a non-loopback `--bind` needs `--allow-remote` *and* a daemon started with `auth.enabled`.
Nothing was opened.
Leave `--bind` off to listen on `127.0.0.1`, which needs neither.

### `memcastle::db::already_running` or `bind_failed`

`already_running` means the endpoint is already open with different settings than the ones you passed
(another `--bind`, another `--port` or an `--allow-origin` it does not allow).
`memcastle db start` without flags just reports the open endpoint, `memcastle db status` says where it is,
and `memcastle db stop` closes it so you can start it again with the new settings.
`bind_failed` means the address is taken.
Port `8000` is also SurrealDB's own default, so a `surreal start` on the same machine is the usual cause.
Pick another with `--port`, `MEMCASTLE_DB_PORT` or `db.port`, or use `--port 0` to let the OS choose
(the answer prints the port).

### `memcastle::db::unavailable`

The palace uses a remote SurrealDB, which already has a server.
Point SurrealDB Studio at that server directly.

### SurrealDB Studio cannot connect to the database admin endpoint

Check `memcastle db status` first: the endpoint only exists after `memcastle db start`.
A refusal with `403` means the client's origin is not allowed, and the daemon's log names it
(`database admin request from a disallowed origin refused`).
The hosted Surrealist needs `--allow-origin https://app.surrealdb.com`;
the SurrealDB Studio desktop app and pages served from this machine are always allowed.
Studio may report that refusal as a failed login.
A refused sign-in needs the user `memcastle`, and as the password the MemCastle token on an authenticated daemon
or `memcastle` when authentication is disabled.
See [Database access](database-access.md).

### `status` reports the registry as `stale`

The registry file names a process that no longer exists, because the daemon was killed or crashed.
It is ignored and the next daemon overwrites it; you can also delete it, see
[the registry file](storage.md#the-registry-file).
Jobs the crashed daemon was running are recovered on the next start.

### `status` exits 1 and says `DEGRADED`

The daemon answers, but its datastore is unreachable or migrations are pending.
The line below the summary says which.
For pending migrations, see [Migrations and upgrades](migrations.md).

### The daemon exits at start with `memcastle::migrate::failed` or `locked`

A migration failed, or another migration holds the lock.
The daemon refuses to serve a palace it cannot vouch for.
Read the message and the log (`memcastle serve -v`), fix the cause and start again;
the palace resumes from the last step that succeeded.
A lock left by a crashed run expires on its own.

### Logs are noisy

The embedded database logs its own startup at the default `info` level.
Set `MEMCASTLE_LOG=warn` to quieten the daemon.

### `ERROR ... did not shut down cleanly` lines appear when the daemon stops

`serve` and `migrate` wait for the embedded database to finish stopping before they exit, so these lines are not expected.
If you see them, or a warning that the database did not finish shutting down within ten seconds, please report it
with the log.

## Wings, rooms and drawers

### `memcastle::palace::wing_not_found`, `room_not_found` and `drawer_not_found`

Nothing answers to that name or UUID.
`memcastle wing list`, `memcastle room list --wing <wing>` and `memcastle drawer list --room <wing>/<room>` show what
exists.
A room is looked up inside the wing you named, so a room's UUID from another wing is not found there.

### `memcastle::palace::path_invalid`

The path or a name in it cannot be used: a part is empty, a wing or room name contains `/`,
a drawer name has an empty, `.` or `..` segment,
or a name is blank, has leading or trailing whitespace, contains a control character
or looks like a UUID (UUIDs are how records are addressed by id, so they cannot also be names).
The command line checks this before contacting the daemon.
A wing from before these rules may have a `/` in its name: address it by its UUID.

### `memcastle::palace::drawer_name_taken`

A drawer with that name already exists in the room, and holds other content.
Drawer content is immutable, so the name cannot be pointed at new content.
Pick another name, or delete the old drawer first.
Writing the same name with the *same* content succeeds and changes nothing.
In a mined room, only the first copy of a file keeps its name, see [Storage and data](storage.md#what-mining-reads).

### A memory I wrote is missing, or a drawer is "duplicated"

Writing the same text twice into one room stores one drawer.
An unnamed `POST /api/wings/{wing}/rooms/{room}/drawers` answers `200` with `created: false` and the drawer that already
holds it, a diary write returns the agent's existing entry, and a checkpoint item that was an exact copy is counted in the
job result's `duplicates`.
The same text in another room, or from another agent's diary, is a different memory and is stored.

A drawer that differs by a typo, or only in case and punctuation, is stored and linked instead.
`GET /api/drawers/{id}/duplicates` shows what it was linked to and why, and nothing is merged.
To store every write without any of this, set `dedup.enabled = false`, see
[Deduplication](configuration.md#deduplication).

### An entity was not merged with another, or was

Names converge on an entity only when they differ in case, punctuation or spacing, are a recorded alias, or are a unique
one-character typo in a name of six characters or more.
Anything else stays a separate entity, and `GET /api/entities/{id}/candidates` lists the ones it resembles.
To settle one by hand, `POST /api/entities/{id}/aliases` records the spelling, and later mentions converge.
Entities that existed before deduplication are never merged retroactively.
Turn the typo rule off with `dedup.entity_fuzzy = false`.

### `memcastle::palace::busy`

A wing or room delete was refused because a mining job, a checkpoint job, or a repair that applies is queued, running
or paused: it files into wings and rooms by name, so it could quietly bring back what you removed.
`memcastle job list` shows which, and `memcastle job cancel <id>` stops one.
Nothing was deleted.

### `memcastle::jobs::contended`

A job pause, resume, cancel or retry could not be applied because the job kept changing state underneath it.
Nothing was changed: run the command again.
The REST API answers `409`.

## The command line

### `memcastle::cli::aborted`

You answered "no" (or just pressed Enter, which means no) to a confirmation, and nothing was changed.
`repair --apply`, `auth generate`, `auth revoke`, `job cancel`, `source remove`, the `delete` commands of `wing`, `room`
and `drawer`, and `source install` and `source update` when a package asks for permissions,
ask before they act when run in a terminal.
Run the command again and answer `y`, or pass `--yes` to skip the question.
In a script, CI job or pipe the first group never asks, so this error only appears at a keyboard.
`source install` and `source update` do not proceed there either: they refuse with `memcastle::source::consent_required`
(`update` exits `1`) until you pass `--yes` or `--consent <digest>`.

### `memcastle::cli::prompt_failed`

A confirmation could not be shown or read, for example because you pressed Ctrl-C at the prompt.
Pass `--yes` to skip it, or run the command from an interactive terminal.

### `memcastle::project::invalid`

A command that files memory under the current project (`note`) could not read the project's scope.
The message names the file or the variable at fault: a `.config/memcastle.toml` that is not valid TOML,
holds an unknown key, or names a wing or room MemCastle refuses, or a `MEMCASTLE_WING` or `MEMCASTLE_ROOM` that is not a
valid name.
Fix it, or pass `--wing` and `--room` explicitly, see [Project configuration](project-config.md).

### Colours look wrong, or escape codes show up in a file

Colour follows the output stream: it is on for a terminal and off for a pipe or a file.
Set `NO_COLOR=1` to turn it off everywhere, or `FORCE_COLOR=1` to turn it on for the text that reaches a pipe
(`--help`, diagnostics, the progress of `source build`), see [Output, colour and prompts](cli.md#output-colour-and-prompts).
Command results are JSON in a pipe and never carry colour; terminal JSON with `--json` is syntax-highlighted.
`NO_COLOR` wins over `FORCE_COLOR` when both are set.
A `FORCE_COLOR` left in your environment is the usual reason escape codes reach a log.

## Configuration

### `memcastle::config::invalid`

A setting is malformed.
The message names the variable, key or flag and what is wrong.
The usual ones:

- `palace.path ... is not an absolute path`: give an absolute path to `--palace`, `MEMCASTLE_PALACE_PATH` or
  `palace.path`.
  A relative path is refused because a daemon and a client started from different directories would disagree
  about which palace they mean.
- `MEMCASTLE_BIND ... includes a port`: the bind address is an IP address alone; use `--port`, `MEMCASTLE_PORT` or
  `server.port` for the port.
- A `XDG_*_HOME` that seems to be ignored: XDG variables that are unset, empty or relative are ignored by design.

### `memcastle::io::failed` on `--config`

A config file named explicitly must exist, so a typo does not silently fall back to defaults.
The default file, `~/.config/memcastle/config.toml`, is optional.

## Using memory

### `memcastle::mode::forbidden`

The session or request is in a [memory mode](memory-modes.md) that forbids the operation:
`read_only` rejects writes, and `disabled` rejects reads too.
Switch the session back to `full`.
Over MCP, call `memcastle_set_mode`; over REST, fix or drop the `X-MemCastle-Mode` header;
for the CLI, check `--mode` and `MEMCASTLE_MODE`.

### `memcastle::input::invalid` with `x-memcastle-mode`

The `X-MemCastle-Mode` header holds an unknown mode.
Use `full`, `read_only` or `disabled`.

### `memcastle_set_mode` fails with a missing session

The request did not belong to an MCP session, so a mode cannot be remembered for later calls.
Use a client that keeps an MCP session open, or send `X-MemCastle-Mode` over REST.

### `memcastle::jobs::id_invalid` or `not_found`

Job ids are UUIDs, as printed by `memcastle job list`.
A well-formed id the daemon does not know may belong to a different palace.

### `memcastle::jobs::transition_invalid`

The job is not in a state the action applies to:
only a failed job can be retried, only a paused one resumed, and only a queued, paused or running one cancelled.
`memcastle job show <id>` prints its status.

### `mine` finds nothing or stops early

Check the job's `result`:
`memcastle job show <id>` reports how many `documents` were handled, how many were `unchanged` (already filed, so
skipped), `skipped` (not readable) and whether the run was `truncated` at the document limit, in which case run it again.
A source that was mined before remembers where it stopped, so a second `mine` that reports `"documents": 0` has nothing
new to read; `memcastle mine <source> --full` reads it from the beginning.
Skipped directories, large files and non-UTF-8 files are listed in [What mining reads](storage.md#what-mining-reads).
The path given to `mine`, and the sessions directory of `pi`, are read by the daemon, so they must exist on the
daemon's machine.
Over MCP and REST it must be absolute; the CLI makes it absolute for you.

### `memcastle::source::cursor_invalid`

A source's stored cursor is not one its adapter can continue from, for example after a change to how that adapter keeps
its place.
Mine it again from the beginning with `memcastle mine <name> --full`: unchanged documents are recognised and
skipped, so nothing is duplicated.

### `memcastle::source::*`

These come from [installed mining sources](writing-sources.md).
`consent_required` means the package asks for permissions you have not agreed to: review what the error lists, then
install with `--yes`, or with `--consent <digest>` in a script.
`not_enabled` means the source is installed but turned off or cannot run here: `memcastle source list` says which and why,
and `memcastle source enable <name>` turns on a disabled one.
A source that is `unavailable` because its component is missing or no longer matches what was installed needs the package
installed again; one that is incompatible needs rebuilding against this MemCastle's contract.
`failed`, `timeout` and `permission_denied` come from a running source: the message is the source's own, and a source that
cannot read a file it expects probably lacks a `filesystem.read` permission.
The full list is in [Writing a mining source](writing-sources.md#troubleshooting).

### `memcastle::credential::*`

These come from [sources that sign in with OAuth](writing-sources.md#signing-in-with-oauth).
`required` means a run, or a miner, needs the source signed in: run `memcastle source auth <source>`.
It also follows a provider that revoked the credential, and an update that changed what the source asks for
(its client, endpoints or scopes), because a credential is never used under terms it was not given under.
`refresh_failed` means the stored credential could not be renewed just now, almost always the provider or the network;
the credential is kept, so running again later is the fix, and `source auth` again if it keeps happening.
`flow_failed` ends a sign-in that was declined, whose code expired, or that the provider refused;
`source auth` starts a new one.
The browser flow redirects to a port on the machine the *daemon* runs on, so with a daemon on another machine
use a source that offers the device flow, or run the browser on the daemon's machine.
`store_failed` means neither the platform keyring nor the fallback file could be used.
On a Linux server with no desktop session there is no keyring, which `auto` handles by using the file,
so look at the permissions of `credentials.dir`; `credentials.backend = "file"` makes the choice explicit.
See [Credentials](configuration.md#credentials).

### `memcastle::miner::*`

These come from [configured miners](configuration.md#miners).
`invalid` means a definition was refused before anything was written: the message names the setting.
For an enabled miner the usual causes are a source that is not installed or enabled (`memcastle source list`),
a credential whose environment variable is not set in the *daemon's* environment or whose file does not exist,
a relative `locator` for `directory`, and a key in `scope` or `config` that reads like a secret
(put the secret in an environment variable and point `credential` at it).
`scope_broadened` means the change would make the miner read more than before; if that is meant, repeat it with
`--allow-broaden`.
`config_file` means the configuration file cannot be read, or changed while the daemon was editing it.
The daemon keeps the last miners that were valid (`memcastle miner list` shows the error beside them) and will not
write until the file is fixed: correct the entry it names, then `memcastle miner reload`.
`not_runnable` and `disabled` come from `memcastle miner run`: `memcastle miner get <name>` says why the miner is not
`ready`, and a miner whose `scope` or `config` names a key its source does not declare is refused.
A miner that never shows up in `memcastle miner list` after you edited the file by hand is in a file the daemon was not
started from; `miner list` prints the path it reads.

### `memcastle::trigger::*`

These come from [triggers](triggers.md).
`not_activatable` is the daemon refusing to switch a trigger on, and its message is the list of what to set up first:
the miner (it must exist, be enabled and be able to run), a source that supports the trigger's type
(`memcastle sources`), for a webhook the `[webhook]` listener and the shared secret's variable or file, for a watch an
existing path.
Nothing was started and nothing was written; fix the cause and enable it again.
`memcastle trigger get <name>` shows the same list under `to enable:` for a disabled trigger.
`invalid` names the setting that is wrong (a typo in a key, an `every` without a unit, a relative `path`).
`disabled` comes from `memcastle trigger fire`: a disabled trigger is not fired; `memcastle miner run` mines without one.

A trigger that is enabled and does nothing is `unavailable` or `failing` in `memcastle trigger list`, with the reason.
A webhook that gets no deliveries: is `[webhook] enable` set, is the listener up (`trigger list` prints where it is
listening), is the port reachable through your proxy or firewall, and does the sender sign with the same secret?
The listener answers an empty `401` to everything it will not accept, deliberately; the daemon's log says which webhook
could not be read at `warn`.
A watch that stops reporting changes is usually the system's limit on file watches (on Linux,
`fs.inotify.max_user_watches`); the trigger shows it as failing and sets the watcher up again, and a `poll` is the
fallback.

### `memcastle::integration::*`

These come from `memcastle integration` and its installation of the [Pi and OpenCode integrations](integrations.md).
Every one is raised before anything is changed on your machine, except `validation_failed`, which means the copy was
made but does not check out.
`incompatible` names the version it found of MemCastle or the agent and the range the integration supports;
`agent_not_found` means the agent's command is not on the `PATH` of this shell; `conflict` means a plugin file of your own
is in the way and was left alone; and `assets_missing` means this MemCastle has no assets root with integrations in it.
The full table, with what to do about each, is in [Agent integrations](integrations.md#troubleshooting).

### Search returns nothing for a query that should match

Without an [embedding provider](configuration.md#embeddings) search is lexical: it matches words in the stored text, not
their meaning.
Words are stemmed and case is ignored, so `languages` matches `language`, but a synonym such as `tongue` does not.
A query returns the drawers containing every word first.
Only when there are none does it fall back to drawers containing any of the words, best match first.
Short keyword queries therefore work best, and a drawer matching one word of a long question ranks low.
Check the scope too: `--wing`, `--room`, `--tag` and `--source-kind` narrow the result, and a wrong one gives an empty result.
A drawer that was corrected with `drawer supersede` is only found by `--as-of`, `--from` with `--until`, or `--include-historical`,
and `memcastle drawer history` shows how it changed.
A date such as `2026-01-01` means midnight UTC at the start of that day,
so a fact that became true later that day is not there yet.

### Semantic search finds nothing, or fails

- `memcastle::search::semantic_unavailable`: you asked for `--ranking semantic` or `hybrid` but the query could not be
  embedded.
  Configure an `[embeddings]` provider, or search with `--ranking lexical` (the default `auto` falls back by itself).
- `memcastle::embed::not_configured`: `memcastle embed` or an embedding request needs a provider, and none is set.
- `memcastle::embed::failed`: the provider answered with an error or not at all.
  The message carries what it said; for a `command` provider that is its exit status and the start of its standard error.
  Searches with `auto` ranking keep working lexically meanwhile.
- `memcastle::embed::dimension_mismatch`: the model returns another vector length than the 768 the palace stores.
  Ask it for 768 (`dimensions`), or choose a model that produces it.
- Semantic search only finds drawers that already have a vector.
  `memcastle job list` shows the `embed` job that fills them; `memcastle audit` reports how many drawers still have none.

## Other diagnostics

These are rarer, and each one's `help:` line names the fix.

### `memcastle::jobs::orphaned` and `memcastle::jobs::lease_lost`

`orphaned` (REST `409`): a job is recorded as running, but nothing in this daemon is running it.
`memcastle daemon restart` re-queues it, because startup recovery picks up jobs left running.
`lease_lost`: another daemon took a job over after this one's lease expired, which can only happen on a remote palace
shared by several daemons.
If the first daemon was merely slow, raise `jobs.lease_ttl_secs`.

### `memcastle::repair::based_on_job_invalid`

`repair` needs the id of a completed `audit` job to plan from.
Give it one that exists, is an audit, and has finished.

### `memcastle::graph::relationship_not_found` and `memcastle::graph::empty_label`

A checkpoint item's `fact` named a relationship the palace does not hold (`relationship_not_found`),
or gave a blank entity kind or predicate (`empty_label`).
Correct the item and submit it again.

### `memcastle::extract::not_configured` and `memcastle::extract::failed`

`not_configured`: an `extract` job was asked for and the daemon has no `[extraction]` provider.
Set `provider` to `heuristic`, `command` or `http`, see [Extraction](configuration.md#extraction), and restart the daemon.
Without one, nothing is extracted and mining is unaffected.

`failed`: the provider is configured but the call did not work, and the message says how.
For a `command`, the program's exit status and stderr are in it; for `http`, the endpoint's status and answer.
The job fails and the drawers it did not reach are read by the next sweep, so retry it with `memcastle job retry` once the
provider is back.

### `memcastle::graph::entity_not_found`

A graph read named an entity the palace does not hold.
List entities with `GET /api/entities` to find the id you mean.

### `memcastle::client::request_failed`

A request reached the daemon, or tried to, and failed in transport:
a timeout, a dropped connection or an unreadable reply.
The daemon may be busy or restarting; retry, and check `memcastle status` and the daemon's log.
A refused connection is `memcastle::client::not_running` instead.

### `memcastle::server::failed`

The HTTP server failed while starting or serving, other than a failed bind.
Run `memcastle serve -v` in the foreground to see what it was doing.

### `memcastle::store::malformed` and `memcastle::store::schema_sync`

`malformed`: a stored row did not have the expected shape,
usually because a different MemCastle version wrote the palace.
Run `memcastle migrate --status`, and `memcastle audit` to look for damage.
`schema_sync`: applying the bundled schema failed, which points at a defect in a build rather than in your palace.

### `memcastle::serialization::failed`

A value could not be turned into JSON, or read back from it.
This is a bug in MemCastle: please report it with the command that triggered it.

## MCP clients

### The client cannot connect

Make sure the daemon is running and that the client uses the URL printed by `memcastle status` on its `mcp` line,
`http://127.0.0.1:8420/mcp` by default.
The endpoint speaks streamable HTTP, so a browser or a plain `GET` returns `406 Not Acceptable`; that is expected.
If the daemon was started on another port, the client's URL must match it.
See [Connect an MCP client](mcp-clients.md).

### The tools do not appear

Most clients read their MCP configuration at startup, so restart the client after adding the server.
The daemon must be up at that moment; some clients do not retry.

## Still stuck

Run the failing command with `-vv`, run `memcastle serve -vv` in the foreground,
and set `RUST_BACKTRACE=1` if an error looks like a bug.
Then open an issue at <https://github.com/noirbizarre/memcastle/issues> with the diagnostic code,
the output of `memcastle status --json`, and your MemCastle version (`memcastle --version`).
