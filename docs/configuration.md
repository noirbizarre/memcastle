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
bind = "127.0.0.1:8420"

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
| `server.bind` | `MEMCASTLE_BIND` | `127.0.0.1:8420` |
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
| `--bind <ADDR>` | `serve`, `restart` | `server.bind`, `MEMCASTLE_BIND` |
| `--mode <MODE>` | client commands | the memory mode of the session |
| `-v`, `-vv` | every command | the log level of memcastle itself |

## Not supported: other platforms' conventions

Windows is built and tested, but has no dedicated layout: the same dot-directories are used under your home directory.
Any platform-specific behaviour would be an explicit design decision, recorded in an ADR;
see [ADR-010](adr/010-unix-xdg-paths.md).
