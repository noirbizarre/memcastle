# Architecture

## The core idea

> MemCastle is a long-running memory server, not a CLI process that happens
> to expose MCP.

There is one MemCastle daemon per palace. Multiple AI coding-agent
instances — several OpenCode sessions, Claude Code, Cursor, a future web
dashboard — connect to that *same* daemon, so they share exactly the same
memory palace, mining queue, database state, and search index.

```text
OpenCode #1 ──────┐
OpenCode #2 ──────┤
Claude Code ──────┤── MCP/HTTP ──> MemCastle daemon ──> SurrealDB
Cursor ───────────┤
CLI ──────────────┘
```

This is a deliberate departure from the reference implementations
([MemPalace](https://github.com/MemPalace/mempalace),
[mempalace-rs](https://github.com/jxoesneon/mempalace-rs)), which run
one-process-per-CLI-invocation: every `mine`/`search`/MCP call independently
loads config, opens its own SQLite connections, and reloads an embedding
model from scratch. `mempalace-rs` in particular pairs SQLite (metadata) with
a separate `usearch` HNSW index, kept in sync only by best-effort, which is
the root cause of an entire "watchdog / auto-repair / re-embed everything"
subsystem in that codebase. MemCastle avoids that failure category by
construction: one store, one writer process, no second index file to
desync.

## Layers

```text
cli / mcp / api          <- interfaces (thin: parse, dispatch, serialize)
       |
      app                <- application services (the only layer the above may call)
       |
  domain + jobs + search  <- pure model + scheduling + retrieval logic
       |
     store                <- SurrealDB, embedded or remote
```

**The CLI has no business logic MCP/HTTP can't reuse.** Every subcommand
except `serve`/`daemon` is a thin `client::DaemonClient` HTTP call —
`memcastle mine ./project` submits a job over HTTP exactly the way an MCP
tool call or a future web dashboard would, rather than mining anything
itself. `serve`/`daemon` is the one command with real work: it *is* the
composition root (`server::run`) that owns the store, the scheduler, and
the HTTP/MCP listeners.

`app::AppServices` is the one seam every interface (`api`, `mcp`, and
`server::run` itself) calls through. Nothing under `api`/`mcp`/`cli` reaches
into `store` or `jobs` directly — a `prek` hook greps for that.

## Storage: one SurrealDB, embedded or remote

`store::SurrealStore` wraps a single `Surreal<Any>` connection
(`surrealdb::engine::any`), which dispatches on a connection string's scheme
at runtime: `surrealkv:<path>` for the embedded default, or a `ws://`/`wss://`
URL for a remotely hosted instance. The rest of the codebase never branches
on which backend is active — `config::StoreConfig` picks one, `Backend`
carries it, `SurrealStore::connect` is the only place that cares. SurrealKV
(pure Rust) is the only embedded backend Phase 1 compiles — see
`docs/adr/001-surrealkv-embedded-storage-engine.md`.

Every read and write is hand-written SurrealQL (`db.query(...).bind(...)`)
rather than the SDK's typed `create`/`select` helpers: datetimes cross the
boundary as RFC3339 strings with explicit `<datetime>`/`<string>` casts, and
a record's own `id` is always projected out via `record::id(id)`. That
trades some verbosity for depending on only the smallest, most stable part
of the driver's API — its typed surface (`bind`/`take`'s `SurrealValue`
requirement, the `Datetime`/`RecordId` wrapper types) has already changed
shape across SDK majors once, and re-deriving it on every domain type would
make the next such change a much bigger diff than a hand-written query is.

### Domain model

```text
Palace
└── Wing        (a project, or a source)
    └── Room     (a topical sub-bucket)
        └── Drawer  (one verbatim chunk — the atomic stored unit)
```

A `Drawer`'s `content` is immutable once written; provenance
(`source`, `provenance.requested_by`, `provenance.job_id`), tags, an
optional `embedding`, and a `valid_from`/`valid_to` pair travel alongside it.
`domain::entity` (`Entity`, `Relationship`) defines a bi-temporal
knowledge-graph shape — schema-ready (see the `entity`/`relates_to` tables in
`store/migrations/0001_init.surql`) but **not wired into mining yet**; that
is deliberate future work, not an oversight.

### Search

Lexical (BM25 full-text) search over `drawer.content` is the "basic working
search path" this bootstrap establishes (`search::lexical_search`), and it
can be scoped to one wing and/or room by name — the scope is expressed as
SurrealQL predicates (nested subqueries resolving the name to room ids), so
SurrealDB applies the filter as part of query execution rather than
MemCastle fetching candidates and filtering them in Rust. Semantic/vector
search, temporal filtering, graph-aware retrieval, and hybrid ranking are
later phases layered on the same table — see
[Non-goals](#non-goals-for-this-bootstrap).

## Jobs: a durable queue, not an in-memory one

> The queue state is durable; the in-memory scheduler is only the execution
> mechanism.

`domain::Job` is a plain record (`id`, `kind`, `status`, `priority`,
timestamps, `progress`, `attempt`/`max_attempts`, `checkpoint`, `error`,
lease fields) persisted in SurrealDB. Its status only ever changes through
`Job::apply(event)`, an explicit, exhaustively-matched transition table:

```text
queued   -> running    (claimed)
running  -> paused     (cooperative)
paused   -> queued     (resumed)
running  -> completed
running  -> failed
queued | paused | running -> cancelled
running  -> queued     (crash recovery, attempt budget permitting)
```

`jobs::Scheduler` is a single sequential dispatcher loop
(`store.claim_next_job`, an atomic claim-and-transition) that spawns bounded
worker tasks (a `tokio::sync::Semaphore`) to execute claimed jobs. Because
exactly one scheduler owns the queue per daemon — the same "one daemon per
palace" invariant as storage — the sequential claim loop needs no
distributed lock to be safe.

**Pause and cancel are cooperative, never a process kill.** A handler
(`jobs::demo`, `mining::run`) is written as a loop over discrete units of
work (steps, files) that checks `JobContext::should_pause`/`is_cancelled`
between units, persists a `checkpoint` before stopping, and returns — the
scheduler transitions its status afterward. Resuming a paused job re-reads
that checkpoint and continues from there, not from zero.

**Crash recovery** (`Scheduler::recover`, run once at daemon startup): any
job left `Running` by an unclean shutdown is re-queued if its attempt budget
allows, or marked `Failed` otherwise — never silently forgotten.

## The daemon lifecycle

`memcastle serve`/`daemon` runs in the **foreground** (matching the
`mempalace-server.service` systemd-unit pattern from the reference
implementations) — backgrounding is a supervisor's job (systemd, Docker,
your shell), not this binary's. On startup it: loads config, connects and
migrates storage, recovers interrupted jobs, starts the scheduler, binds the
HTTP listener (serving both the REST API and MCP), and writes a small
registry file. On SIGINT/SIGTERM or `POST /api/shutdown`, it stops accepting
new jobs, lets the scheduler's dispatch loop drain, and exits.

The registry file (`~/.memcastle/run/<hash of the canonical palace
path>/daemon.json`) is **operational metadata, never the source of truth**
for "is a daemon running" — that question is always answered by a live
HTTP request. The file's PID is checked with a liveness probe before it's
trusted at all; a stale file from a crashed daemon is simply overwritten by
the next one that starts.

## MCP: another interface on the daemon, not a special process

`mcp::McpTools` implements `rmcp::ServerHandler` and is mounted at `/mcp` on
the *same* axum router as the REST API, via `rmcp`'s streamable-HTTP server
transport. This is deliberately HTTP-only for the bootstrap: it is natively
multi-client (the actual goal — N agents sharing one daemon), and most MCP
clients already support a URL-based transport directly, so no bridging
process is required to get there. A stdio bridge for clients that only
support spawning a local subprocess is real, but explicitly deferred, future
work (see below) — tool logic itself never touches a transport type, so
adding one is additive when it's needed.

## Non-goals for this bootstrap

Deliberately out of scope, and each is structurally possible without
rework given the module boundaries above:

- Semantic/vector search, embeddings, hybrid ranking.
- Real entity/relationship extraction wired into mining (the schema exists;
  nothing populates it).
- A stdio MCP bridge/proxy for clients that can't speak HTTP.
- The CLI auto-starting a daemon on demand.
- Full `wings`/`rooms`/`drawers`/`maintenance` CRUD (currently stubs).
- Remote SurrealDB authentication beyond root sign-in.
- Robust cross-platform process supervision for `memcastle restart` (it's a
  best-effort respawn; use a real supervisor in production).
- A web dashboard (the API is shaped so one can be built entirely as an
  API client, same as the CLI).
