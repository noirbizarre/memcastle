# Integrations

An **integration** is lifecycle/platform glue for one agent ecosystem — it
decides *when* to call MemCastle (session start, before context compaction, on
an interval, on a manual command), never *how* memory works. All memory
semantics — checkpoint persistence, wake-up composition, recall/search,
knowledge-graph mutation, mining — live in the MemCastle daemon (`src/`), behind
`app::AppServices`, and are reused identically by every integration.

See [`docs/architecture.md`](../docs/architecture.md) and `PLAN.md` for the full
rationale.

## Hard rule

An integration talks to MemCastle **exclusively** over its HTTP API and MCP
tools. It must never:

- reimplement memory logic (checkpoint classification into destinations is the
  one exception — see below — everything else is a MemCastle call),
- touch `store`/`jobs` directly, or embed a second copy of the persistence
  layer,
- fork the memory model per ecosystem.

The one deliberate exception: **deciding what's worth remembering** (which
words in a conversation become a checkpoint item, and which destination bucket
— preference / project / diary / fact — each belongs to) requires the
integration's own model and full conversation context, which MemCastle does not
have. MemCastle persists whatever already-classified payload it's given; it
does not classify.

## Layout (planned)

```text
integrations/
  pi/            TypeScript/bun — the primary V1 integration (Phase 2)
  opencode/      TypeScript — native OpenCode extension mechanism (Phase 3)
  claude-code/   MCP + skills only, pragmatic/best-effort (Phase 4)
```

Each ecosystem uses its own native language/runtime and packaging conventions —
this is not forced into Rust, and there is no shared plugin runtime or package
manager across them. A MemCastle release can still ship the complete
integration ecosystem from this one repository.

## Status

No integration code exists yet. Tracked as GitHub issues under the
"Phase 2 — Pi integration", "Phase 3 — OpenCode integration", and
"Phase 4 — Claude Code integration" milestones.
