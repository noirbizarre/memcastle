# ADR-022: Integrations are self-contained bun packages, tested against a real daemon

## Status

Accepted, settles the toolchain question that [ADR-019](019-shared-integration-contract.md) left to the first adapter.
Amended by [ADR-027](027-cross-integration-tests-live-in-a-common-package.md):
tests that need two integrations live in a test-only `integrations/common` package.
Production code is still per package

## Context

ADR-019 gave the integrations one contract and language-neutral fixtures, and deferred the client-side toolchain
until an adapter existed to exercise it.
Two adapters now exist: Pi loads TypeScript extensions, and OpenCode loads TypeScript plugins that it installs with bun.

The client half of every contract row can only be tested where the lifecycle is, so each adapter needs its own test suite.
That suite is only worth having if it speaks to a real daemon: the properties at stake are protocol ones,
such as the mode being selected on every new MCP session, a registry file being found, and an error body being read.
A fake daemon would test the fake.

Two pressures conflict.
Both adapters need the same small client (discovery, an MCP session, mode translation, failure classes),
which suggests sharing it.
But there is no shared runtime between ecosystems by design, and the two clients are not the same:
OpenCode needs one connection per OpenCode session, Pi has its own persistent-connection and settings conventions.

## Decision

- **Each integration is a self-contained bun package** under `integrations/<name>/`,
  with its own `package.json`, `bun.lock` and `tsconfig.json`.
  There is no workspace root and nothing shared between packages.
- **Each package carries its own copy of the small client** (`daemon`, `session`, `modes`, `failures`).
  It is about two hundred lines, and what differs between ecosystems is exactly what a shared copy would have to abstract.
- **The MCP transport is the official SDK,** `@modelcontextprotocol/sdk`,
  over the daemon's streamable HTTP endpoint with one handshake per agent session.
  The packages do not speak the protocol by hand.
- **Tests run against a real daemon.** A package's suite starts `memcastle serve` on an OS-assigned port in a temporary
  palace, finds it through its real registry file, and replays `tests/fixtures/integration/` through its own client.
  A missing binary fails the suite and never skips it.
- **One task runs them all.** `mise run integrations:check` builds the daemon and, for each package,
  installs with a frozen lockfile, typechecks and tests.
  bun is declared on that task and not in `[tools]`, so a CI job that does not run it never installs it.
  The task is part of `mise run check`.
- **CI has its own job** for it, so the client halves of the contract are checked on every pull request,
  not only on a contributor's machine.
- **A package never takes a dependency that the `integrations-http-only` hook would reject.**
  The hook scans lockfiles too, so a transitive dependency on a database driver fails the build.

## Alternatives rejected

- **A shared workspace package** holding the client.
  Less code, but it makes a runtime and a release unit that Pi and OpenCode both depend on,
  and the first time the two need different behaviour it grows options.
  ADR-019 already rejected a shared framework for the same reason.
- **A Rust-side harness** driving the adapters as subprocesses.
  It cannot see inside a plugin: the lifecycle hooks are the thing to test, and they only exist inside the host.
- **A fake or recorded daemon.**
  It would pass while the real protocol changed, which is the failure the suite exists to catch.
- **Node and npm instead of bun.**
  OpenCode installs plugin dependencies with bun and runs plugins on it, and Pi's own packaging supports it,
  so bun is the one runtime both hosts already use.
- **Package-local scripts only,** with no task and no CI job.
  Nothing would run them, and the contract's client half would again be a promise.

## Consequences

- Each integration's README states its conformance status against the matrix, and its tests prove the rows it claims.
- Two copies of the small client can drift.
  The fixtures are what keeps them honest, because both suites replay the same files,
  and a behavioural difference between the copies has to be a deliberate one.
- CI runs a Rust build in the new job so that it has a daemon to test against.
  The job uses the same caches as the others and adds a few minutes.
- The CI workflow is generated from a template, so the job is a local divergence that should be proposed upstream.
- bun is a new development dependency, but only for contributors who run the integration task or a package's own tests.
- Dependency updates for the packages need their own Dependabot entries, which are added with the packages.
