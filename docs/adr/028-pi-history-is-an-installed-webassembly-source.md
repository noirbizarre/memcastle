# ADR-028: Pi history is an installed WebAssembly source, and the core has no Pi-specific code

## Status

Accepted, builds on [ADR-023](023-unified-source-model-for-mining.md) (the source model),
[ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) (sources as WebAssembly components) and
[ADR-019](019-shared-integration-contract.md) (an integration is lifecycle glue over MCP and HTTP).
It supersedes the part of ADR-023 that shipped `pi-sessions` as a built-in adapter, and the part of ADR-026 that kept it
native (issue #161).

## Context

`pi-sessions` was the second built-in adapter: Rust compiled into MemCastle that knew Pi's session layout (`~/.pi/agent/sessions`),
its JSONL format and a configuration key, `mining.pi_sessions_dir`.
That was the right start, and ADR-026 says why it does not scale: a source for one agent's history is code the core
compiles and releases for every user, while a change in Pi's format is Pi's to make and should not need a MemCastle release.
It also sat beside the question of where Pi's mining belongs.
The live Pi integration is client glue over MCP and HTTP, and must stay so (invariant 8): it decides *when* to ask for
mining, and acquiring history is not its job.

## Decision

- **Pi history is the `pi` source, a WebAssembly component built from `sources/pi/`.**
  It implements the same contract as every source: it discovers session files, reads them, and normalizes them into
  transcript documents.
  It declares `incremental` and `retains_raw` and asks for read access to the folder it is mined from and to
  `~/.pi/agent/sessions`, plus the `HOME` variable to find that folder.
  It has no network, runs no program and writes nothing.
- **The built-in `pi-sessions` adapter is removed**, with `mining.pi_sessions_dir` and `MEMCASTLE_MINING_PI_SESSIONS_DIR`.
  `directory` is the only adapter compiled in, and `registry::BUILTIN_NAMES` holds only its name.
  Nothing in `src/` names Pi's format; `tests/wasm_pi.rs` holds the source to it, and its conformance cases run on every
  change through the `Source (pi)` CI job and `memcastle source test`.
- **MemCastle keeps everything that is not acquisition.**
  Normalization is the source's, and chunking, deduplication, provenance, the cursor, idempotency and the durable job are
  the pipeline's, exactly as for any source.
  A session's identity is its path under the sessions folder, its revision changes when its content does, and a grown
  session files only its new tail.
- **A session is dated by its own header.**
  The document's time is the session's start (RFC 3339, from its header), not the modification time of its file, which
  changes when a session is copied or restored.
  A header whose time is not RFC 3339 is left undated rather than failing the session, because the host fails a whole job
  on an unparsable time.
- **The live integration is unchanged.**
  `integrations/pi/` keeps talking to the daemon over MCP.
  What it will submit for background mining ([#26](https://github.com/noirbizarre/memcastle/issues/26), which is now only
  about *when*) is `memcastle_mine` with `source: "pi"`; it never reads a session file.

## Consequences

- **Breaking.**
  The provider name `pi-sessions` is gone, so a script or a stored job that names it is refused as an unknown source,
  and the error lists what is known and how to install one.
  The new source is a different provider, so MemCastle keeps no cursor for it and its first run reads every session again.
  What the old adapter filed stays where it is, attributed to `pi-sessions`, and nothing links it to what `pi` files:
  a palace that mined Pi history before should remove the `pi` wing, or accept that sessions may be filed twice.
  A `pi_sessions_dir` in a configuration file is ignored; pass `--locator` instead.
- **Pi history needs an install step** until the official sources are bundled with releases, which is the packaging work
  of [#159](https://github.com/noirbizarre/memcastle/issues/159).
  `docs/mining-sources.md` documents building, packaging and installing it from a checkout.
  [ADR-033](033-source-distribution.md) now bundles it with releases, so `memcastle source install pi` needs no checkout.
- **Installing is consented.**
  The user agrees to exactly the two readable folders and `HOME`,
  and a component altered on disk is never run (invariant 10).
- **Mining Pi stays a daemon job.**
  The daemon reads the sessions, so sessions that ended long ago are mined with no agent running, which is the reason
  ADR-023 rejected reading them from the integration.
- **One more reference source**, which the conformance runner holds to the same cases as `directory` through its own
  fixtures in `sources/pi/fixtures`.
