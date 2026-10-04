# ADR-002: Memory mode is per-session/per-request, never daemon-global

## Status

Accepted; the list of gated operations is amended by [ADR-007](007-memory-mode-gate-follows-data-access.md),
and the session-mode mechanism by the amendment (2026-09-29) at the end of this record.

## Context

One MemCastle daemon serves many agent sessions simultaneously (PLAN.md principle 7).
Memory must be explicitly disableable per session (principle 8),
and a disabled session must behave as if MemCastle doesn't exist (principle 9) — task brief §22.

Two client transports need this enforced differently: HTTP can carry a value on every request;
MCP's tool-call model has no per-call slot for it, only a persistent session id.

Two failure modes had to be designed against:
a daemon-global toggle that would silently affect every other connected agent,
and an ambiguous "found nothing" read response for a disabled session
that is indistinguishable from a genuinely empty result.

## Decision

`domain::MemoryMode` is a closed three-value enum (`Full`/`ReadOnly`/`Disabled`),
not a per-capability flag set (task brief §21 explicitly deferred).
Enforcement lives in exactly one place —
`app::AppServices::require_read`/`require_write`, checked before any store contact
by the seven memory-content operations (`search`/`recall`/`wake_up`/`diary_read` for reads;
`checkpoint`/`emergency_checkpoint`/`diary_write` for writes).
`api` and `mcp` only ever *extract* a `MemoryMode` and pass it down; neither independently decides what's allowed.

Administrative/daemon-level operations (`status`, job listing/control, `submit_mine`, `submit_demo`,
and — provisionally — `Audit`/`Repair`) are never gated:
a disabled session still sees daemon/job state and can submit background work.
*(Amended by ADR-007: job listing/inspection, `mine` and applied `repair` are gated after all,
because they read or write memory content.)*

`Disabled` is deliberately symmetric:
reads are rejected with the same typed `Error::ModeForbidden` as writes, never a silent `Ok(empty)`.

- **HTTP**: `api::ModeHeader`, an `axum::extract::FromRequestParts` extractor,
  reads `X-MemCastle-Mode` once per request, defaulting to `Full` when absent
  so pre-existing/unaware clients see unchanged behavior.
  An unparsable value is a 400, never silently downgraded to `Full`.
- **MCP**: mode is negotiated once per session via a `memcastle_set_mode` tool call,
  cached in `Arc<DashMap<session id, MemoryMode>>` inside `McpTools`,
  keyed by the `mcp-session-id` header rmcp's streamable-HTTP transport assigns —
  reusing the same map-of-controls idiom `jobs::Scheduler` already uses for job control.
  A session that never calls `memcastle_set_mode` defaults to `Full`.

## Alternatives rejected

- A daemon-global `MEMCASTLE_ENABLED=false` flag — the task brief rules this out explicitly:
  the daemon keeps running, only the opting-out client stops using memory;
  every other client's in-flight jobs/reads are unaffected (`tests/in_process/concurrency.rs` asserts this).
- Per-capability mode flags (separate toggles for search vs. checkpoint vs. diary) —
  out of scope for V1, a simplicity call, not a discovered constraint.
- An empty `Ok` result for `Disabled` reads instead of an error —
  rejected because it's indistinguishable from "genuinely found nothing,"
  which would defeat the context-isolation guarantee that no MemCastle-derived content may reach a disabled session.
- Silently downgrading an unparsable `X-MemCastle-Mode` header to `Full` —
  rejected as a safety regression disguised as permissiveness.
- Gating administrative operations (`status`, job control, `Audit`/`Repair`) by mode —
  rejected as conflating "automatic memory operations" with "explicit daemon operations";
  a disabled session must still observe and manage daemon/job state.

## Consequences

- The MCP session-mode cache is purely in-process (never written to `SurrealStore`)
  and does not survive a daemon restart — unlike the durable job queue.
  In practice this is likely moot:
  rmcp's `LocalSessionManager` session ids are themselves in-memory and process-local,
  so a reconnecting client gets a new session id and the same Full-by-default cold start a first-time caller gets,
  rather than a stale-mode bug —
  but this interaction between two independently-owned ephemeral designs isn't itself tested,
  and should be if either side changes.
- HTTP's per-request header has no such caveat —
  every request re-supplies its own mode, so there's nothing to lose on restart.
  This makes HTTP's model strictly more robust than MCP's session cache,
  an accepted trade-off imposed by MCP's protocol shape.
- Callers must treat `Error::ModeForbidden` distinctly from a generic failure,
  since "no results" and "not allowed to read" are only distinguishable through the error type, by design.
- `Audit`/`Repair` remaining ungated is provisional
  (open to future reclassification, and recorded here rather than in `domain::memory_mode`), not a settled boundary.
  *(Amended by ADR-007: an applied `repair` is gated; a dry run and `audit` are not.)*

## Amendment (2026-09-29): how the session's mode is held

The decision above stands; only the mechanism changed (issue #72).
rmcp constructs one `McpTools` per session and drops it when the session closes,
so the per-session map described under Decision never held more than one entry.
It is now a single slot tagged with its session id, which makes the "nothing outlives the session" property structural.
A request without an `mcp-session-id` (a stateless transport) is treated as having no session:
it runs as `Full`, is never cached, and `memcastle_set_mode` refuses it,
instead of every such request sharing one empty-string entry and changing each other's mode.
