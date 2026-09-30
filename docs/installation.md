# Installation

MemCastle is a single self-contained binary, `memcastle`.
It embeds its database, so there is nothing else to install and no service to set up.
Pick the method that suits your platform.

## Install

### Homebrew (macOS)

```sh
brew install noirbizarre/homebrew-tap/memcastle
```

The formula ships macOS builds only (Apple silicon and Intel).
On Linux, use one of the other methods, including under Linuxbrew.

### Arch Linux (AUR)

```sh
paru -S memcastle-bin   # or your AUR helper of choice
```

The `memcastle-bin` package installs the release binary for x86_64 and aarch64.

### Release binary

Download the binary for your platform from the
[latest release](https://github.com/noirbizarre/memcastle/releases/latest),
make it executable and put it on your `PATH`.
Assets are the executable itself, not an archive, and are named `memcastle_<version>_<platform>`:

| Platform | Asset suffix |
|---|---|
| Linux x86_64 | `linux-amd64` |
| Linux aarch64 | `linux-arm64` |
| macOS Intel | `darwin-amd64` |
| macOS Apple silicon | `darwin-arm64` |
| Windows x86_64 | `windows-amd64.exe` |

```sh
chmod +x memcastle_*_linux-amd64
mv memcastle_*_linux-amd64 ~/.local/bin/memcastle
```

### From source

You need a Rust toolchain, version 1.90 or newer.

```sh
git clone https://github.com/noirbizarre/memcastle
cd memcastle
cargo install --path .
```

The first build compiles the embedded database and takes a few minutes.
MemCastle is not published on crates.io, so `cargo install memcastle` does not work.

## Check the install

```sh
memcastle --version
```

```text
memcastle 0.1.0
```

`memcastle --help` lists every command; the [CLI reference](cli.md) describes them.

## Platform notes

- **Linux and macOS** follow the same Unix XDG layout for configuration, data and runtime state.
  macOS does not use `~/Library/Application Support`.
  See [Configuration](configuration.md#where-files-live).
- **Windows** builds are published and tested, but there is no package and no dedicated layout:
  the same dot-directories are used under your home directory.
- No external database is needed for local use.
  A remote SurrealDB server is optional, see [Storage and data](storage.md#embedded-and-remote-stores).

## Next

[Quickstart](quickstart.md) starts the daemon and stores your first memories.
