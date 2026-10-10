# ADR-049: Provider plugins are distributed as multi-module releases

**Status:** Accepted

## Context

ADR-026 runs a single source WASM component behind `SourceAdapter`,
ADR-033 distributes one source package per registry entry,
and ADR-034 installs an agent integration from the Core assets root.
None is a unit of release for an upstream provider that supplies several sources and an agent integration.
Coupling every upstream compatibility fix to a Core version prevents independent provider CI and releases.

## Decision

One standalone provider repository publishes one versioned plugin archive with a root `plugin.toml`.
During the in-tree transition, multiple provider directories under `plugins/` use independent version tags and
explicit catalogue asset URLs instead of treating the Core repository as a direct one-plugin discovery URL.
The manifest owns stable plugin and module IDs, compatibility, dependencies, checked artifact paths and per-module types.
Module-specific source and integration manifests remain authoritative for their own runtime contract and permissions.
The plugin catalogue at `plugins.json` is reviewed discovery metadata with a module inventory,
not a safety endorsement; the existing `registry.json` remains an unchanged format-1 source catalogue.

The daemon verifies an indexed or GitHub-reported release digest before reading its archive,
checks the manifest against catalogue metadata, and applies the configured plugin signature policy.
A direct GitHub repository URL and a local built project/archive are also explicit installation inputs.
The install stages an immutable generation and transactionally publishes package ownership and source module records.
Adding a module leaves it inactive; upgrading keeps the existing source module's state and credential agreement
unless its permissions change.
Removing or changing the type of a module requires disabling it and removing configured miners or agent registrations.
Uninstall refuses dependents, active jobs, configured miners and enabled sources.
It deletes installation records and credentials, never mined drawers, source documents or their cursors.

Source modules continue through ADR-026's WASM host and the unchanged mining pipeline.
Agent integrations continue to be installed locally by the CLI, from a selected installed plugin generation;
there is no route or MCP tool that registers an integration in an agent.
`embedding_provider` is a recognised manifest type whose selection/runtime contract is left to #256.

GitTPL templates live in three independent repositories.
The parent template composes pinned source and integration template revisions repeatedly from recorded module IDs,
allowing the same generated project to run under `plugins/<provider>/` or in a standalone repository.
Core and provider CI/release workflows select their own changed paths and independent version tags.

## Consequences

An old source package remains available through the legacy REST/CLI commands without a data rewrite;
it cannot be silently claimed by a new plugin of the same source name.
A removed legacy source with mined history needs `--adopt-source <id>` to make the handoff explicit.
An uninstalled plugin keeps the source-ID ownership marker so another publisher cannot inherit its preserved cursor.
The package is the unit of download and trust, while enablement, configured miner scope,
integration registration and source cursor remain independent.
Changing the GitTPL child revision is explicit in the parent manifest and produces a normal Git merge for the author.

## Alternatives

A plugin per source kind would duplicate provider authentication and force Jira and Confluence into different releases.
Reinterpreting `registry.json` as a plugin index would break format-1 clients and their published digests.
Making integrations implement `SourceAdapter` would give agent-side lifecycle code permissions it does not need.
