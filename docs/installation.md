# Installation

MemCastle is a single self-contained binary, `memcastle`.
It embeds its database, so there is nothing else to install and no service to set up.
Pick the method that suits your platform.

## Standalone binary or native package

There are two ways to have MemCastle, and they run the same daemon.

- **A standalone binary** is one file, wherever you put it.
  It embeds everything the daemon needs to start, including the database schema and the data migrations,
  so it works offline and needs no directory of assets.
  Nothing is downloaded when it first starts.
- **A native package** (the AUR package, or a Linux distribution's own) installs the same binary into the system,
  and may also install read-only files that belong to the package rather than to the binary,
  such as a web UI or a service unit.
  Your package manager owns and updates them.

Your configuration and your data are never part of either.
They stay under your home directory, whichever way MemCastle was installed:

| What | Where | Owned by |
|---|---|---|
| Executable | `/usr/bin/memcastle` | the package, or you |
| Package assets | `/usr/share/memcastle/` | the package |
| Documentation | `/usr/share/doc/memcastle/` | the package |
| Configuration | `~/.config/memcastle/` | you |
| Palace (persistent data) | `~/.local/share/memcastle/` | you |
| Daemon runtime state | `~/.local/state/memcastle/` | MemCastle |

Package assets and your data directories never overlap, so removing or upgrading a package cannot touch a palace.
The daemon looks for assets in a fixed order: a directory you name with `--assets-dir`, then the installed
`share/memcastle`, then what is built in.
See [Runtime assets](configuration.md#runtime-assets) for the rule, and
[ADR-013](adr/013-release-packaging-and-asset-resolution.md) for why.
In 0.1 no package installs assets yet, so a standalone binary and a package behave identically.

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
The assets named `memcastle_<version>_<platform>` are the executable itself, not an archive:

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

### Release tarball (Linux and macOS)

Each Unix platform also has `memcastle_<version>_<platform>.tar.gz`, laid out the way a native package is:

```text
memcastle-<version>-<platform>/
├── bin/memcastle
└── share/doc/memcastle/{LICENSE,README.md}
```

Unpack it at a prefix to install it in the same places a package would:

```sh
sudo tar -xzf memcastle_*_linux-amd64.tar.gz --strip-components=1 -C /usr/local
```

There is no `share/memcastle/` in the tarball yet, because 0.1 has no package assets.
Do not unpack it at `~/.local` once it has one: that would put the package's assets inside your data directory,
which MemCastle ignores as an asset location on purpose.
Use `/usr/local`, or a directory of your own together with `--assets-dir`.

### Verify a download

Every release lists a SHA-256 checksum for each file in `SHA256SUMS`:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

The files are also attested: GitHub signs a statement that its Actions workflow built exactly these bytes from this
repository at the release tag.
With the [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify memcastle_<version>_linux-amd64 --repo noirbizarre/memcastle
```

A software bill of materials, `memcastle-<version>.cdx.json` in CycloneDX format, lists every crate in the build.
The releases are built to be reproducible; the Linux binary is rebuilt from two different checkout paths in CI
and compared byte for byte.

### From source

You need a Rust toolchain, version 1.90 or newer.

```sh
git clone https://github.com/noirbizarre/memcastle
cd memcastle
cargo install --path .
```

The first build compiles the embedded database and takes a few minutes.
The storage engine is pure Rust, but a TLS dependency compiles some C, so a C compiler is needed;
`cmake` may be needed too on some hosts.
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
