# Running the daemon

MemCastle is a long-running server: one daemon serves one palace, and every client — the CLI, MCP clients, scripts —
talks to it over HTTP.
Nothing starts it for you, so you start it once and leave it running.

## Start it

```sh
memcastle daemon start
```

`daemon start` runs the daemon in the **background**: it spawns `serve` detached from your terminal,
waits until the daemon is really serving, and reports where.
It fails with `memcastle::client::already_running` when a daemon already answers for the palace;
use `daemon restart` to replace it.
The background daemon's log output is discarded.

```sh
memcastle serve
```

`serve` runs in the **foreground** and logs to standard error.
It is what a supervisor runs (see [Under a supervisor](#under-a-supervisor)),
and what to use when you need to see the log, for instance when `daemon start` reports that the daemon did not come up.

With no configuration it listens on `127.0.0.1:8420` and serves the palace at `~/.local/share/memcastle/default`.
Change either from the command line, the environment or the config file;
`daemon start` and `daemon restart` take the same `--bind`, `--port` and `--assets-dir` as `serve`:

```sh
memcastle serve --bind 127.0.0.1 --port 8787
memcastle daemon start --bind 127.0.0.1 --port 8787
memcastle --palace ~/palaces/work serve
```

See [Configuration](configuration.md) for every setting and how they combine.

### What happens on startup

```mermaid
sequenceDiagram
    participant D as memcastle serve
    participant S as Storage
    participant C as Clients

    D->>D: load and validate configuration
    D->>D: locate the runtime assets (read-only)
    D->>D: bind the listener
    Note over D: a taken port fails here, before anything is touched
    D->>S: connect
    D->>S: apply pending migrations
    Note over D,S: a failed migration stops the daemon, nothing is served
    D->>D: check authentication is configured
    Note over D: enabled without a token stops here, before the scheduler starts
    D->>S: recover jobs left running by a crash
    D->>D: start the job scheduler
    D->>D: write the registry file
    C->>D: connect (REST, MCP)
```

The listener is bound first, so an address that is taken or unavailable fails the start
without having created, migrated or recovered anything.
Connections that arrive while storage is still opening wait in the listener's backlog.
The registry file, which is how clients find the daemon, is written only once the daemon is ready to serve,
and `serve` never answers a request against a palace that is not fully migrated.
See [Migrations and upgrades](migrations.md).

## Check on it

```sh
memcastle status
```

```text
MemCastle is running
  version    0.2.0 (pid 2528880, up 3s)
  endpoint   http://127.0.0.1:8420 (from the daemon's registry file)
  mcp        http://127.0.0.1:8420/mcp
  palace     default (/home/alice/.local/share/memcastle/default)
  datastore  ok - embedded /home/alice/.local/share/memcastle/default/db, migrations 4/4
  drawers    0
  jobs       0 queued, 0 running, 0 paused
  mode       full
  auth       disabled
Restart with `memcastle daemon restart`, stop with `memcastle daemon stop`.
```

With no daemon, `status` says so, shows where it looked and how to start one.
The exit code tells scripts which state it found:

| Exit code | State |
|---|---|
| `0` | Running, datastore healthy. |
| `1` | Running but degraded (datastore unreachable or migrations pending), or an error. |
| `3` | Not running. |

`memcastle status --json` prints the same report as JSON:

```sh
memcastle status --json | jq .daemon.datastore
memcastle status > /dev/null || echo "exit $?"
```

!!! note
    `status || memcastle daemon start` also runs `daemon start` when `status` exits `1` (a degraded daemon),
    but that fails with `memcastle::client::already_running`, because anything that answers its health check counts as running.
    A degraded daemon is never replaced by it: use `daemon restart` for that,
    or test for exit code 3 explicitly to start a daemon only when none is running.

## How clients find the daemon

Client commands do not take `--bind` or `--port`.
They look for the daemon in this order:

1. The **registry file** of the palace they resolved, which a running daemon writes with its real address.
2. The configured address (`server.bind` and `server.port`, defaulting to `127.0.0.1:8420`).

The registry file is a hint, never the source of truth: whether a daemon is running is always answered by a live request.
It is keyed by the palace path, so several daemons for different palaces can run side by side on different ports,
and a command finds the right one as long as it is given the same `--palace` or configuration.
A file left behind by a crashed daemon (its process is gone) is reported by `status` as `stale` and overwritten by the
next daemon.
Its location and contents are in [Storage and data](storage.md#the-registry-file).

## Stop and restart

```sh
memcastle daemon stop
memcastle daemon restart
```

`daemon stop` asks the daemon to shut down gracefully, and so does `SIGINT` or `SIGTERM` sent to a foreground `serve`.
The daemon stops accepting new jobs and asks every running job to stop at its next unit of work.
Each such job saves its checkpoint and goes back to the queue, so the next daemon resumes it on its own;
a job you had paused stays paused.
The wait is bounded by `jobs.drain_timeout_secs` (10 seconds by default).
A job that does not stop in time is left running, and the next start re-queues it.
Either way the daemon then removes its registry file and exits.

`daemon restart` stops the running daemon, starts a fresh detached one and waits until it serves.
With no daemon running it just starts one.
Like `daemon start`, it passes on `--config`, `--bind`, `--port`, `--assets-dir` and the palace it resolved.
The new daemon's log output is discarded, and `daemon restart` is a best-effort convenience:
under a supervisor, restart through the supervisor instead.

## Logging

The daemon logs to standard error only; there are no log files.
`-v` logs MemCastle itself at `debug`, `-vv` at `trace`.
`MEMCASTLE_LOG` and `RUST_LOG` override the flags and accept filter directives such as `warn` or `memcastle=debug`;
`logging.level` in the config file is the lowest-precedence setting.
At `debug`, every REST and MCP request and every MCP tool call is logged.

### Format

`MEMCASTLE_LOG_FORMAT=json` (or `format = "json"` under `[logging]`) writes one JSON object per line instead of text.
Use it where a log shipper reads the output; the default `text` suits a terminal.
Colour is only used when standard error is a terminal, so journald never receives escape codes.

### What is logged

At the default `info` level the daemon logs lifecycle transitions, not individual operations:

| Event | Fields |
| ----- | ------ |
| Startup, listener ready, shutdown | version, pid, bind address, palace, backend |
| Scheduler configured, jobs recovered | concurrency, lease TTL, job ID |
| `job queued` | `job_id`, `job_type`, `priority`, `requested_by` |
| `job claimed by worker` | `job_id`, `job_type`, `priority`, `attempt`, `worker_id` |
| `job started` | `job_id`, `job_type`, `attempt` |
| `job completed`, `paused`, `cancelled` | `job_id`, `job_type`, `elapsed_ms` |
| `job failed` | `job_id`, `error`, `elapsed_ms`, `attempt` |
| Pause, cancel, resume and retry requests | `job_id` |
| Mining, checkpoint, audit and repair start | counts only (files, items, dry run) |

Lines emitted while a job runs carry its `job_id` and `job_type` through the enclosing `job` span,
so filtering on the ID gives the whole lifecycle.
At `debug`, every REST and MCP request is logged with a `request_id`, method and path;
the same ID is returned in the `x-request-id` response header, and a client-supplied one is kept.
Failed requests and tool calls are logged at `warn` or `error` with the same ID and the diagnostic code.

Logs never contain memory or file contents, file names, query strings, request headers, tokens or secrets:
only identifiers, counts, types, durations, statuses and error messages.

### Reading the logs under systemd

```sh
journalctl --user -u memcastle -f
journalctl --user -u memcastle -o cat | grep <job-id>
```

With `MEMCASTLE_LOG_FORMAT=json`, `journalctl --user -u memcastle -o cat | jq 'select(.fields.job_id == "<job-id>")'` works.
The packaged unit sets `MEMCASTLE_LOG=warn`, which hides the `info` lifecycle events above:
override it with `systemctl --user edit memcastle` (`Environment=MEMCASTLE_LOG=info`) to see them.

The default level also shows startup messages from the embedded database, which are noisy but harmless.
Set `MEMCASTLE_LOG=warn` for a quiet daemon.

## Under a supervisor

The daemon does its own graceful shutdown on `SIGTERM`, does not fork, and logs to standard error,
which is what supervisors expect.
The binary neither generates nor installs service files: integrating with an init system is a package's job,
not the core binary's (see [ADR-013](adr/013-release-packaging-and-asset-resolution.md)).
The Arch package (`memcastle-bin`) and the `.deb` and `.rpm` packages install a systemd user unit,
so there `systemctl --user start memcastle` is enough;
see [Installation](installation.md#arch-linux-aur) and
[Installation](installation.md#standalone-binary-or-native-package).
Its `ExecStart` is `/usr/bin/memcastle serve`, and it reads an optional `~/.config/memcastle/secret.env`.
Change it with `systemctl --user edit memcastle`, which writes a drop-in and leaves the package file alone.
The packaged unit sets `MEMCASTLE_LOG=warn`, which outranks `-v` and `logging.level`;
override it in the same drop-in if you want more output.
If `systemctl --user status memcastle` shows a failed start, `journalctl --user -u memcastle -n 50` has the diagnostic.

Elsewhere, a systemd user unit you can write by hand looks like this:

```ini title="~/.config/systemd/user/memcastle.service"
[Unit]
Description=MemCastle memory daemon

[Service]
ExecStart=%h/.local/bin/memcastle serve
Restart=on-failure
# Give running jobs time to checkpoint (jobs.drain_timeout_secs is 10 by default).
TimeoutStopSec=30

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now memcastle
journalctl --user -u memcastle -f
```

Adjust `ExecStart` to where `memcastle` is installed.
On macOS a launchd agent that runs `memcastle serve` with `KeepAlive` does the same job.

## Exposing the daemon

Authentication is optional and **off by default**.
The default, `127.0.0.1`, keeps the daemon reachable from your machine only.
Binding a non-loopback address such as `0.0.0.0` exposes the palace to that network, and the daemon logs a warning
when it does so without authentication.
Enable [authentication](authentication.md) when you do, and put a TLS-terminating proxy in front of it,
because the token travels in cleartext over plain HTTP.
`/mcp` also refuses a non-loopback `Host` header, so reach it through a loopback name or a proxy that rewrites `Host`.

With a supervisor, inject the secret as `MEMCASTLE_AUTH_TOKEN` from an `EnvironmentFile=` or your secret manager,
not as an argument; [Authentication](authentication.md#with-a-shared-secret-from-a-secret-manager) shows how.
