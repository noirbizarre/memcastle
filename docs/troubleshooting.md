# Troubleshooting

Every MemCastle failure carries a diagnostic code of the form `memcastle::<module>::<kind>` and a `help:` line saying
what to do.
Codes are stable, so it is safe to search for or match on them.
When the daemon rejects a CLI request, the CLI reports it as `memcastle::client::remote_rejected`
and shows the daemon's own code in parentheses, such as `(403, memcastle::app::mode_forbidden)`;
that inner code is the one to look up below.
Add `-v` for the full cause chain of an error, and see [Logging](daemon.md#logging) for the daemon's own log.

Start with `memcastle status`: it says whether a daemon is running, where, and whether its datastore is healthy.

## The daemon

### `memcastle::client::not_running`

No daemon answered.
Start one with `memcastle serve`.
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

## The command line

### `memcastle::cli::aborted`

You answered "no" (or just pressed Enter, which means no) to a confirmation, and nothing was changed.
`repair --apply`, `auth generate`, `auth revoke` and `jobs cancel` ask before they act when run in a terminal.
Run the command again and answer `y`, or pass `--yes` to skip the question.
In a script, CI job or pipe they never ask, so this error only appears at a keyboard.

### `memcastle::cli::prompt_failed`

A confirmation could not be shown or read, for example because you pressed Ctrl-C at the prompt.
Pass `--yes` to skip it, or run the command from an interactive terminal.

### Colours look wrong, or escape codes show up in a file

Colour follows the output stream: it is on for a terminal and off for a pipe or a file.
Set `NO_COLOR=1` to turn it off everywhere, or `CLICOLOR_FORCE=1` to turn it on for a pipe,
see [Output, colour and prompts](cli.md#output-colour-and-prompts).
A `CLICOLOR_FORCE` left in your environment is the usual reason escape codes reach a log.

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

### `memcastle::app::mode_forbidden`

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

### `memcastle::jobs::invalid_id` or `not_found`

Job ids are UUIDs, as printed by `memcastle jobs list`.
A well-formed id the daemon does not know may belong to a different palace.

### `memcastle::jobs::invalid_transition`

The job is not in a state the action applies to:
only a failed job can be retried, only a paused one resumed, and only a queued, paused or running one cancelled.
`memcastle jobs show <id>` prints its status.

### `mine` finds nothing or stops early

Check the job's `result`:
`memcastle jobs show <id>` reports `files_considered` and whether the run was `truncated` at 2000 files.
Skipped directories, large files and non-UTF-8 files are listed in [What mining reads](storage.md#what-mining-reads).
The path given to `mine` is read by the daemon, so it must exist on the daemon's machine.
Over MCP and REST it must be absolute; the CLI makes it absolute for you.

### Search returns nothing for a query that should match

Search is lexical, not semantic: it matches words in the stored text, not their meaning.
Check the scope too: `--wing` and `--room` take names, and a wrong one gives an empty result.

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
