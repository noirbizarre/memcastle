# Configuration

MemCastle runs with no configuration at all.
Everything below is for changing where it keeps things, or how it behaves.

## Where files live

MemCastle follows the **Unix XDG Base Directory convention** on Linux and macOS alike.
macOS does **not** use `~/Library/Application Support`: both platforms use the same paths,
so a dotfiles repository or a shell profile written once works on both.

| What | Default | Relocated by |
|---|---|---|
| Config file | `~/.config/memcastle/config.toml` | `XDG_CONFIG_HOME`, or `--config` / `MEMCASTLE_CONFIG` |
| Palace (persistent data) | `~/.local/share/memcastle/default` | `XDG_DATA_HOME`, or `--palace` / `MEMCASTLE_PALACE_PATH` / `palace.path` |
| Daemon registry (runtime state) | `~/.local/state/memcastle/run/` | `XDG_STATE_HOME` |

The XDG variables are honoured as the specification describes:
an unset, empty or relative value is ignored, and the default under your home directory is used instead.
For example, with `XDG_DATA_HOME=/srv/data` the palace is `/srv/data/memcastle/default`.

The config file is optional.
If the default file does not exist, built-in defaults are used.
A file named explicitly with `--config` must exist, because a typo there would otherwise be silently ignored.

### Palace and data path selection

A palace is one directory.
With the embedded store, the SurrealDB files live in its `db/` subdirectory.
The path is chosen by, highest precedence first: `--palace`, `MEMCASTLE_PALACE_PATH`, `palace.path` in the config file,
then `$XDG_DATA_HOME/memcastle/default`.
It must be an absolute path; a relative one is rejected at startup, because a daemon and a client started
from different directories would disagree about which palace they mean.

Every command resolves the palace the same way, so `serve`, `status`, `migrate`, `stop` and the rest agree on it.
A client finds a running daemon through the registry file, which is keyed by the palace path.
`restart` passes the palace it resolved on to the new daemon.

The registry is runtime metadata, not palace data, so it lives in the state directory rather than beside the palace.
Backing up or copying a palace never carries a stale daemon record with it.

## Precedence

From lowest to highest, a later layer overrides an earlier one:

1. Built-in defaults.
2. The config file.
3. `MEMCASTLE_*` environment variables.
4. Command-line flags.

A malformed environment variable is an error naming the variable; it is never silently ignored.
The resolved configuration is validated once all layers are applied.

Logging has one more input.
Its precedence, highest first, is `MEMCASTLE_LOG`, `RUST_LOG`, the `-v` / `-vv` flags, then `logging.level`.

## Config file

TOML.
Every key is optional, and a file may set only some of them.

```toml
[palace]
path = "/home/alice/.local/share/memcastle/default"

[server]
bind = "127.0.0.1"
port = 8420

[logging]
level = "info"

[jobs]
max_concurrency = 4
drain_timeout_secs = 10
lease_ttl_secs = 30

# Embedded SurrealKV under palace.path (the default), or a remote SurrealDB.
[store]
mode = "embedded"
```

A remote store also needs a URL and credentials:

```toml
[store]
mode = "remote"
url = "ws://localhost:8000"
namespace = "memcastle"
database = "main"
username = "root"
password = "..."
```

Remote store settings are file-only; there is no environment variable for them.
Keep secrets out of version control: put this file outside any repository, and restrict it with `chmod 600`.

## Environment variables

| Setting (TOML key) | Environment variable | Default |
|---|---|---|
| `palace.path` | `MEMCASTLE_PALACE_PATH` | `~/.local/share/memcastle/default` |
| `server.bind` (an IP address) | `MEMCASTLE_BIND` | `127.0.0.1` |
| `server.port` (0 to 65535) | `MEMCASTLE_PORT` | `8420` |
| `logging.level` | `MEMCASTLE_LOG` | `info` |
| `jobs.max_concurrency` | `MEMCASTLE_JOBS_MAX_CONCURRENCY` | `4` |
| `jobs.drain_timeout_secs` (1 to 86400) | `MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS` | `10` |
| `jobs.lease_ttl_secs` (3 to 86400) | `MEMCASTLE_JOBS_LEASE_TTL_SECS` | `30` |
| `store.mode` and remote settings | none | `embedded` |

Some variables are read by the command line rather than the config file:

| Variable | Equivalent flag |
|---|---|
| `MEMCASTLE_CONFIG` | `--config` |
| `MEMCASTLE_MODE` | `--mode` |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` | none |
| `RUST_LOG` | none |

## Command-line flags

| Flag | Applies to | Overrides |
|---|---|---|
| `--config <FILE>` | every command | the default config file location |
| `--palace <PATH>` | every command | `palace.path`, `MEMCASTLE_PALACE_PATH` |
| `--bind <IP>` | `serve`, `restart` | `server.bind`, `MEMCASTLE_BIND` |
| `--port <PORT>` | `serve`, `restart` | `server.port`, `MEMCASTLE_PORT` |
| `--mode <MODE>` | client commands | the memory mode of the session |
| `-v`, `-vv` | every command | the log level of memcastle itself |

## The listener: address and port

The daemon serves the REST API and MCP on one TCP listener, `127.0.0.1` port `8420` unless configured otherwise:

```sh
memcastle serve --bind 127.0.0.1 --port 8787
```

The address and the port are separate settings, so either can be changed alone.
Each is chosen by, highest precedence first: the flag, the environment variable, the config file, the default.
`serve`, `restart` and a supervisor such as systemd all start the daemon through the same path,
so the same three sources work everywhere.
Client commands (`status`, `search`, `jobs` and the rest) read the config file and the environment,
but not the flags, so a daemon started on a non-default port with `--port` is found through its registry file.
`restart` passes its `--bind` and `--port` on to the new daemon.

`memcastle status` shows which of the two it used (`endpoint_source`: `registry` or `config`).
It exits 0 for a healthy daemon, 1 for a degraded one and 3 when none is running, and takes `--json` for scripts:

```sh
memcastle status                      # human-readable report
memcastle status --json | jq .daemon.datastore
memcastle status || echo "exit $?"    # 3 means not running
```

`server.bind` is an IP address, IPv4 or IPv6, and not a host name.
A `host:port` value, as `bind` accepted before the port became its own setting, is refused with a message naming the port
setting to use instead.
Port `0` asks the OS for a free port, which is useful for tests and scripts;
the port actually chosen is in the daemon's registry file and its log.

The default never listens on all network interfaces.
The daemon has no authentication, so use a non-loopback address such as `0.0.0.0` only on a network you trust.

The listener is bound before anything else happens.
If the address is taken, needs privileges, or does not exist on this machine,
the start fails with `memcastle::server::bind_failed`, naming the address and what to change,
and has not created the palace, migrated it or touched its jobs.

## Not supported: other platforms' conventions

Windows is built and tested, but has no dedicated layout: the same dot-directories are used under your home directory.
Any platform-specific behaviour would be an explicit design decision, recorded in an ADR;
see [ADR-010](adr/010-unix-xdg-paths.md).
