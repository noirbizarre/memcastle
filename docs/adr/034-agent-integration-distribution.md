# ADR-034: Integrations ship as bundles under the assets root, and `memcastle integration` copies and registers them

## Status

Accepted.
Amends [ADR-013](013-release-packaging-and-asset-resolution.md) (the assets root gains a second consumer),
[ADR-020](020-skills-are-versioned-with-the-repository.md) (skills are now packaged, because an installed integration
reads them) and [ADR-022](022-integrations-are-bun-packages-tested-against-a-real-daemon.md) (an integration is also
distributed as one bundled file, still tested from its sources).
[ADR-035](035-web-dashboard.md) adds `web/` as a further tree under the same root.

## Context

Issue #200 asks for the Pi and OpenCode integrations to be installable: documented, packaged with releases, and usable
from a package and from a development checkout.
Until now the only way to use one was to point the agent at a path in a checkout of the repository,
which ties an integration to a layout (`skills/` three levels above `src/`), to `node_modules` that nothing installs,
and to a git clone of the version you happen to run.

Four forces shape the answer.
An integration is tightly coupled to the MCP contract, the shared skills and the MemCastle version,
so it should be released with them and not on its own lifecycle.
Installing one edits another program's configuration, so it must be idempotent, report what it changed, preserve
everything else and be removable exactly.
A developer must be able to install what is in their working tree without touching an installed MemCastle, and the code
that does it must be the code users run.
And installing is administrative: it changes what code an agent runs (the reasoning of invariant 10 for sources).

ADR-013 already chose how files that belong to a package are found: an explicit directory, the installed
`share/memcastle`, then what is built in.
ADR-033 put the bundled sources there.
That directory is the natural place for integrations too, and the same rule makes a checkout a valid root.

## Decision

- **One assets root holds everything MemCastle ships.**
  `integrations/<id>/` and `skills/` sit beside `sources/`, under the directory ADR-013 resolves
  (`--assets-dir`, `MEMCASTLE_ASSETS_DIR`, `assets.dir`, else `<prefix>/share/memcastle`).
  A checkout already has exactly that layout, so development mode is `--assets-dir <checkout>` and shares the discovery,
  the manifest and the installer with a package.
  `assets.dir` also becomes the root of the bundled sources when it holds a `sources/memcastle-index.json`
  (it was logged and otherwise ignored), and `mining.bundled_dir` still outranks it.
- **An integration is described by `memcastle-integration.toml`.**
  Its format, version, supported MemCastle and agent ranges (semver), the agent it is for, the files to install and the
  file the agent loads.
  Unknown keys are refused, a newer format is refused whole, and asset paths may not climb out of their directory.
  What a file list cannot express is an adapter chosen by `agent.kind`, so a new agent is code and a new `kind`, and not
  a new manifest format.
- **Releases carry bundles, not sources.**
  `packaging/integrations/build.sh` bundles each integration with `bun build` into one file (the agents' own SDKs stay
  external), so installing needs no `node_modules`, no bun and no npm package.
  The release tarballs, `.deb`, `.rpm`, AUR package and Homebrew formula carry `share/memcastle/{integrations,skills}`,
  and `memcastle_<version>_integrations.tar.gz` carries them alone for the recipes that fetch a single binary.
- **`memcastle integration list|install|update|remove` is local tooling.**
  It reads files and runs the agent's own program; it opens no palace, calls no daemon and uses no network, like
  `memcastle source build`.
  `tests/integration_isolation.rs` holds `src/integration` to touching neither the store, the jobs, the client nor the
  network, and `crate::app`, `mcp` and `api` to not calling it: there is no MCP tool and no route.
- **Install copies, then registers, and records a receipt.**
  The files go to `<data>/memcastle/agents/<id>/`, which is the user's and survives a package upgrade or removal.
  They are staged, swapped in whole and put back if the agent refuses.
  The receipt (`.memcastle-install.json`) holds the version, the origin, a SHA-256 per file and what the agent was told,
  so `list` can tell installed from outdated from modified, and `remove` needs neither the manifest nor the package.
  Content is compared as well as the version, because a development build never changes the version.
