# Contributing to memcastle

## Setup

The toolchain is managed by [mise](https://mise.jdx.dev/);
every tool is pinned to an exact build in `mise.lock`, so a local run and a CI run use the same versions.

```bash
mise install          # the tools
prek install          # the Git hooks
mise run ci           # the local equivalent of CI's lint, test and docs steps
```

Rust itself is not managed by mise: `rust-toolchain.toml` pins the channel, and rustup installs it.
CI uses `dtolnay/rust-toolchain` because it needs per-job components and cross-compilation targets.

## Tasks

| Command | What it does |
|---|---|
| `mise run default` | Format, lint, build and test |
| `mise run build` | Build the binary |
| `mise cli <args>` | Run `memcastle` from source (passes flags through) |
| `mise run setup` | Install `memcastle` into `~/.cargo/bin` |
| `mise run test` | Run the basic tests, without the WebAssembly suite (accepts nextest selectors) |
| `mise run test:wasm` | Run the WebAssembly suite, every `tests/wasm_*.rs` binary (slow, needs the `wasm32-wasip2` target) |
| `mise run sources:check` | Build and conformance-test every source under `sources/` |
| `mise run sources:test -- <name>` | Build and conformance-test one source under `sources/` |
| `mise run sources:package` | Package the sources that ship with releases, with their index, into `target/bundled-sources` |
| `mise run integrations:check` | Typecheck and test every package under `integrations/` against a real daemon (needs bun) |
| `mise run cover` | Run the tests with coverage |
| `mise run format` | Format |
| `mise run format:check` | Check the formatting without rewriting |
| `mise run lint` | Clippy, warnings denied |
| `mise run lint:actions` | actionlint over the workflows |
| `mise run lint:md` | markdownlint over AGENTS.md, CONTRIBUTING.md, README.md, docs/ and the skills' `SKILL.md` files |
| `mise run spell` | typos |
| `mise run guards` | The architecture guard hooks (`store-isolation`, `single-writer`, `job-status-only-via-apply`, `no-hand-rolled-ddl`, `integrations-http-only`) over the whole tree |
| `mise run snapshots` | Review pending insta snapshots |
| `mise run check` | Every lint, the guards, both test suites, the sources and the integrations, without modifying the working tree |
| `mise run ci` | `check` plus the documentation build |
| `mise run docs` | Serve the documentation locally |
| `mise run docs:build` | Build the documentation |
| `mise run changelog` | Preview the changelog for unreleased commits |
| `mise run release` | Show the version the next release would take |
| `mise run ship:validate` | Check the release setup against what gh-ship requires |
| `mise run tpl:check` | Has the template moved? Exits 1 if merging it would change anything |
| `mise run tpl:update` | Bring the rendered template ref up to date |
| `mise run tpl:diff` | Show what merging the template would change |

`mise <task>` is a shorthand for `mise run <task>`, but a builtin subcommand of the same name wins it silently —
which is why the format task is `format` and not `fmt` (`mise fmt` formats `mise.toml`),
and why running the binary is `mise cli` and not `mise run` (`mise run` runs a task).
Prefer the explicit `mise run <task>` in scripts: mise can claim a new name in any release.

## Documentation

Documentation is part of the change, not a follow-up:
a pull request that changes a flag, a setting, an MCP tool or a user-visible behaviour updates the page that describes it.
User pages live in `docs/`, are listed in the `nav` of `zensical.toml`, and use one sentence per line.
Architecture diagrams are Mermaid.
[Development](docs/development.md#documentation) has the conventions, and `mise run docs` previews the site.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/), enforced by commitlint on `commit-msg`.
The type selects the changelog section, and a `!` or a `BREAKING CHANGE:` footer drives the version bump —
so the message is part of the release, not paperwork around it.

## Releases

Releases are run by [gh-ship](https://github.com/noirbizarre/gh-ship).
**Never bump a version or push a tag by hand.**

1. A push to `main` triggers 🚢 Ship, which runs `gh ship prepare`.
2. `prepare` dispatches 🚀 Prepare Release,
   which asks git-cliff for the next version, writes `CHANGELOG.md`, bumps `Cargo.toml`, commits,
   and uploads a `ship.release.json` artifact describing what would ship.
3. gh-ship opens (or updates) the Release PR from `release/next`.
   Review it.
4. Merging it triggers 🚢 Ship again, which runs `gh ship release`: it tags the merge commit, creates a draft release,
   dispatches 📦 Publish Release to attach the binaries, then makes the release public.

Nothing to release is the normal case for step 1, and costs one workflow run reporting `changed: false`.

`gh ship validate` runs on every pull request, so a broken release contract fails on the PR rather than mid-release.

### What a release contains

📦 Publish Release attaches, for a tag `<tag>`:

- the raw executables, `memcastle_<tag>_<platform>[.exe]`, which the AUR recipe, the Homebrew formula and
  `cargo binstall` fetch by name, so renaming them breaks those;
- a tarball for each Unix platform, `memcastle_<tag>_<platform>.tar.gz`, in the native-package layout
  (`bin/`, `share/doc/memcastle/`, `share/memcastle/sources/`);
- the bundled sources on their own, `memcastle_<tag>_sources.tar.gz`, which one asset serves to the AUR package;
- `.deb` and `.rpm` packages for linux-amd64 and linux-arm64, `memcastle_<tag>_<platform>.{deb,rpm}`, built by nfpm
  from `packaging/nfpm/nfpm.yaml` (binary, systemd user unit, shell completions, documentation and bundled sources)
  and smoke-tested before upload;
  CI builds them from stub binaries on every pull request, through the same `packaging/nfpm/build.sh`,
  so a broken packaging config fails on the pull request and not mid-release;
- `memcastle-<tag>.cdx.json`, a CycloneDX bill of materials;
- `SHA256SUMS` over all of the above, with build-provenance attestations for every file and an SBOM attestation for the
  executables and tarballs, which `gh attestation verify` checks.

The release is built to be reproducible, and two things keep it that way.
The Rust toolchain, `cross` and the runner images are pinned to exact versions,
so moving any of them is an edit to the workflow and not something that happens between two releases.
Build paths are remapped to fixed names, and the tarballs have a fixed order, timestamps and ownership.
CI does not rebuild the release to check this, because two LTO release builds are too slow for a pull request.
Bump the toolchain in `RUSTUP_TOOLCHAIN` and the `dtolnay/rust-toolchain` refs together.
See [ADR-013](docs/adr/013-release-packaging-and-asset-resolution.md) for the reasoning.

The pinned release workflow is template-owned (see below), so a change that belongs to every project generated
from the template should be made in rust.tpl.

### Repository requirements

The release jobs authenticate as a GitHub App, not with `GITHUB_TOKEN` —
the default token cannot trigger workflows, so a Release PR it authored would show no CI results.
That means the repository needs:

- a `release` environment holding the variable `APP_CLIENT_ID` and the secret `APP_PRIVATE_KEY`.
- squash-merge settings of `squash_merge_commit_title: PR_TITLE` and `squash_merge_commit_message: BLANK`,
  so the squash commit subject is the Conventional Commit title from `.github/ship.yml`.
- a `homebrew` environment holding the secret `TAP_TOKEN`, scoped to push to `noirbizarre/homebrew-tap` only.
- an `aur` environment holding the secret `AUR_SSH_PRIVATE_KEY`,
  for the AUR account that owns the `memcastle-bin` package.

## This repository is generated from a template

The toolchain, hooks, CI and release workflows come from [rust.tpl](https://github.com/noirbizarre/rust.tpl):

```bash
mise run tpl:check     # is there a template update pending?
mise run tpl:update    # advance refs/tpl/<id> — HEAD, index and worktree untouched
mise run tpl:diff      # read what merging it would change
git tpl merge          # take it
```

`tpl:update` is safe to run at any time: it only advances the rendered ref.
Nothing reaches your branch until the merge.

Requires git-tpl, which `mise install` provides (it is pinned in `mise.toml`'s `[tools]`,
installed from a prebuilt release archive rather than compiled).

Files carrying template-owned content — `mise.toml`, `prek.toml`, `Cargo.toml` —
end with a `# --- project-specific ...` marker (`project-specific tasks` in `mise.toml`, `project-specific hooks` in `prek.toml`).
Add below it;
Git's 3-way merge then preserves your additions across updates.
A fix that belongs to every project belongs in the template, not here.
