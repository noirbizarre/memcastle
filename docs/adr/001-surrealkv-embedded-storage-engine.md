# ADR-001: SurrealKV as the only embedded storage engine in Phase 1

## Status

Accepted

## Context

MemCastle is local-first during Phase 1: one daemon, one embedded SurrealDB instance, and no server-side storage
engine of its own (a remote SurrealDB can be connected to, but MemCastle does not build or bundle one).
`store::SurrealStore` wraps a single `Surreal<Any>` connection (`surrealdb::engine::any`),
which dispatches on a connection string's scheme at runtime —
the choice of embedded backend is already isolated to one function (`Backend::endpoint`)
and one Cargo feature flag, never a domain concept.

Until now, that feature flag was `kv-rocksdb`. RocksDB is mature and battle-tested, but it is a native C++ dependency
(`surrealdb-librocksdb-sys`, vendoring `rocksdb`/`snappy` and compiling them from source):
every fresh build environment pays a multi-minute native compile,
`cargo tree` pulls in a `cc`/`cmake` build-dependency chain,
cross-compiling release binaries for musl/ARM targets has to carry a C toolchain along for the ride,
and any C++-toolchain mismatch on the developer's machine (vendored vs. system library, missing symbols)
becomes a linker error rather than a Cargo error. None of that cost buys anything Phase 1 needs:
there is no bundled server-side engine to make RocksDB's production track record relevant,
and the project is a single-writer, single-palace, local-first tool.

The alternative already available in the same `surrealdb` crate's `any` engine is `kv-surrealkv`:
SurrealDB's own embedded engine, pure Rust (confirmed by inspecting its resolved dependency tree —
`arc-swap`, `crossbeam-skiplist`, `lz4_flex`, `snap`, etc., no `cc`/`cmake` build-dependency at all).

## Decision

Use SurrealKV (`kv-surrealkv`) as the only embedded storage backend for Phase 1. Concretely:

- `Cargo.toml` compiles `kv-surrealkv`, not `kv-rocksdb`.
  Phase 1 does not compile, package, test, or document RocksDB at all — no feature flag kept around "just in case."
- `store::Backend::endpoint()` builds a `surrealkv:<path>` connection string for the embedded case,
  exactly the same shape (`<scheme>:<path>`, single colon) as the code it replaces —
  no new abstraction, no new `config` knob for "which embedded engine":
  `config::StoreConfig` and `store::Backend` still only distinguish embedded vs. remote, same as before this change.
- The future server-side storage backend choice is explicitly **not** decided here.
  When a server deployment architecture exists, that's a separate design task,
  evaluating SurrealKV's maturity and performance at that point
  against whatever alternatives (RocksDB included) are relevant then —
  this ADR does not pre-commit to RocksDB, SurrealKV, or anything else for that case.

## Consequences

- The default build no longer needs a C/C++ toolchain or `cmake` at all;
  first-build time and cross-compilation for release targets both get simpler as a side effect.
- SurrealKV is younger and less battle-tested than RocksDB, particularly for large-scale or high-throughput workloads.
  That risk is accepted for Phase 1's local-first, single-user, single-writer scope;
  it is exactly what the server-mode follow-up task above needs to re-evaluate
  before Phase 1's deployment shape changes.
- SurrealKV does not release its on-disk lock file when a `Surreal` handle merely drops within the same process,
  the same constraint RocksDB had — reopening the same palace path twice in one process still isn't possible,
  though SurrealKV fails that attempt immediately with an explicit error instead of RocksDB's silent hang.
  `tests/persistence.rs`'s two-process restart test remains the correct way to prove "data survives a restart."
