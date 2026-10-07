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
  such as the web UI or a service unit.
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
One package asset is the set of sources that ship with MemCastle, under `share/memcastle/sources/`
(see [Publishing and installing sources](publishing-sources.md#bundled-sources)): the release tarballs, the `.deb` and
`.rpm`, the AUR package and the Homebrew formula carry them, unpacked and installed from the start, and a standalone binary
does not, so it installs a source from a file or from the official registry instead.
The other package assets are the agent integrations and the skills they expose, under `share/memcastle/integrations/` and
`share/memcastle/skills/`; `memcastle integration install pi` copies one to your home and registers it with the agent
(see [Agent integrations](integrations.md)).
The same release packages carry them, and a standalone binary does not.
The last package asset is the [web UI](web.md), under `share/memcastle/web/dist/`, which the daemon serves only when
you set `web.enable`.

## Install

### Homebrew (macOS)

```sh
brew install noirbizarre/homebrew-tap/memcastle
```

The formula ships macOS builds only (Apple silicon and Intel), the sources bundled with MemCastle under its
`share/memcastle/sources/`, so `memcastle source enable pi` works with no registry, and the agent integrations under
`share/memcastle/integrations/`, so `memcastle integration install pi` works without any other download.
On Linux, use one of the other methods, including under Linuxbrew.

### Arch Linux (AUR)

```sh
paru -S memcastle-bin   # or your AUR helper of choice
```

The `memcastle-bin` package installs the release binary for x86_64 and aarch64,
a systemd user unit, `/usr/lib/systemd/user/memcastle.service`,
the bash, zsh and fish completion scripts,
the sources bundled with MemCastle under `/usr/share/memcastle/sources/`, so `memcastle source enable pi` and
`memcastle source enable opencode` work with no registry,
and the agent integrations and skills under `/usr/share/memcastle/integrations/` and `/usr/share/memcastle/skills/`.

The package owns only those files and its licence under `/usr`.
Your configuration (`~/.config/memcastle`), palace (`~/.local/share/memcastle`) and state (`~/.local/state/memcastle`)
stay yours, and so does any secret: the package never writes to them.

The unit runs as you, not as root, with your XDG environment.
Authentication is off by default, so it starts without any provisioning:

```sh
systemctl --user start memcastle
systemctl --user status memcastle
systemctl --user restart memcastle
systemctl --user stop memcastle
```

To start it at every login, and now:

```sh
systemctl --user enable --now memcastle
```

Logs are in the journal: `journalctl --user -u memcastle -f`.
To enable authentication under this unit, see [Under systemd](authentication.md#under-systemd).

### Debian, Ubuntu, Fedora and RHEL family (.deb, .rpm)

Each release carries packages for linux-amd64 and linux-arm64:

```sh
sudo apt install ./memcastle_<version>_linux-amd64.deb
sudo dnf install ./memcastle_<version>_linux-amd64.rpm
```

They install `/usr/bin/memcastle`, the systemd user unit `/usr/lib/systemd/user/memcastle.service`,
the bash, zsh and fish completion scripts, `/usr/share/doc/memcastle/`, the bundled sources under
`/usr/share/memcastle/sources/` and the agent integrations and skills under `/usr/share/memcastle/integrations/` and
`/usr/share/memcastle/skills/`, and nothing under your XDG directories,
so removing the package leaves your configuration and palace alone.
The unit is used as on Arch: `systemctl --user start memcastle`, as described in [Arch Linux](#arch-linux-aur).
The package does not enable or start it.
Verify them like any other download, as in [Verify a download](#verify-a-download).

The packages are unsigned, there is no apt or dnf repository so upgrades are manual,
and no dependencies are declared: the binary needs the glibc of its build image,
so a distribution older than that fails at run time rather than at install.

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
└── share/
    ├── doc/memcastle/{LICENSE,README.md}
    └── memcastle/{sources,integrations,skills,web}/
```

Unpack it at a prefix to install it in the same places a package would:

```sh
sudo tar -xzf memcastle_*_linux-amd64.tar.gz --strip-components=1 -C /usr/local
```

The tarball carries the bundled sources, the agent integrations, the skills and the web UI under
`share/memcastle/`, which are package assets.
Do not unpack it at `~/.local`: that would put the package's assets inside your data directory,
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
The releases are built to be reproducible: the toolchain and build environment are pinned and build paths are
remapped, so rebuilding a tag on the same target should give the same bytes.

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
memcastle 0.2.0
```

`memcastle --help` lists every command; the [CLI reference](cli.md) describes them.

## Shell completion

`memcastle completions <shell>` prints a completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish`.
Package installs already include the bash, zsh and fish scripts.
For a binary download or a source build, generate one yourself, for example:

```sh
memcastle completions bash > ~/.local/share/bash-completion/completions/memcastle
```

The [CLI reference](cli.md#shell-completion) has the install location for each shell.

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
