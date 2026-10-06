# ADR-040: Bundled sources are installed from the start, and install and update are for sources from elsewhere

## Status

Accepted.
Amends [ADR-033](033-source-distribution.md) (the bundle is no longer an index, `install` and `update` no longer serve
it, `mining.registries` is no longer empty by default, and an index entry may name a GitHub repository)
and [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) (what "installed" means for a source that
ships with MemCastle).

## Context

ADR-033 shipped `pi` and `opencode` as ordinary packages beside the binary, with an index, and made them *installable
by name with no registry*.
That kept one lifecycle for every origin, and it cost the user a step that decides nothing:
the source is on the same disk as the daemon, so `memcastle source install pi` fetched a file from one directory into
another, asked for consent to the permissions of code the release already carried, and recorded the result.
It also made `update` mean two things (a newer bundle after upgrading MemCastle, or a newer version from a registry),
gave the bundle a trust exemption inside `Registry::fetch`, and left a copy of each package in the sources directory that
a later MemCastle could leave stale.

Meanwhile nothing published a registry: a standalone binary, a checkout or a `cargo install` had no way to get `pi`
at all except from a release archive by hand.

## Decision

- **A bundled source is installed from the start.**
  The release unpacks each package under `share/memcastle/sources/<name>/` (the same `memcastle-source.toml` and
  `source.wasm` an installed source has), and the daemon reads and runs it from there.
  It is listed with the origin `bundled`, in the state `installed`, and `memcastle source enable pi` is all it needs.
  Nothing is copied into `mining.sources_dir()`.
- **The bundle is the source of truth for what a bundled source is; the database keeps only whether it is on.**
  The manifest, the version and the digest come from the bundle on every look, so a new MemCastle that carries a new
  version of a source simply runs it, with no update step and no stale copy.
  The stored row, written the first time a user enables or disables the source, supplies the state and nothing else.
  A row left by an earlier MemCastle that installed the bundled package by name is read the same way.
- **Enabling a bundled source is the consent.**
  It is as trusted as the MemCastle that carries it, which is also why it has no signature to check,
  so `enable` asks for no digest, and `source show` prints what it may do.
  Installing from a file or a registry still needs the exact consent (invariant 10); that is where code arrives from
  outside the release.
  Enable and disable stay REST and CLI only, with no MCP tool, so an agent cannot turn a source on.
- **`install` and `update` are for sources from elsewhere.**
  `install <name>` and `update` consult registries only, and a registry install of a name the release ships is refused
  with `memcastle::source::bundled`, as are `update <name>` and `remove <name>`: the source is already installed,
  it is updated with MemCastle, and `disable` is how it is turned off.
  `update` with no name leaves bundled sources alone, and `search` reports them as installed and never as updatable.
- **A package the user installs from a file still wins.**
  A developer who runs their own build of `pi` installs it with `memcastle source install ./pi`, which is a local
  package of the same name and is read instead of the bundled one;
  removing it brings the bundled one back.
  A registry never replaces a bundled source, since two differently trusted copies under one name would be the
  confusing outcome.
- **No bundle means no bundled sources.**
  A checkout, a standalone binary or a `cargo install` has none, and `source list` simply does not show them.
  A directory holding source *projects* (a manifest and no `source.wasm`, as `sources/` in a checkout does) is not a bundle.
  `mining.bundled_dir` and `assets.dir` still point the daemon at one.
- **The official registry is a static file in the documentation, and is the default.**
  `docs/registry.json` is edited by pull request and published with the documentation site at
  `https://noirbizarre.github.io/memcastle/registry.json`: the documentation workflow builds it on every
  pull request, so a mistake fails there, and deploys on every push to `main`, so the registry changes when a pull
  request is merged and never because a release was cut.
  `mining.registries` defaults to it, so `memcastle source search` and `install <name>` work on any installation, and a
  standalone binary can install the official sources the same way as any other.
  Setting `mining.registries` replaces the default, and `registries = []` opts out.
- **A registry entry may name a GitHub repository instead of listing versions.**
  The registry then says only which repositories publish sources (`pi` and `opencode` name this one),
  and the daemon reads each repository's releases through the GitHub API (`mining.github_api_url`),
  one request for each repository however many sources it publishes.
  The versions are the non-draft, non-prerelease releases' assets named `<name>-<version>.tar.gz` (what `source package`
  writes), and the SHA-256 is the `digest` GitHub computed at upload, which `Registry::fetch` checks the download against
  like any other index digest, before the name and version inside the archive are checked against the entry.
  Publishing a version is a release of the source's own repository, and registering a source is one entry in the file.
  The compatibility a version declares is not known until the archive is read, so the install checks the manifest inside
  it and `search` may offer a version that `install` then refuses.
  A repository that cannot be read is a warning that names it and its sources are left out, so the rest of the registry,
  and an exhausted unauthenticated rate limit, never fail a search outright.
- **The network is still only reached on request.**
  The registry and the repositories it names are read when a user runs `search`, `install <name>` or `update`, never at
  startup or by a background check, and every package they serve is held to its SHA-256 and to `mining.trust` like any
  other registry's.
  A release asset carries no signature, so `trust = "required"` refuses the official packages until they are signed.

## Consequences

- `Registry::fetch` has no exemption: every archive it returns has been checked against the trust policy.
- The release carries each package twice, unpacked in the bundle and as an archive attached to the release.
  The unpacked copy is what the daemon runs; the archive is what a daemon without a bundle installs, and the release
  builds no index.
- A daemon moved from a standalone binary to a packaged one finds the bundled source in the bundle and leaves its
  registry-installed copy in place: the installed copy wins until it is removed.
- The registry does not depend on a release: it is deployed with the documentation, and a release only has to attach
  `<name>-<version>.tar.gz` for the version to be found.
- The integrity of an official package rests on GitHub's digest and TLS, with no signature to check, and on the
  unauthenticated API rate limit (60 requests an hour for an address) for availability.

## Alternatives rejected

- **Install the bundle into the sources directory on first start.**
  It keeps one code path, and keeps the copy that goes stale, the second place to look, and a step that can fail
  at startup.
  Reading the bundle in place has neither.
- **Keep an index in the bundle and only hide the install step.**
  The index existed so that the bundle could be fetched like a registry, and nothing fetches from it any more.
- **Require consent to enable a bundled source.**
  The consent protects against code the user did not choose to receive, and the user chose the release.
  Their decision is to turn it on, and the permissions are one `source show` away.
- **Leave `mining.registries` empty and document the URL.**
  The official sources would then be installable only after editing configuration, on the installations that need them
  most (those with no bundle), and the default would protect nobody: nothing is fetched until the user asks.
- **Build the index in the release and publish it with the documentation.**
  It was the first design, and it made the registry follow MemCastle's release cadence, needed the documentation workflow
  to fetch a release asset, and had to extend the previous release's index so that no published version disappeared.
  A registry that names repositories has none of that: the releases are the list.
- **A hand-maintained index of every version.**
  Every new version of any source would need a documentation pull request, which is the burden the repository entry
  removes, and the digest would be copied by hand.
- **Publish the archives on the documentation site too.**
  It would grow the site by every version of every source and tie their availability to a docs deployment,
  where a release asset is permanent and already attested.
- **Versioned documentation now (the `mike` fork).**
  Zensical's versioning deploys each version to a `gh-pages` branch and replaces the Pages artifact flow, and the registry
  would then live under a version path or need its own step.
  The registry keeps a stable URL, so versioned documentation can follow when Zensical supports it natively.
