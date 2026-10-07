# ADR-013: Releases are one binary plus an optional package layout, and assets resolve override, installed, embedded

## Status

Accepted, amended by dated notes under Alternatives rejected
(2026-09-30: `.deb` and `.rpm` packages; 2026-10-03: completions)
and by [ADR-033](033-source-distribution.md), whose bundled sources are the first package asset
(the "no `share/memcastle/` in 0.1" statements below describe the original decision)
and by [ADR-034](034-agent-integration-distribution.md), whose integrations and skills are the second and third package
assets and make `assets.dir` the one root for all of them
and by [ADR-035](035-web-ui.md), whose dashboard is the consumer the resolver was built for

## Context

MemCastle ships as a single executable that embeds its database, and that must stay true:
a downloaded binary has to start a daemon on a machine with nothing else installed and no network.

But not everything a release may carry belongs inside the executable.
A web UI is large, changes independently of the daemon, and is exactly what an OS package manager is good at installing.
A service unit is a property of the init system, not of the binary.
Neither exists yet, and the packaging contract has to be settled before they do,
or each will force a redesign of how releases are built and found at runtime.

Three kinds of file are involved, and confusing them is the expensive mistake:

- Small files that must match the binary's version exactly, where a mismatch is a corrupted database or a crash.
  The SurrealDB schema and the data migrations are these.
  They are already compiled in, through `surrealkit::embed_schema!` and `crate::migrate` (ADR-004).
- Files a package manager can own: a web UI, documentation, a service unit.
  Updating them must not require rebuilding the binary, and uninstalling them must not touch anyone's data.
- The user's own configuration and palace, under the XDG directories (ADR-010).
  These outlive every install and upgrade.

There is also a reproducibility goal for 0.1.0.
A release that cannot be rebuilt to the same bytes cannot be checked by anyone but its builder.

## Decision

- **Three categories, never mixed.**
  *Embedded* assets are compiled into the binary.
  *Installed* assets live in a package-owned directory, `share/memcastle` under the install prefix.
  *User data and configuration* stay under the XDG directories and are never treated as assets.
- **The native-package layout is fixed.**
  On Linux: `/usr/bin/memcastle` and `/usr/share/memcastle/` for package assets, `/usr/share/doc/memcastle/` for documentation.
  Configuration is `~/.config/memcastle`, data `~/.local/share/memcastle` and state `~/.local/state/memcastle`,
  as before, and a package installs nothing there.
  The release tarball has the same shape (`bin/`, `share/doc/memcastle/`) so unpacking it at a prefix is an install.
  There is no `share/memcastle/` in 0.1, because nothing in 0.1 is a package asset.
- **One rule for finding assets, in this order.**
  1. An explicit directory: `--assets-dir`, `MEMCASTLE_ASSETS_DIR` or `assets.dir`, with the usual precedence among them.
  2. The installed directory: `<prefix of the executable>/share/memcastle`, then `/usr/local/share/memcastle`,
     then `/usr/share/memcastle`; the first that exists.
  3. The assets embedded in the binary.
- **An explicit directory never falls through.**
  If it does not exist, the daemon refuses to start with `memcastle::assets::not_found`.
  A typo that silently served the installed files would be found by whoever noticed the wrong UI.
- **The resolver never returns user data.**
  An installed candidate at or under the XDG data directory is discarded.
  `~/.local/bin/memcastle` would otherwise resolve to `~/.local/share/memcastle`,
  which is where the user's palace lives.
- **Startup never touches the network.**
  Resolution reads directories and nothing else.
  If remote asset fetching is ever wanted, it is an explicit command, not something the daemon does on first start.
- **The resolver is built now, and serves nothing.**
  `assets::Assets` picks the source at startup and logs it; no HTTP route uses it yet.
  The web UI, when it lands, adds a consumer and, if needed, an embedded fallback entry.
- **Service integration is the package's job.**
  The binary generates no unit file and installs none.
  A package may install a systemd user unit; `docs/daemon.md` documents the one to write by hand.
