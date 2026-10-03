# ADR-019: Integrations share one documented contract and language-neutral fixtures, not a framework

## Status

Accepted, builds on [ADR-002](002-memory-mode-session-scoping.md) (the mode is per session, which the contract's first
capability depends on) and [ADR-014](014-optional-token-authentication.md) (an integration never handles credentials
over MCP)

## Context

Pi and OpenCode are the first two agents to get a MemCastle integration, and Claude Code follows.
Each has its own lifecycle, its own language and its own extension mechanism, and each was tracked as its own set of issues
with the same capabilities repeated in each: wake-up, recall, checkpoint, memory modes, background mining, failure
reporting and audit.

Left alone, the same questions are answered twice.
What does `off` mean on the wire?
Which failures must a client tell apart?
What may a client assume about a checkpoint it just submitted?
The answers would drift, and the second client would find out what the first one already knew only by rediscovering it.

Two forces pull in opposite directions.
The behaviour behind these questions is MemCastle's, and should be defined and tested once, against a real daemon.
The lifecycle is the client's: Pi can hook before compaction, another client may have no such point, and pretending
otherwise would put client logic into the core to manufacture a parity that does not exist.

The daemon also knows less about a session than the issues assume.
It has an MCP session id that carries the mode and a free `agent_identity` string, and no session entity of its own.
The wire name for the "off" mode is `disabled`.

## Decision

- **One contract page,** [Integration contract](../integration-contract.md), holds a conformance matrix.
  Each row is a capability with the MemCastle operation, the client's responsibility and whether a gap is allowed.
  The page lives under `docs/` so it is published, linted and linked, rather than next to the adapters where it would drift.
- **The fixtures are strict JSON** under `tests/fixtures/integration/`: the capability manifest, the mode rules,
  the checkpoint payloads and the failure classes.
  JSON is read by any language, so the Pi and OpenCode suites replay the same files the Rust suite does.
- **The daemon half is proved once,** by `tests/integration_contract.rs`, against a real daemon over a real MCP session.
  Each capability names its test in the manifest, and a test fails when the manifest, the page and the test names disagree.
- **The client half stays with each client.**
  When a client calls an operation, and whether a disabled session leaks context,
  can only be tested where the lifecycle is.
  The contract lists what each client must test and does not test it for them.
- **A gap is documented, never faked.**
  Emergency checkpoint, background mining and audit are the capabilities where a client may have no equivalent hook,
  and the contract requires its README to say what is missing, the fallback and the effect.
  The others are not allowed a gap.
- **The label is the client's, the wire value is MemCastle's.**
  `off` is translated to `disabled` by the client, and sending the label is refused instead of being read as `full`.
- **Integrations reach MemCastle only over MCP and HTTP,** enforced by the `integrations-http-only` hook,
  which is AGENTS.md invariant 8.

## Alternatives rejected

- **A generic cross-agent integration framework.**
  A shared runtime or base class would have to hide exactly the lifecycle differences that matter, and would be a second
  thing to maintain for two clients.
  Each integration keeps its own language and packaging, as `integrations/README.md` already says.
- **Moving the lifecycle into MemCastle,** for example a scheduler that mines daily or a daemon-side session entity,
  so that the clients need less.
  It would give a client with no timer parity it does not have, and it moves client decisions into the core.
  The daemon stays a memory runtime.
- **The contract next to the adapters** in `integrations/`.
  It is outside the documentation build and the Markdown lint, and nothing would tell a reader when it went stale.
- **Rust-only test helpers without fixture files.**
  Simpler, but the TypeScript adapters could not reuse the expectations, which was the point.
- **A TypeScript harness now,** with its own toolchain and CI job.
  There is no adapter yet to exercise it, and the toolchain decision belongs with the first adapter.
  The fixtures make the harness cheap to add then.

## Consequences

- A new integration starts from the matrix, replays four JSON files and writes only the client-side tests,
  instead of rediscovering the architecture.
- Adding a capability means a row in the page, an entry in the manifest and a test, and forgetting any of the three fails
  the build.
- The Rust side proves only the daemon half.
  Until each adapter ships its own suite, the client half of every row is a promise in the page and not a check.
- The fixtures describe the daemon as it is today, including its thin session model.
  Giving the daemon a real session identity later would be a new decision, and would change the first capability.
- `tests/common/mcp.rs` adds one more copy of the small MCP client helpers that several test files already carry.
  The older copies are left alone, and folding them in is a separate cleanup.
- Real orphan data cannot be created through HTTP or MCP, so applying a repair to real orphans is not part of the
  conformance suite and stays covered by the unit tests in `src/repair`.
