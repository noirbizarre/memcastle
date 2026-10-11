# Architecture Decisions

Records of the decisions that shape this project, and — more usefully — the reasons behind them.
An ADR is written when a choice is hard to reverse or likely to be re-proposed.

The point is not the decision; it is the alternatives that were rejected and why.
A record that only states the outcome saves nobody the argument.

A decision is changed by writing a new ADR that supersedes the old one, never by editing the old one.
The one allowed edit is an *amendment*: a dated note at the end of a record, or a pointer in its Status,
for a change that leaves the decision standing (a mechanism that moved, or a list that grew).
The history is the value.

## Format

`NNN-kebab-case-title.md`, numbered in the order written, with the sections:

- **Status** — Proposed, Accepted, Superseded by ADR-NNN, or Accepted with a pointer to the ADR that amends it
- **Context** — the forces in play, before any decision
- **Decision** — what was decided
- **Alternatives rejected** — what else was on the table and why it lost; a record whose alternatives are
  argued in its Context, or that is about scope, may say so there or use **Non-goals** instead
- **Consequences** — what this costs, including what it makes harder

## Index

- [ADR-001](001-surrealkv-embedded-storage-engine.md) — SurrealKV as the only embedded storage engine in Phase 1
  (C-free claim narrowed by a dated note)
- [ADR-002](002-memory-mode-session-scoping.md) — memory mode is per-session/per-request, never daemon-global
  (gated operations amended by ADR-007)
- [ADR-003](003-checkpoint-as-a-durable-job.md) — checkpoint is a durable job; diary writes are a direct call
  (submission methods renamed by a dated note)
- [ADR-004](004-versioned-database-migrations.md) — versioned MemCastle data migrations,
  decoupled from the SurrealDB engine and storage backend
- [ADR-005](005-timestamp-representation.md) — timestamps are `datetime` when required,
  canonical RFC 3339 strings when optional
- [ADR-006](006-job-leases.md) — running jobs are held by a heartbeat lease,
  not by an assumption of one daemon
- [ADR-007](007-memory-mode-gate-follows-data-access.md) — the memory-mode gate follows what an operation
  reads or writes, not its method name (extended by ADR-018)