- **Release artifacts are built to be reproducible, and say who built them.**
  The toolchain, `cross` and the runner images are pinned, and every build path is remapped to a fixed name.
  Tarballs are assembled with a fixed order, commit-time mtimes and no owner, and gzip without a header timestamp.
  Each release carries `SHA256SUMS`, a CycloneDX bill of materials and GitHub build-provenance attestations.
  No CI job rebuilds the release to compare the bytes: two LTO release builds take far too long for a pull request check.
  Reproducibility is a property of how the release is built, and anyone can verify it by rebuilding the tag.
- **SurrealKV is the only storage engine in the tree, and a test says so.**
  `tests/dependencies.rs` reads `Cargo.lock` and fails on RocksDB, TiKV, FoundationDB, IndexedDB or any
  SurrealDB engine crate other than SurrealKV and the in-memory engine used by tests.

## Alternatives rejected

- **Embed everything, the web UI included.**
  Simplest, and right for small files, which is why the schema and migrations are embedded.
  For a UI it makes every UI fix a daemon release, bloats a binary that most users run headless,
  and leaves the package manager nothing to manage.
- **Download assets on first start.**
  It would make the binary small and the UI current, and the daemon unusable offline, in an air-gapped build
  or behind a proxy that blocks the download.
  Releases would also stop being reproducible, since a start would depend on what a server returned that day.
- **Ask the installed directory first, the override second.**
  A packaged install would then always win, which makes developing against a local web build impossible
  without uninstalling the package.
- **Merge the override with the installed and embedded assets file by file.**
  Flexible, but which file a user sees would depend on which directories exist,
  and two machines with the same version could disagree.
  The resolver picks one source; only the embedded table is consulted per file, as the fallback.
- **Use `XDG_DATA_DIRS` for the installed directory.**
  It is the standard search path, but it is user-controlled and often includes directories that are not package-owned,
  which defeats the point of a deterministic lookup.
  The three fixed candidates cover the prefixes packages actually use.
- **Ship `.deb` and `.rpm` packages now.**
  There is nothing to put in them beyond the binary that the AUR and Homebrew recipes already install.
  The layout is the contract; the formats can follow when a second installable file exists.
  *Amended 2026-09-30: the release now attaches `.deb` and `.rpm` packages built with nfpm,
  with the same contents as the tarball plus the systemd user unit.
  They are unsigned, have no apt or dnf repository, and declare no dependencies.
  The Arch package installs the same unit.*
  *Amended 2026-10-03: the `.deb`, `.rpm` and Arch packages also install the bash, zsh and fish completion scripts,
  generated from the binary.*
- **Generate the systemd unit from the binary, such as `memcastle install-service`.**
  It couples the core binary to one init system and to paths it cannot know, and it is a packager's responsibility.
- **Only pin the toolchain.**
  Path leaks and runner-image drift change the bytes as surely as the compiler does.

## Consequences

- A standalone binary needs no assets directory and starts offline.
  Packaged and standalone installs behave the same until a package actually ships an asset.
- A new package asset needs three things: a file under `share/memcastle/` in the package,
  a consumer that asks `Assets::find`, and, if the binary must work without the package, an entry in the embedded table.
- Unpacking the tarball at `~/.local` overlaps the XDG data directory as soon as a package asset exists,
  because `share/memcastle` would land in `~/.local/share/memcastle`.
  The resolver ignores that directory on purpose, so such an install has to name its assets with `--assets-dir`
  or unpack somewhere else, such as `/usr/local` or `~/.local/opt/memcastle`.
  In 0.1, with no package assets, nothing is affected.
- Reproducibility is designed in but not enforced by CI.
  A native Linux release build was checked by hand to give identical bytes from two checkout paths,
  but a change that breaks that (a build script embedding a path, a dependency stamping the time) is not caught
  until someone rebuilds a release.
  The macOS and Windows builds, and the Linux ones built under `cross`, are pinned and attested,
  but their linkers and SDKs embed details the workflow does not control, so identical bytes are expected, not verified.
- The release workflow is stricter about versions: bumping the Rust toolchain is a deliberate edit to
  `RUSTUP_TOOLCHAIN` and the `dtolnay/rust-toolchain` refs together.
- `aws-lc-sys`, a TLS dependency, compiles C and can need `cmake`, so the build is C-free for storage only (ADR-001).
