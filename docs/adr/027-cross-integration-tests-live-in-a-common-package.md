# ADR-027: Tests that need two integrations live in a common test package, and production code stays per package

## Status

Accepted, amends [ADR-022](022-integrations-are-bun-packages-tested-against-a-real-daemon.md) (which said there is
nothing shared between packages) and applies [ADR-019](019-shared-integration-contract.md) (the client half of the
contract is proved by the clients, against the same fixtures).

## Context

The `session-mode` capability says a session in `off` must behave as if MemCastle were not installed,
and that sessions in different modes must not affect each other, including across integrations.
The daemon proves its half once, in Rust: three MCP sessions in three modes share one daemon and keep their own mode.
The other half is about the clients, and it is a statement about two of them at once:
a `full` Pi session's checkpoint lands while an `off` OpenCode session beside it does nothing.
Neither package can host that test without importing the other's sources, which ADR-022 says does not happen.

The isolation proof also needs the same three tools in both places:
a real daemon, a recorder of what each integration sent over HTTP, and a fake of the host.
Keeping a copy in each package, as ADR-022 does for the small client, would triple the harness
for tests that are the same by definition.

## Decision

- **`integrations/common/` is a private, test-only bun package.**
  It has its own `package.json`, `bun.lock` and `tsconfig.json`, no `src/`, and ships nothing.
- **It holds the tests that need more than one integration,** and the harness they share:
  the daemon (`memcastle serve` on an OS-assigned port, found through its real registry file),
  a recorder of every HTTP request attributed to the actor that caused it,
  and a fake of each host's slice (Pi's `on` and `registerCommand`, OpenCode's client).
- **It imports each integration's sources by relative path,** so it tests the code that ships and no copy of it.
  The host packages' types resolve through `paths` in its `tsconfig.json`, so their versions are pinned once, in the
  integration that owns them.
- **Production code is still per package.**
  The small client, the settings, the mode translation and the lifecycle code stay duplicated, as ADR-022 decided,
  and nothing under `integrations/pi/` or `integrations/opencode/` imports from `common`.
- **Isolation is proved on the wire, with a control.**
  `tests/fixtures/integration/off-isolation.json` lists every path by which an integration can put MemCastle material in
  front of the model, and which integrations have it.
  Each path is driven in `off` and in `full`: `off` must surface nothing and make no request,
  and `full` must surface something, so a driver that observes nothing cannot pass.
  A path added to the fixture fails the test until each integration that names it drives it.
- **`mise run integrations:check` runs `common` last,** after the integrations whose sources it imports are installed.
  The `integrations-http-only` hook covers it like any other package.

## Alternatives rejected

- **A shared workspace package for production code.**
  ADR-022 already rejected it: it makes a release unit both integrations depend on,
  and it grows options the first time they need to differ.
  This decision shares tests, which are never released.
- **A cross-package import from one integration's tests.**
  It would work, and it hides a dependency of Pi's tests on OpenCode's checkout (or the reverse)
  that nothing in either `package.json` says.
- **A raw MCP client standing in for the other integration.**
  It tests the contract but not the second integration,
  and the point of the test is that both real integrations stay inside their own mode.
- **Asserting on what a handler returned.**
  An `off` handler that fails to connect returns nothing too.
  Watching the requests is the only way to tell "chose not to ask" from "asked and failed".

## Consequences

- A third integration adds its drivers to `off-isolation.test.ts` and an actor to `mixed-modes.test.ts`,
  and replays the same fixture, so Claude Code starts with the same proof.
- `common` reaches into the integrations' file layout.
  Renaming a source file there is a change in two places, and the typecheck says so.
- A user-installed copy of a MemCastle skill, or an `mcp.memcastle` entry in the host's own configuration,
  is outside what any integration controls.
  An `off` integration registers nothing at all rather than guarding a hook it can never be asked to run,
  and the contract says so.
- Dependabot has one more package to watch, which carries only TypeScript and the bun types.