- **Registration uses the agent's own mechanism and nothing else.**
  Pi: `pi install`, `pi list` and `pi remove` on the installed directory, so Pi alone edits its `settings.json`.
  OpenCode: one `plugins/memcastle.ts` carrying a marker, removed only when the marker is there, and never overwritten
  when it is not.
  Neither adapter opens `opencode.json`, adds an `mcp.memcastle` entry or writes a token.
- **Compatibility is checked before anything is written**, against `CARGO_PKG_VERSION` and the agent's `--version`.
  An agent whose version cannot be read is refused when the manifest requires a range, because guessing installs
  something that may not load.
- **The skills are installed beside the integration, and the manifest names them.**
  `skill-text.ts` looks for `skills/` next to the bundle first and for the checkout's second, so one source file serves
  both layouts.
  A `[[skills]]` entry per skill replaces the first form's all-or-nothing `[skills] install = true`:
  a `name` alone references the shared `skills/<name>/` of the assets root, so nothing is duplicated, and `local = true`
  takes it from the integration's own `skills/<name>/` for a skill only that integration needs.
  Only the named skills are installed, a named skill without a `SKILL.md` refuses the installation before anything is
  written, and the receipt records the names.
  Both layouts resolve a skill through the same assets root as everything else, and `build.sh` ships an integration's own
  `skills/` inside its package directory.
  The agent is told about the skills where they sit in the installed copy (Pi through `resources_discover`, which an `off`
  session never answers, and OpenCode through `skills.paths`), and the installer never writes into the directories a user
  keeps skills in, so skills stay independently installable and the integration never overwrites one of the user's.

## Alternatives rejected

- **Publishing `memcastle-pi` and `memcastle-opencode` to npm.**
  It splits the release of code that is coupled to the daemon's contract, needs a registry account and a second
  versioning story, and makes the official path depend on a network and a package manager.
  An integration that genuinely needs its own lifecycle can be published later; the bundle does not prevent it.
- **Running `bun install` at install time.**
  It needs bun and the network on the user's machine, and Pi does not install a local package's dependencies anyway.
- **Registering the files in place under the assets root.**
  It breaks the agent whenever the package is upgraded or moved, and ties a development install to a build directory.
- **Editing `settings.json` or `opencode.json` directly.**
  It means owning the formats of two programs we do not control, and a mistake corrupts the user's configuration.
  The agents' own commands, or one marked file, cannot.
- **Asking the daemon to install integrations.**
  The daemon is a memory server and has no business changing another program's configuration; installation must work
  before a daemon has ever run, and a route would make "an agent can change the code it runs" a one-request mistake.
- **A new `integrations_dir` setting.**
  `assets.dir` already means "where the files that ship with MemCastle are", and a second root for the same kind of
  thing would mean two answers to where a package was installed.
- **A `plugin` alias.**
  The issue left it open.
  The concept is an integration with an agent, and an alias for one word would be a second name to document and keep in
  step.
  It can be added without changing anything here.

## Consequences

- A release is larger by the two bundles (about 0.7 MB each) and the skills, and building one needs bun.
  `mise run integrations:check` builds the bundles and installs them from both layouts, so a release that cannot be
  installed fails before a tag.
- The integration's version in `memcastle-integration.toml` is maintained by hand with its compatibility ranges.
  `tests/integration_bundle.rs` checks that each accepts the MemCastle that ships it.
- An installed integration is a copy: a bug fixed in a later release reaches the user through `integration update`,
  not through the package manager alone, and `list` says when one is outdated.
- OpenCode's plugin options live in `opencode.json`, which MemCastle does not edit, so an installed plugin is configured
  through the environment.
- The agents' command-line surface (`pi install`, `pi list`, `pi remove`, `--version`) is now a dependency of the
  installer.
  The tests stand in for the programs, so a change to one of those commands is found by running the installer against
  the real agent, not by the suite.
- Claude Code (#6) adds an adapter and a `kind`, a manifest and a bundle, and nothing to the installer's shape.
