# Plugin packages

This page describes the plugin manifest, archive, registry and module lifecycle contracts.

A **plugin** is a versioned, installable package published by one repository.
A **provider** names the shared upstream implementation family.
A plugin declares zero or more **modules**, each with a stable ID and a type: `source`, `integration` or `embedding_provider`.
A **source kind** is a mining capability exposed by a source module.
A configured **miner instance** selects one source and keeps its own scope and ingestion history.
An **integration instance** is separately installed into an agent from a plugin's integration module.

`docs/plugins.json` is the reviewed plugin discovery catalogue, published at `https://memcastle.github.io/plugins.json`.
It lists repository-level plugin IDs and module inventories; it is not a declaration that a package is safe.
The existing `registry.json` and `memcastle-index.json` remain the format-1 source catalogues for v0.4 consumers.
The new plugin catalogue must never be substituted for either legacy index.
Its format-1 `plugins[]` entries name an `id`, `description`, GitHub `owner/repository` and an inventory of
`{id, type}` modules.
Entries may list signed versions with `version`, archive `url` and `sha256`, or omit `versions` to discover
digest-bearing GitHub Release assets named `<plugin-id>-<version>.tar.gz`.
For a signed static entry, run `memcastle source keygen <private-key>` once, then
`memcastle plugin sign <archive> --key <private-key>` and publish its reported `sha256` and `signature` fields.
Users who require that publisher's signature set `plugins.trust = "required"` and list its public key under
`plugins.trusted_keys`.
An older version with a different module inventory may give its own `modules` list in its version entry.
The root inventory describes the newest release; a pinned older release without indexed module metadata is
identified and checked against the digest of its own archive.

The root `plugin.toml` has `format = 1` and a `[plugin]` table containing `id`, `version`, `provider`,
`description`, `repository` and `license`.
It also declares a top-level `memcastle` version requirement, optional `[[dependencies]]` with `id`, `version`
and `optional`,
and any number of `[[modules]]`.
A required dependency must already be installed at a compatible version; MemCastle does not silently fetch another
plugin while installing this one.
An optional dependency may be absent, but is version-checked when present.
Each module specifies a stable `id`, `type`, semantic `version`, paths `manifest` and `entry`,
SHA-256 values `manifest_sha256` and `sha256`, and optionally `optional`, `config_schema` and a source `contract`.
The parent may also declare `shared_config_schema` and `[authentication]` with a public `kind` and a credential reference;
no login token or client secret is written into the manifest.
Source modules retain their `memcastle-source.toml` and `source.wasm` contract;
integration modules retain their `memcastle-integration.toml` and agent assets.
An embedding-provider module may be declared for discovery, but its execution contract and selection belong to #256.

The tar.gz release archive contains `plugin.toml` and the declared module files under their module directories.
Its manifest and entry bytes must match their declared hashes.
Source and integration module identities and versions must match their own manifests.
For example, one OpenAI plugin can list separate `chatgpt` and `codex` source IDs,
plus an agent integration: those source IDs remain distinct miner kinds with separate cursors.
Jira and Confluence likewise remain separate source modules even if one Atlassian plugin shares API-client code.
`shared_config_schema` and `[authentication]` describe provider-wide configuration and a public credential reference;
each module's own permissions and each miner's scope remain separate.
`memcastle plugin install` makes source modules available but leaves each one installed and inactive.
`memcastle source enable/disable` controls its mining availability independently of other modules.
`memcastle integration install/remove` copies and registers an integration from an installed plugin's local files;
it never fetches a standalone module or needs a daemon to reach its agent.
Embedding-provider selection and runtime execution belong to #256; declaring the type does not activate it.
Plugin removal must preserve drawers, mined source records and cursors.
Module configuration and dependent installations must be resolved before removal.
A plugin update cannot replace the bytes of an already installed version or downgrade a configured release;
publish a new plugin version, keeping stable module IDs and independent module versions.
A mined legacy source requires an explicit `--adopt-source <id>` when moving into a plugin.
Uninstall keeps a source-ID ownership record so a different plugin cannot later inherit retained cursors or drawers.

GitTPL authoring templates are maintained separately as `memcastle-plugin.tpl`, `memcastle-source.tpl` and
`memcastle-integration.tpl`.
The parent composes independently pinned source/integration template revisions for each selected module ID;
an empty initial plugin is valid, and another module can be added by changing the recorded answer and updating GitTPL.
Call `git tpl init` on those repositories directly rather than asking MemCastle to scaffold code.

From a directory containing local clones of all three template repositories:

```sh
git tpl init "$PWD/memcastle-plugin.tpl" example --init \
  --answer plugin_id=example --answer sources=files-one,files-two \
  --answer integrations=assistant \
  --answer "source_template=$PWD/memcastle-source.tpl" \
  --answer "integration_template=$PWD/memcastle-integration.tpl" \
  --defaults --strict-answers --json
# Build every selected source and calculate artifact digests before installing the resulting archive.
python3 example/scripts/package.py
memcastle plugin install example/dist/example-0.1.0.tar.gz
```

Once the template repositories are published, use the three `https://github.com/memcastle/*.tpl` URLs instead;
`git tpl init` then needs `--trust` to allow the pinned child-template clones.

To scaffold only a module, invoke `memcastle-source.tpl` or `memcastle-integration.tpl` directly.
For an existing provider project, edit its recorded comma-separated `sources` or `integrations` answer in
`.config/git.tpl.toml`, then run `git tpl update --defaults --trust`, review `git tpl diff`, and run `git tpl merge`.
The parent manifest pins each child template to a commit, so changing a child on its own does not silently change
an already generated provider project.

An in-tree provider project may live under `plugins/<provider>/` and publish its own `<id>-<version>.tar.gz` release
without changing Core's version.
While several providers share the Core repository, each uses its own `plugin/<provider>/v<version>` tag;
its reviewed catalogue entry lists the artifact URL and digest explicitly.
A direct GitHub repository URL is for a standalone repository whose releases publish exactly one plugin ID;
the Core monorepo's mixed releases must be discovered through `plugins.json`.
GitHub's release-asset SHA-256 protects a direct download's integrity; it is not an ed25519 publisher signature.
With `plugins.trust = "required"`, use a signed catalogue entry instead of an unsigned direct release.
Moving the same project into a standalone repository keeps `plugin.id` and module IDs stable;
update `plugin.repository` and the discovery catalogue through review before releasing from the new repository.
