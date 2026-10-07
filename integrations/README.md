# Integrations

An **integration** is lifecycle/platform glue for one agent ecosystem — it
decides *when* to call MemCastle (session start, before context compaction, on
an interval, on a manual command), never *how* memory works. All memory
semantics — checkpoint persistence, wake-up composition, recall/search,
knowledge-graph mutation, mining — live in the MemCastle daemon (`src/`), behind
`app::AppServices`, and are reused identically by every integration.

See [`docs/architecture.md`](../docs/architecture.md) and `PLAN.md` for the full
rationale.

Every integration satisfies the same conformance matrix, defined in
[`docs/integration-contract.md`](../docs/integration-contract.md): what MemCastle
operation each capability uses, what the client is responsible for, and what to
test. The daemon half is replayed from the language-neutral fixtures in
`tests/fixtures/integration/`; the client half is each integration's own tests.
Where a client has no lifecycle point for a capability, say so in that
integration's README (missing, fallback, effect) instead of faking one.

## Hard rule

An integration talks to MemCastle **exclusively** over its HTTP API and MCP
tools. It must never:

- reimplement memory logic (checkpoint classification into destinations is the
  one exception — see below — everything else is a MemCastle call),
- touch `store`/`jobs` directly, or embed a second copy of the persistence
  layer,
- fork the memory model per ecosystem.

The `integrations-http-only` hook enforces this: it fails on `surrealdb`,
`surrealkv`, `SurrealStore`, a `store`/`jobs` path or `/api/db` anywhere under
this directory (Markdown and `node_modules` excepted).

The one deliberate exception: **deciding what's worth remembering** (which
words in a conversation become a checkpoint item, and which destination —
preference / project / diary / general, plus an optional fact mutation — each
belongs to) requires the
integration's own model and full conversation context, which MemCastle does not
have. MemCastle persists whatever already-classified payload it's given; it
does not classify.

## Layout

```text
integrations/
  pi/            TypeScript/bun — the primary V1 integration (Phase 2); scaffolded
  opencode/      TypeScript/bun — an OpenCode plugin (Phase 3); scaffolded
  claude-code/   MCP + skills only, pragmatic/best-effort (Phase 4); planned
  common/        test-only: the tests that need two integrations side by side, and the harness they share
```

Each TypeScript package is self-contained, with its own `package.json`, lockfile and tests, and carries its own copy of
the small client it needs.
`mise run integrations:check` typechecks and tests every one of them against a real daemon;
see [ADR-022](../docs/adr/022-integrations-are-bun-packages-tested-against-a-real-daemon.md).
`common/` is the one exception to "nothing shared": it holds no production code, only tests that import the other
packages' sources, such as mixed-mode sessions and the proof that an `off` session receives nothing
([ADR-027](../docs/adr/027-cross-integration-tests-live-in-a-common-package.md)).

Each ecosystem uses its own native language/runtime and packaging conventions —
this is not forced into Rust, and there is no shared plugin runtime or package
manager across them. A MemCastle release can still ship the complete
integration ecosystem from this one repository.

## Install and distribution

Users install an integration with `memcastle integration install <agent>`; see
[Agent integrations](../docs/integrations.md).
Each integration here carries a `memcastle-integration.toml` (id, version, the MemCastle and agent versions it supports,
the files to install), and `mise run integrations:build` bundles its sources into `dist/` (not committed).
Releases ship those bundles under `share/memcastle/integrations/`, with no npm package: a bundle has its dependencies
inlined, and the agents' own SDKs stay external.
The same command installs from this checkout with `--assets-dir "$PWD"`, so what you test is what ships
([ADR-034](../docs/adr/034-agent-integration-distribution.md)).

## Skills

An integration never keeps its own copy of agent instructions: it loads them from [`skills/`](../skills/README.md),
which is also installable on its own (see [Agent skills](../docs/skills.md)).
An integration's `memcastle-integration.toml` names the skills it exposes, a `[[skills]]` entry each: the shared ones by
name, and one that only this integration needs as `local = true` from its own `skills/` directory.
`memcastle integration install` copies them into the installed copy, so no separate skill installation follows.
What stays here is the lifecycle: when to load a skill, when to call MemCastle, and loading none into a session whose
memory mode is off.

## Per-ecosystem findings

- [`opencode/docs/research.md`](opencode/docs/research.md) maps OpenCode's plugin hooks, MCP client and skills onto
  MemCastle operations, and records where OpenCode has no clean equivalent.

## Non-goals

No plugin marketplace, no dynamic plugin runtime, no package manager across
ecosystems — this is plain directories with markdown and each ecosystem's own
native tooling, nothing more. `memcastle integration` copies a bundle and uses the agent's own command to register it;
it is not a package manager and resolves nothing.

## Status

The Pi and OpenCode packages are scaffolds in that two capabilities are still unbuilt, but the rest works: their
connection, mode, discovery and failure handling are tested, and wake-up on session start (#22, #33), search-before-answer with the shared skills
(#25, #36), checkpointing (interval and manual in both, #23 and #24; emergency in both, #34) and the project context
(#183) are implemented in both.
The manual audit/repair command is implemented in both (#28, #127); background mining is not built in either, by design: the daemon keeps that schedule when the user enables a [trigger](../docs/triggers.md) (#26 and #125, superseded by #189).
Explicit memory modes (`full`, `read-only`, `off`) are complete in both (#27, #35): `read-only` never attempts a write
and `off` registers nothing, proved on the wire in `common/`.
The persistent MCP session is complete in both: one connection for the whole session, kept alive, and replaced with its
mode re-selected if the daemon forgets it.
Tracked as GitHub issues under the
"Phase 2 — Pi integration", "Phase 3 — OpenCode integration", and
"Phase 4 — Claude Code integration" milestones.