- [ADR-008](008-replay-safe-job-resume.md) — resuming a job is replay-safe (mining's identity amended by ADR-023)
- [ADR-009](009-shutdown-drains-jobs.md) — shutdown drains running jobs and hands them back to the queue
- [ADR-010](010-unix-xdg-paths.md) — configuration, data and state follow the Unix XDG layout on Linux and macOS
- [ADR-011](011-split-bind-address-and-port.md) — the listener's address and port are separate settings,
  bound before the daemon does anything else (non-loopback warning added by ADR-014,
  commands renamed by ADR-017)
- [ADR-012](012-status-reports-a-stopped-daemon-and-exits-by-state.md) — `status` answers for a stopped daemon too,
  and its exit code says which state it found (`--json` extended by ADR-015, form by stream amended by ADR-016,
  the start command renamed by ADR-017)
- [ADR-013](013-release-packaging-and-asset-resolution.md) — releases are one binary plus an optional package layout,
  and assets resolve override, installed, embedded
  (`.deb` and `.rpm` packages and shell completions added by dated notes, bundled sources by ADR-033,
  integrations and skills by ADR-034)
- [ADR-014](014-optional-token-authentication.md) — authentication is an optional bearer token, checked at one layer,
  and never an MCP capability (commands renamed by ADR-017)
- [ADR-015](015-database-admin-endpoint.md) — the database admin endpoint is an opt-in listener inside the daemon,
  over its own database handle (`db serve` renamed `db start` by an amendment)
- [ADR-016](016-cli-presentation-follows-the-output-stream.md) — the CLI shows tables, colour and prompts to a terminal
  and plain data to a pipe, with no flag to choose
- [ADR-017](017-daemon-lifecycle-commands-live-under-daemon.md) — the daemon's background lifecycle lives under
  `memcastle daemon` (`start`, `stop`, `restart`), and `serve` stays the foreground server
- [ADR-018](018-palace-hierarchy-management.md) — wings, rooms and drawers are managed through REST and the CLI,
  with cascading transactional deletes, optional drawer names and no MCP tool (re-mining amended by ADR-023)
- [ADR-019](019-shared-integration-contract.md) — integrations share one documented contract and language-neutral
  fixtures, not a framework, and reach MemCastle only over MCP and HTTP
- [ADR-020](020-skills-are-versioned-with-the-repository.md) — agent skills are plain files versioned with the
  repository and installed by copying, and a test holds them to the tools, commands and routes they name
  (packaged and installed with an integration by ADR-034)
- [ADR-021](021-richer-retrieval.md) — retrieval is SurrealDB-native and derived: one HNSW index, one shared scope,
  `search::rrf` fusion, point-in-time validity, drawer supersession and graph expansion, with embeddings from a provider
  or the caller (temporal rule and supersession amended by ADR-032)
- [ADR-022](022-integrations-are-bun-packages-tested-against-a-real-daemon.md) — integrations are self-contained bun
  packages, each with its own small client, tested against a real daemon in their own CI job
  (cross-integration tests amended by ADR-027, bundled releases by ADR-034)
- [ADR-023](023-unified-source-model-for-mining.md) — mining goes through one source model in three stages (acquire,
  normalize, chunk and ingest), with idempotent ingestion and a cursor MemCastle keeps per source; re-mining no longer
  duplicates (`pi-sessions` as a built-in adapter partly superseded by ADR-028)
- [ADR-024](024-entity-extraction-as-an-enrich-job.md) — entity extraction is an enrich job that only adds graph
  records, with provenance on every edge and a closed vocabulary for extracted facts, from a built-in heuristic or
  an external provider (the drawers it reads widened to notes by ADR-031)
- [ADR-025](025-memory-deduplication-and-entity-resolution.md) — deduplication is a conservative domain decision:
  an exact copy in a room is not stored twice, a typo or a case variant is stored and linked with its evidence, and
  entity spelling variants converge while ambiguous names stay distinct; nothing is merged and nothing needs a model
  (amends ADR-008, ADR-023 and ADR-024)
- [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) — mining sources are pluggable as
  WebAssembly components behind the same adapter contract, with explicit consented permissions, a computed lifecycle,
  a `source` command group for development and one conformance suite run against native and WebAssembly sources
  (`pi-sessions` as a built-in adapter partly superseded by ADR-028, distribution completed by ADR-033)
- [ADR-027](027-cross-integration-tests-live-in-a-common-package.md) — tests that need two integrations live in a
  test-only `integrations/common` package that imports their sources, and production code stays per package
  (amends ADR-022)
- [ADR-028](028-pi-history-is-an-installed-webassembly-source.md) — Pi's conversation history is an installed WebAssembly
  source built from `sources/pi/`, and the core has no Pi-specific code; the native `pi-sessions` adapter is removed
  (supersedes part of ADR-023 and ADR-026)
- [ADR-029](029-project-local-configuration.md) — a project declares its wing and room in `.config/memcastle.toml`
  and `MEMCASTLE_WING` / `MEMCASTLE_ROOM`, read by each integration and passed as ordinary arguments; the daemon reads it
  only to choose a mined directory's wing (and the CLI only for `note`, per ADR-031),
  and a project file cannot pick a palace
- [ADR-030](030-opencode-history-is-an-installed-webassembly-source.md) — OpenCode's conversation history is an
  installed WebAssembly source built from `sources/opencode/` that acquires sessions by running the `opencode` command,
  a wider permission than a file grant, so no OpenCode or SQLite code enters the core
- [ADR-031](031-note-capture.md) — `memcastle note` captures a thought as an unnamed drawer of source kind `note`, written
  through a synchronous service like the diary and read for entities like mined content; the CLI resolves the project
  scope itself with the reader the mining adapter shares, and there is no MCP tool
  (amends ADR-029)
- [ADR-032](032-temporal-retrieval-and-history.md) — temporal retrieval is one overlap rule over validity time:
  `current`, `as_of` and an interval (`from` and `until`) are the same clause over a window,
  shared by every ranking and by graph expansion,
  and a superseded drawer records what replaced it so `drawer history` can return how knowledge evolved
  (amends ADR-021)
- [ADR-033](033-source-distribution.md) — sources are distributed as versioned packages through static JSON registry
  indexes (a URL, a `file://` URL or a directory, so offline needs no separate mechanism), pinned by SHA-256 and optionally
  signed with ed25519 under a `mining.trust` policy; bundled, registry and local sources share one lifecycle, `update`
  never widens a source's permissions without consent, and releases bundle `pi`, `opencode` and `claude` as ordinary packages
  (completes ADR-026)
- [ADR-034](034-agent-integration-distribution.md) — integrations ship with MemCastle as bundles (no npm package, no
  `bun install`) under the one assets root beside `sources/` and `skills/`, a checkout being a valid root;
  `memcastle integration list|install|update|remove` is local tooling that copies an integration to the user's data
  directory, registers it through the agent's own mechanism (`pi install`, one marked OpenCode plugin file), records a
  receipt, and refuses what its manifest does not support
  (amends ADR-013, ADR-020 and ADR-022)
- [ADR-035](035-web-ui.md) — the web UI is a Vue and OpenVue client in `web/`, opt-in (`web.enable`) and
  served under `/ui` from `web/dist/` of the one assets root, in a checkout and in a package alike;
  its static shell is the second public path,
  it signs in as the database console does (user `memcastle`, the token as the password, checked by every request),
  carries a memory mode, and a hook holds `web/` to HTTP
  (amends ADR-013, ADR-014, ADR-015 and ADR-034; its read-on-demand rule amended by ADR-041)
- [ADR-036](036-retrieval-evaluation-framework.md) — retrieval is evaluated by an HTTP-only harness under
  `tests/in_process/retrieval_eval/`, so nothing ships in the binary or a release;
  a small bundled dataset runs in the everyday suite against a committed quality baseline,
  vectors come from the harness so engine quality is not confounded with model quality,
  time is seeded in epochs over HTTP, timings are reported and never gated,
  and LongMemEval is a converter for a file the user downloads
  (relates to ADR-021, ADR-032)
- [ADR-037](037-persistent-miner-configuration.md) — miners are named `[[miners]]` definitions in the configuration file
  (source, locator, saved options, a credential *reference*), edited in place by the daemon with comments kept and
  re-read when the file changes, read-only over MCP and administrative over REST and the CLI;
  a cursor belongs to the source a miner points at and not to its name, an option change that may broaden requires
  `--allow-broaden`
  (triggers were stored but not acted on, until ADR-043 replaced them with `[[triggers]]`)
- [ADR-038](038-one-output-contract-for-every-command.md) — every command with a data answer is readable in a terminal
  and JSON in a pipe, and the global `--json` forces JSON on a terminal
  (amends ADR-012, ADR-015 and ADR-016)
- [ADR-039](039-oauth-credentials-for-mining-sources.md) — a mining source that cannot use a static token declares
  `[permissions.oauth]` (a public client, endpoints, scopes; part of the consent digest) and asks the host for an access
  token through a new `host.access-token` function (source contract `0.3.0`);
  the daemon runs the device or browser-with-PKCE sign-in for `memcastle source auth <source>`, keeps the refresh token
  in the platform keyring or an owner-only file and never in the palace, renews it under one lock per source,
  and signing in is administrative with no MCP tool
  (amends ADR-026, builds on ADR-014 and ADR-037)
- [ADR-040](040-bundled-sources-are-installed-from-the-start-and-the-official-registry-is-published.md) — bundled
  sources are unpacked beside the binary, installed from the start and run in place, so enabling is all they need and
  `install` and `update` are for registry sources only; the official registry is a static file in the documentation
  that names GitHub repositories, whose releases are the versions, and is the default `mining.registries`
  (amends ADR-033 and ADR-026)
- [ADR-041](041-server-sent-events-for-dashboard-updates.md) — the daemon announces changes on an in-process bus and
  `GET /api/events` relays them as server-sent events: identifiers and kinds, never content, behind the ordinary
  authentication layer and read gate (`disabled` is refused), a slow connection told to `resync`, the stream ended by
  shutdown;
  the dashboard reads it with `fetch` (not `EventSource`, which cannot send the token) and `useLoad` re-reads quietly,
  keeping the Refresh button for a stream-less session or a shared remote palace
  (amends ADR-035, builds on ADR-006, ADR-007 and ADR-014)
- [ADR-042](042-mine-takes-a-source-and-its-options.md) — `memcastle mine <source> [place] [key=value]...` replaces
  `--source` and `--locator`, with `memcastle mine <path>` kept as the directory shorthand;
  sources declare the options they accept (`[options.<name>]`) and the daemon refuses the rest before queuing,
  the source contract is `0.4.0` (`identify` takes the options, `source-ref` carries them) and the source decides which
  options are identity and which only narrow; a miner's single options table becomes the defaults of its run
  (amends ADR-023, ADR-026 and ADR-037)
- [ADR-043](043-source-triggers.md) — triggers are `[[triggers]]` entries (`schedule`, `poll`, `webhook`, `watch`), disabled
  until the user enables them, that only decide *when* and end in the request `miner run` makes,
  a source declares what it supports (`[triggers.<kind>]`, a capability and not a permission),
  a webhook has its own opt-in, loopback-by-default listener authenticated by a per-trigger secret,
  bursts join a waiting run, deliveries are idempotent and restart-safe, and changing any of it is REST and CLI only
  (amends ADR-037)
- [ADR-044](044-pi-integration-uses-pis-mcp-client-library.md) — Pi 1.0 is the minimum Pi version, and the Pi integration's
  MCP session is built on Pi's own `@earendil-works/pi-mcp` client library (bundled) instead of the MCP SDK
  (amends ADR-022 for Pi)
- [ADR-045](045-codex-history-is-an-installed-webassembly-source.md) — Codex rollout history is an installed WebAssembly
  source built from `sources/codex/`, with read-only access only to the dated rollout directory and no Codex-specific
  code in the daemon core
- [ADR-046](046-fact-lifecycle-and-contradictions.md) — graph assertions keep auditable confirmation, contradiction and
  supersession links; unresolved conflicts remain visible and explicit corrections retain their evidence
  (builds on ADR-024, ADR-025 and ADR-032)
- [ADR-047](047-terminal-operations-console.md) — the TUI reads the same SSE endpoint as the web dashboard and
  performs all controls over REST; a confirmed force-cancel durably requests cancellation before aborting a locally
  owned mining worker and fences its terminal transition against the worker's lease
  (builds on ADR-006 and ADR-041)
- [ADR-048](048-source-preferences.md) — global and palace-local source authority levels merge field-by-field;
  connector-specific metadata criteria shape extraction context, conflict hints and relevant retrieval candidates
  without overriding validity, explicit corrections, confidence or provenance
  (builds on ADR-023, ADR-024, ADR-032 and ADR-046)
- [ADR-049](049-provider-plugins-are-distributed-as-multi-module-releases.md) — a versioned plugin repository distributes
  independently selectable source and integration modules; a separate reviewed catalogue preserves the legacy source
  index, module state remains independent, and provider-scoped CI/releases decouple upstream changes from Core
  (builds on ADR-026, ADR-033 and ADR-034)
