# MemCastle roadmap

MemCastle is the memory runtime, not an agent plugin: one daemon serves one
palace, over HTTP/MCP, for as many agent clients as connect to it
simultaneously. Agent integrations (Pi, OpenCode, Claude Code) are lifecycle
adapters around that runtime — they decide *when* to call MemCastle, never
*how* memory works. See [`docs/architecture.md`](docs/architecture.md) for the
as-built architecture, [`integrations/README.md`](integrations/README.md) and
[`skills/README.md`](skills/README.md) for the planned integration/skill
layout.

This roadmap is tracked as GitHub milestones and issues, one epic issue per
phase with the detailed work linked underneath. The tables below are a quick
index; the issues themselves carry the actual scope, design decisions, and test
expectations.

## Phase 1 — Align the core

[Milestone](https://github.com/noirbizarre/memcastle/milestone/1) ·
[Epic #3](https://github.com/noirbizarre/memcastle/issues/3)

Preserve every existing foundation (job state machine, scheduler, `AppServices`
seam, embedded/remote store, lexical search) and extend it: meaningful job
priorities, checkpoint as a first-class durable job, minimal knowledge-graph
wiring for checkpoint-originated facts, explicit per-session memory modes, the
recall/wake-up/diary primitives everything downstream depends on, and a
SurrealKV-only embedded store with a versioned migration mechanism. The
choice of a server-side storage engine is deliberately deferred to the server
deployment phase.

Status: complete — every issue below is done.

| # | Title | Done |
|---|---|---|
| [#8](https://github.com/noirbizarre/memcastle/issues/8) | Add `domain::Priority` enum and rework job priority representation | yes |
| [#9](https://github.com/noirbizarre/memcastle/issues/9) | Fix `Scheduler::retry()` to go through `Job::apply` (add `JobEvent::Retry`) | yes |
| [#10](https://github.com/noirbizarre/memcastle/issues/10) | Extend `lexical_search` with an optional wing/room scope | yes |
| [#11](https://github.com/noirbizarre/memcastle/issues/11) | Knowledge-graph store operations: create/supersede/invalidate relationships | yes |
| [#12](https://github.com/noirbizarre/memcastle/issues/12) | Add `JobKind::Checkpoint` and its handler (memory checkpoint as a durable job) | yes |
| [#13](https://github.com/noirbizarre/memcastle/issues/13) | Add `AppServices::diary_write` / `diary_read` (direct call, not a job) | yes |
| [#14](https://github.com/noirbizarre/memcastle/issues/14) | Add `AppServices::recall` and `AppServices::wake_up` | yes |
| [#15](https://github.com/noirbizarre/memcastle/issues/15) | Add `MemoryMode` (full/read-only/disabled) with per-session enforcement | yes |
| [#16](https://github.com/noirbizarre/memcastle/issues/16) | Add `JobKind::Audit` (read-only palace consistency report) | yes |
| [#17](https://github.com/noirbizarre/memcastle/issues/17) | Add `JobKind::Repair` (narrow, dry-run-first) | yes |
| [#18](https://github.com/noirbizarre/memcastle/issues/18) | Introduce a mining source-adapter seam (refactor only, no new sources) | yes |
| [#19](https://github.com/noirbizarre/memcastle/issues/19) | Scaffold `integrations/` and `skills/` directories | yes |
| [#20](https://github.com/noirbizarre/memcastle/issues/20) | Update `docs/architecture.md` and add ADRs for Phase 1 decisions | yes |
| [#44](https://github.com/noirbizarre/memcastle/issues/44) | Versioned database schema and data migrations (SurrealKit-backed schema sync) | yes |
| [#47](https://github.com/noirbizarre/memcastle/issues/47) | Use SurrealKV as the only embedded storage backend | yes |

**Key design corrections captured in these issues** (see the epic and each
issue for full reasoning):

- `pi-palace` routes every MemPalace write through its daemon's job queue to
  dodge a multi-process file-lock race. An embedded MemCastle palace already
  has one process, one writer (AGENTS.md invariant #4), and a remote one is
  protected by job leases (ADR-006) — that specific race doesn't exist here.
  Checkpoint is a job for durability/priority/restart-survival, not lock
  avoidance; diary stays a direct, fast `AppServices` call (#12, #13).
- MemCastle's single-SurrealDB design is explicitly meant to avoid needing a
  `mempalace-rs`-style repair/auto-repair subsystem "by construction" — Audit/
  Repair (#16, #17) are a narrow safety net, not a port of `pi-palace`'s
  `/palace-audit` feature list.
- Memory mode is per-request (HTTP header) / per-MCP-session, never a
  daemon-global switch (#15).

## Phase 2 — Pi integration

[Milestone](https://github.com/noirbizarre/memcastle/milestone/2) ·
[Epic #4](https://github.com/noirbizarre/memcastle/issues/4)

The primary V1 integration — `pi-palace` already proves the behavior; this
phase makes it a thin MemCastle adapter instead of a fork. Depends on Phase 1.

| # | Title |
|---|---|
| [#21](https://github.com/noirbizarre/memcastle/issues/21) | Scaffold `integrations/pi/` package |
| [#22](https://github.com/noirbizarre/memcastle/issues/22) | Port personalized wake-up on session start |
| [#23](https://github.com/noirbizarre/memcastle/issues/23) | Port automatic + manual conversation checkpointing |
| [#24](https://github.com/noirbizarre/memcastle/issues/24) | Port emergency checkpoint before context compaction |
| [#25](https://github.com/noirbizarre/memcastle/issues/25) | Author and inject the search-before-answer skill |
| [#26](https://github.com/noirbizarre/memcastle/issues/26) | Port daily background mining trigger |
| [#27](https://github.com/noirbizarre/memcastle/issues/27) | Implement explicit memory modes (full/read-only/disabled) |
| [#28](https://github.com/noirbizarre/memcastle/issues/28) | Port palace-audit-equivalent manual command (scoped down) |
| [#29](https://github.com/noirbizarre/memcastle/issues/29) | Persistent MCP connection for the session |
| [#30](https://github.com/noirbizarre/memcastle/issues/30) | Actionable, classified failure handling UX |
| [#123](https://github.com/noirbizarre/memcastle/issues/123) | Shared Pi/OpenCode integration contract and conformance fixtures (`docs/integration-contract.md`) |
| [#122](https://github.com/noirbizarre/memcastle/issues/122) | Distribute reusable agent skills: five shared skills, install workflow, `tests/skills.rs` (`docs/skills.md`) |

## Phase 3 — OpenCode integration

[Milestone](https://github.com/noirbizarre/memcastle/milestone/3) ·
[Epic #5](https://github.com/noirbizarre/memcastle/issues/5)

Reuses the same MemCastle operations and skills as Pi, mapped onto OpenCode's
own native lifecycle mechanisms — not assumed to be identical to Pi's. Depends
on Phase 1; may proceed alongside or after Phase 2.

| # | Title |
|---|---|
| [#31](https://github.com/noirbizarre/memcastle/issues/31) | Research spike: map extension/hook/MCP mechanisms to MemCastle ops |
| [#32](https://github.com/noirbizarre/memcastle/issues/32) | Scaffold `integrations/opencode/` package |
| [#33](https://github.com/noirbizarre/memcastle/issues/33) | Implement wake-up on session start |
| [#34](https://github.com/noirbizarre/memcastle/issues/34) | Implement checkpoint and emergency checkpoint |
| [#35](https://github.com/noirbizarre/memcastle/issues/35) | Implement explicit memory modes |
| [#36](https://github.com/noirbizarre/memcastle/issues/36) | Reuse search-before-answer / checkpoint-instructions skills |

## Phase 4 — Claude Code integration

[Milestone](https://github.com/noirbizarre/memcastle/milestone/4) ·
[Epic #6](https://github.com/noirbizarre/memcastle/issues/6)

Pragmatic, best-effort. Explicitly lower priority than Pi/OpenCode — must not
block either.

| # | Title |
|---|---|
| [#37](https://github.com/noirbizarre/memcastle/issues/37) | Research spike: supported extension mechanisms |
| [#38](https://github.com/noirbizarre/memcastle/issues/38) | Pragmatic MCP + skills-based integration |

## Phase 5 — Mining / graph / maintenance expansion

[Milestone](https://github.com/noirbizarre/memcastle/milestone/5) ·
[Epic #7](https://github.com/noirbizarre/memcastle/issues/7)

Everything Phase 1 deliberately kept minimal. Do not start before Phases 1–3
have a coherent working path.

| # | Title |
|---|---|
| [#85](https://github.com/noirbizarre/memcastle/issues/85) | Introduce the unified Source model for mining (done, [ADR-023](docs/adr/023-unified-source-model-for-mining.md)) |
| [#39](https://github.com/noirbizarre/memcastle/issues/39) | Expand mining source adapters beyond the filesystem (done: `pi-sessions`, shared chunker; builds on #85) |
| [#40](https://github.com/noirbizarre/memcastle/issues/40) | Wire real entity/relationship extraction into mining |
| [#41](https://github.com/noirbizarre/memcastle/issues/41) | Expand audit/repair coverage (data-driven) |
| [#42](https://github.com/noirbizarre/memcastle/issues/42) | Richer retrieval: semantic/vector search, temporal + graph-aware ranking |

The concrete platform and agent-history adapters (Slack #86, ChatGPT #87, Claude #88, Codex #89, OpenCode #90,
GitHub #91, Atlassian #92) are implemented on the #85 model, one file each under `src/mining/adapters/`.

## Principles

Use these to resolve any ambiguity not already settled by an issue:

1. MemCastle is the memory runtime, not an agent plugin.
2. The daemon is the coordination boundary.
3. Persistent jobs are the source of truth.
4. The CLI, HTTP API, MCP and integrations are interfaces/adapters.
5. Memory semantics belong in MemCastle; agent lifecycle semantics belong in
   integrations.
6. Pi, OpenCode and Claude Code must share one memory model.
7. One daemon can serve multiple agents simultaneously.
8. Memory must be explicitly disableable per session.
9. A disabled-memory session must behave as if MemCastle does not exist.
10. A read-only session must never mutate the palace.
11. Canonical memory (drawers, facts) is never silently replaced by derived
    information (summaries, embeddings, rankings).
