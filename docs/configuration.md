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
| Package assets (not user data) | `<prefix>/share/memcastle`, or none | `--assets-dir` / `MEMCASTLE_ASSETS_DIR` / `assets.dir` |

The last row is not a place you keep anything.
It is where an OS package may install read-only files such as a future web UI, and it is never under the XDG directories.
See [Runtime assets](#runtime-assets).

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
Each palace has its own file, `$XDG_STATE_HOME/memcastle/run/<palace-hash>/daemon.json`,
where `<palace-hash>` is derived from the palace's canonical path.
If no home directory can be determined, the registry falls back to `memcastle/run` under the system temporary directory.
[Storage and data](storage.md#the-registry-file) describes the file.

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
`MEMCASTLE_LOG`, `RUST_LOG` and `logging.level` accept a level (`error`, `warn`, `info`, `debug`, `trace`)
or a filter directive such as `memcastle=debug,warn`.

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
# "text" for a terminal, "json" (one object per line) for journald and log shippers.
format = "text"

# Optional bearer-token authentication. Prefer MEMCASTLE_AUTH_TOKEN to `token` here,
# so the secret stays out of the file.
[auth]
enabled = false

# Only to serve assets from somewhere other than the installed or embedded ones.
[assets]
dir = "/home/alice/src/memcastle-web/dist"

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
| `assets.dir` (an absolute path) | `MEMCASTLE_ASSETS_DIR` | none: installed, then embedded assets |
| `logging.level` | `MEMCASTLE_LOG` | `info` |
| `logging.format` | `MEMCASTLE_LOG_FORMAT` | `text` (`text` or `json`) |
| `jobs.max_concurrency` (at least 1) | `MEMCASTLE_JOBS_MAX_CONCURRENCY` | `4` |
| `jobs.drain_timeout_secs` (1 to 86400) | `MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS` | `10` |
| `jobs.lease_ttl_secs` (3 to 86400) | `MEMCASTLE_JOBS_LEASE_TTL_SECS` | `30` |
| `auth.enabled` (`true` or `false`) | `MEMCASTLE_AUTH_ENABLED` | `false` |
| `auth.token` (at least 16 characters) | `MEMCASTLE_AUTH_TOKEN` | none |
| `store.mode` and remote settings | none | `embedded` |

`auth.token` is a secret, and is handled as one: MemCastle never logs it, prints it, serialises it, or writes it anywhere,
and the client commands read it from the environment or the config file, never from a flag.
See [Authentication](authentication.md).

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
| `--assets-dir <DIR>` | `serve`, `restart` | `assets.dir`, `MEMCASTLE_ASSETS_DIR` |
| `--mode <MODE>` | every command (acted on by client commands) | the memory mode of the session |
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
`restart` passes its `--bind`, `--port` and `--assets-dir` on to the new daemon.

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
Authentication is off unless you enable it, so use a non-loopback address such as `0.0.0.0` only on a network you trust,
or with [authentication](authentication.md) enabled.
The daemon logs a warning when it listens beyond loopback without it.
The MCP endpoint additionally refuses a non-loopback `Host` header, see [Authentication](authentication.md#exposing-the-daemon).

The listener is bound before anything else happens.
If the address is taken, needs privileges, or does not exist on this machine,
the start fails with `memcastle::server::bind_failed`, naming the address and what to change,
and has not created the palace, migrated it or touched its jobs.

## Runtime assets

Some files a release carries are not your configuration or your data.
They are read-only, come with the version of MemCastle you installed, and are found by one fixed rule, tried in order:

1. **An explicit directory**, from `--assets-dir`, `MEMCASTLE_ASSETS_DIR` or `assets.dir`, by the usual precedence.
   It must be an absolute path to an existing directory.
   If it does not exist the daemon refuses to start with `memcastle::assets::not_found`;
   it never quietly uses another source in its place.
2. **The installed directory**, which an OS package creates: `share/memcastle` next to the executable's `bin/`
   (`/usr/share/memcastle` for `/usr/bin/memcastle`), then `/usr/local/share/memcastle`, then `/usr/share/memcastle`.
   The first that exists is used.
   A candidate inside your XDG data directory is ignored, because that is where your palace lives.
3. **The assets built into the binary.**
   This is what a standalone download uses, and it needs no directory and no network.

The daemon logs which source it resolved at startup (`runtime assets resolved`).
The schema and the data migrations are always built into the binary and are not affected by any of this.
[Installation](installation.md#standalone-binary-or-native-package) describes the package layout.

Nothing in 0.1 installs files into the assets directory, so you only need the setting when developing a web UI locally.

## Not supported: other platforms' conventions

Windows is built and tested, but has no dedicated layout: the same dot-directories are used under your home directory.
Any platform-specific behaviour would be an explicit design decision, recorded in an ADR;
see [ADR-010](adr/010-unix-xdg-paths.md).

## See also

- [Running the daemon](daemon.md) for starting, stopping and supervising it.
- [Storage and data](storage.md) for what lives in the palace and the registry.
- [CLI reference](cli.md) for every flag.
- [Troubleshooting](troubleshooting.md) for configuration errors.
