# ADR-010: Configuration, data and state follow the Unix XDG layout on Linux and macOS

## Status

Accepted

## Context

MemCastle started with everything under `~/.memcastle`: the config file, the default palace and the daemon registry.
That mixes three kinds of file with different lifetimes, and it is not where users of Unix tools look.
The usual fix is the XDG Base Directory specification.

The convenient way to get it, `dirs::config_dir()` and `dirs::data_dir()`, is platform-native.
On macOS those answer `~/Library/Application Support`.
MemCastle is a developer tool whose users keep dotfiles and shell profiles in sync between a Linux machine and a Mac,
and whose documentation and support answers should not fork per operating system.

A relative `XDG_*` value, or a missing home directory, is a further trap.
A daemon and a client started from different working directories would disagree about which palace they mean.

## Decision

- **One layout for Linux and macOS.**
  Configuration is `$XDG_CONFIG_HOME/memcastle/` (default `~/.config/memcastle/`), the palace is
  `$XDG_DATA_HOME/memcastle/default` (default `~/.local/share/memcastle/default`), and the daemon registry is
  `$XDG_STATE_HOME/memcastle/run/` (default `~/.local/state/memcastle/run/`).
  The resolver is hand-written in `config::paths`, taking the environment and home directory as inputs,
  so tests cover both platforms' semantics on every operating system.
- **XDG semantics as specified.**
  An unset, empty or relative variable is ignored in favour of the home-directory default.
- **The registry moves out of the palace's way.**
  It is runtime metadata, so it lives in the state directory, not in the data directory that users back up and copy.
- **Precedence is defaults, config file, environment, command line.**
  A global `--palace` flag joins `--bind` (and, since ADR-011, `--port`) as the command-line layer,
  built once before the config is loaded
  so that every command resolves the same palace.
- **A palace path must be absolute.**
  Validation rejects a relative one with a message naming the ways to set it.
- **Platform-specific behaviour is a future, explicit decision.**
  It must be recorded in a new ADR that supersedes this one.

## Alternatives rejected

- **`dirs::config_dir()` and `dirs::data_dir()`.**
  Correct per platform, but it splits macOS from Linux, which is exactly what this decision avoids.
- **Keep `~/.memcastle`.**
  Simple, but it puts config, data and runtime state in one directory the XDG-aware tooling cannot relocate.
- **Fall back to `~/.memcastle` when it exists.**
  The project is pre-release, so a permanent second location to document and test costs more than it saves.
- **The registry beside the palace.**
  Keeps it with its palace, but ships a stale daemon record along with every backup or copy.
- **`$XDG_RUNTIME_DIR` for the registry.**
  It is absent on macOS and cleared at logout, which would make daemons undiscoverable across sessions.

## Consequences

- Existing `~/.memcastle` directories are not read.
  A user who has one moves it by hand, or points `--palace` at it.
- A machine with no home directory and no usable `XDG_DATA_HOME` needs an explicit palace path.
  The error says so, rather than creating a palace in the current directory.
- Windows compiles and uses the same dot-directories under the home directory;
  it is built and tested, but has no dedicated layout.
- The `dirs` crate is still used, but only to find the home directory.
