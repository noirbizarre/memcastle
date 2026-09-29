# Architecture

## The core idea

> MemCastle is a long-running memory server, not a CLI process that happens to expose MCP.

There is one MemCastle daemon per palace. Multiple AI coding-agent instances —
several OpenCode sessions, Claude Code, Cursor, a future web dashboard — connect to that *same* daemon,
so they share exactly the same memory palace, mining queue, database state, and search index.

```text
OpenCode #1 ──────┐
OpenCode #2 ──────┤
Claude Code ──────┤── MCP/HTTP ──> MemCastle daemon ──> SurrealDB
Cursor ───────────┤
CLI ──────────────┘
```

This is a deliberate departure from the reference implementations
([MemPalace](https://github.com/MemPalace/mempalace), [mempalace-rs](https://github.com/jxoesneon/mempalace-rs)),
which run one-process-per-CLI-invocation: every `mine`/`search`/MCP call independently loads config,
opens its own SQLite connections, and reloads an embedding model from scratch.
`mempalace-rs` in particular pairs SQLite (metadata) with a separate `usearch` HNSW index,
kept in sync only by best-effort,
which is the root cause of an entire "watchdog / auto-repair / re-embed everything" subsystem in that codebase.
MemCastle avoids that failure category by construction: one store, one writer process, no second index file to desync.

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

**The CLI has no business logic MCP/HTTP can't reuse.**
Every subcommand except `serve`/`daemon`/`migrate` is a thin `client::DaemonClient` HTTP call —
`memcastle mine ./project` submits a job over HTTP
exactly the way an MCP tool call or a future web dashboard would, rather than mining anything itself.
`serve`/`daemon` is the one command with real work:
it *is* the composition root (`server::run`) that owns the store, the scheduler, and the HTTP/MCP listeners.
`migrate` is a second, narrow exception: it connects to storage directly through `crate::migrate`,
the same runner `serve` calls on every startup, because migration must work before a daemon exists
(see `docs/adr/004-versioned-database-migrations.md`).

`app::AppServices` is the one seam every interface (`api`, `mcp`, and `server::run` itself) calls through.
Nothing under `api`/`mcp`/`cli`/`client` reaches into `store` or `jobs` directly —
the `store-isolation` `prek` hook greps for that (and `single-writer` limits who may construct a store).

## Storage: one SurrealDB, embedded or remote

`store::SurrealStore` wraps a single `Surreal<Any>` connection (`surrealdb::engine::any`),
which dispatches on a connection string's scheme at runtime:
`surrealkv:<path>` for the embedded default, or a `ws://`/`wss://` URL for a remotely hosted instance.
The rest of the codebase never branches on which backend is active —
`config::StoreConfig` picks one, `Backend` carries it, `SurrealStore::connect` is the only place that cares.
SurrealKV (pure Rust) is the only embedded backend Phase 1 compiles —
see `docs/adr/001-surrealkv-embedded-storage-engine.md`. Server-side storage is out of scope for Phase 1 entirely —
no RocksDB build variant is kept around speculatively for a server deployment model that doesn't exist yet;
that choice is deferred until it does.

Every read and write is hand-written SurrealQL (`db.query(...).bind(...)`)
rather than the SDK's typed `create`/`select` helpers:
datetimes cross the boundary as RFC3339 strings with explicit `<datetime>`/`<string>` casts,
and a record's own `id` is always projected out via `record::id(id)`.
That trades some verbosity for depending on only the smallest, most stable part of the driver's API —
its typed surface (`bind`/`take`'s `SurrealValue` requirement, the `Datetime`/`RecordId` wrapper types)
has already changed shape across SDK majors once,
and re-deriving it on every domain type would make the next such change a much bigger diff than a hand-written query is.

### Migrations

Two migration shapes, kept deliberately separate, both driven by one `crate::migrate::run`:

- **Schema.** Declarative `.surql` files under `database/schema/` (`database/schema/palace.surql`,
  `database/schema/migration_state.surql`), embedded into the binary via SurrealKit's
  `embed_schema!()` macro and applied through its `Sync` builder
  (`store::mod`'s `SurrealStore::sync_schema`). SurrealKit — not MemCastle — owns diffing,
  content-hash tracking (in its own `__entity`/`__rollout` metadata tables), and pruning; MemCastle does not
  implement a parallel schema-diff/versioning engine. SurrealKit's `Rollout` API is available for a
  future staged/expand-contract schema change, but nothing shipped yet has needed one.
- **Data.** An ordered, immutable list of versioned Rust steps (`crate::migrate::DataMigration`) for
  changes that can't be expressed as additive schema sync — a rename, reshape, split/merge, or
  backfill. Gated by a MemCastle-owned version watermark (`migration_state` table, defined as one
  of the schema files above but read/written exclusively by `store::migration_state` — never
  SurrealKit's own bookkeeping), so each step runs exactly once. Empty today: nothing shipped yet
  has needed one.

`crate::migrate::run(&store)` is the one runner both entry points call — normal daemon startup
(`server::run`) and the explicit `memcastle migrate` (`--check`, `--status`) command, which connects
to storage directly like `serve` does, bypassing `client::DaemonClient`. Sequence: sync schema (so
its own bookkeeping table exists) → acquire an exclusive lock → read the current data version → run
pending data migrations in order, recording the watermark after each success → re-sync schema (picks
up anything the release also shipped) → release the lock. A failed migration fails the daemon closed
rather than serving a partially migrated database; the watermark stays at the last step that
succeeded, so a later, corrected run resumes instead of replaying. `SurrealStore::connect` itself
does not sync schema or migrate — that's this explicit step, not an implicit side effect of opening
a connection. See `docs/adr/004-versioned-database-migrations.md` for the full design.

### Domain model

```text
Palace
└── Wing        (a project, or a source)
    └── Room     (a topical sub-bucket)
        └── Drawer  (one verbatim chunk — the atomic stored unit)
```

A `Drawer`'s `content` is immutable once written;
provenance (`source`, `provenance.requested_by`, `provenance.job_id`), tags, an optional `embedding`,
and a `valid_from`/`valid_to` pair travel alongside it.
`domain::entity` (`Entity`, `Relationship`) defines a bi-temporal knowledge-graph shape,
and `store::entities` is schema-**wired**: create/supersede/invalidate/list-relationships operations exist
and are exercised today by `checkpoint::run`'s optional `fact` mutation.
`relates_to` (`database/schema/palace.surql`) is a genuine SurrealDB-native graph edge table —
`TYPE RELATION IN entity OUT entity`, mutated and traversed with `RELATE`/graph-traversal SurrealQL —
unlike every other domain relationship in this codebase (wing→palace, room→wing, drawer→room),
which is a plain foreign-key column on a regular table. The only missing piece is a *populator*:
no mining code extracts entities/relationships from mined content yet —
that is issue #40's deliberate future work, not an oversight.

### Search

Lexical (BM25 full-text) search over `drawer.content` is the "basic working search path" this bootstrap establishes
(`search::lexical_search`), and it can be scoped to one wing and/or room by name —
the scope is expressed as SurrealQL predicates (nested subqueries resolving the name to room ids),
so SurrealDB applies the filter as part of query execution
rather than MemCastle fetching candidates and filtering them in Rust.
Semantic/vector search, temporal filtering, graph-aware retrieval, and hybrid ranking
are later phases layered on the same table — see [Non-goals](#non-goals-for-this-bootstrap).

## Memory mode: per-session, never daemon-global

`domain::MemoryMode` (`Full`/`ReadOnly`/`Disabled`) is how memory gets explicitly disabled for one client
without stopping the daemon or affecting any other client sharing it (PLAN.md principles 8-9).
It is *never* a process-wide setting — there is no `MEMCASTLE_ENABLED=false` daemon flag —
because the daemon already serves many agents at once;
disabling memory for one of them must not touch the others' in-flight jobs or reads.

| mode        | read (`search`/`recall`/`wake_up`/`diary_read`, job `list`/`show`) | write (`checkpoint`/`emergency_checkpoint`/`diary_write`, `mine`, applied `repair`) |
|-------------|--------------------------------------------------|-------------------------------------------------------------|
| `Full`      | ok                                               | ok                                                            |
| `ReadOnly`  | ok                                               | rejected (`Error::ModeForbidden`)                             |
| `Disabled`  | rejected (`Error::ModeForbidden`)                | rejected (`Error::ModeForbidden`)                             |

`Disabled` is symmetric on purpose: reads are rejected with the same typed error as writes, never a silent `Ok(empty)` —
an empty result would be indistinguishable from "genuinely found nothing,"
which would leak an ambiguous signal into a session that is supposed to behave as if MemCastle doesn't exist.

The gate follows what an operation reads or writes, not its method name.
A job record carries its whole input — for a checkpoint job, the memory being written —
so listing or showing jobs is a memory read, and a job whose purpose is to file or delete drawers
(`submit_mine`, and `submit_repair` when it is not a dry run) is a memory write.
Otherwise a disabled session could read palace content through the job list,
and a read-only one could mutate the palace by submitting a mine.
`ReadOnly` keeps job reads, since it can already read the same content through search.

What stays ungated is genuinely administrative:
`status` (counts and version, no content), job control (`pause_job`, `resume_job`, `cancel_job`, `retry_job` —
they need a job id, which a session that cannot list jobs never learns),
`submit_demo` (touches no palace content), `submit_audit` and a dry-run `submit_repair` (they only report).

Enforcement is centralized in `app::AppServices` (`require_read`/`require_write`, checked before any store contact) —
`api` and `mcp` only *extract* a `MemoryMode` and pass it down, never independently deciding what's allowed:

- **HTTP** reads an `X-MemCastle-Mode` header once per request
  via a small `axum::extract::FromRequestParts` extractor (`api::ModeHeader`),
  defaulting to `Full` when the header is absent so existing clients are unaffected.
  An unparsable value is a 400, never silently downgraded to `Full`.
- **MCP** has no per-request header in the tool-call model, so mode is negotiated once per session:
  a `memcastle_set_mode` tool call is cached in an `Arc<DashMap<session id, MemoryMode>>` inside `McpTools`,
  keyed by the `mcp-session-id` header rmcp's streamable-HTTP transport already assigns.
  Every other tool looks up this map before delegating to `AppServices`,
  defaulting to `Full` for a session that never called `memcastle_set_mode`.

See `docs/adr/002-memory-mode-session-scoping.md` for the full rationale and rejected alternatives.

## Memory primitives: recall, wake_up, diary

- `AppServices::recall` is `search` under a recall-oriented name (task brief §14's vocabulary) —
  the same scoped `lexical_search` underneath, never paraphrasing or truncating a `Drawer.content`.
  It exists as a name to hang a future recall-specific reranking off, not a reason to duplicate logic today;
  MemCastle itself does not enforce a search-before-answer protocol — that discipline is an integration/skill's job.
- `AppServices::wake_up` builds a deterministic session-start context:
  the agent's most recent diary entry (when a `wing` is given)
  plus up to `WakeUpBudget::max_items` most recent checkpoint-originated drawers,
  trimmed to `max_bytes` by whole drawers only — never mid-content, preserving the same verbatim guarantee as `recall`.
  Deliberately simple for V1 (task brief §13: "do not prematurely implement an elaborate token optimizer") —
  this is only L0/L1 of the task brief's layered retrieval model; project-specific and deeper retrieval are future work.
- `AppServices::diary_write`/`diary_read` persist/read an agent's journal entries
  as drawers filed under a fixed per-wing `"diary"` room,
  keyed by a caller-supplied `agent_identity` string that MemCastle stores and retrieves faithfully but never validates.
  See `docs/adr/003-checkpoint-as-a-durable-job.md` for why this is a direct, synchronous call rather than a job.

All four are gated by `MemoryMode` exactly like `search` — see the mode table above.

## Jobs: a durable queue, not an in-memory one

> The queue state is durable; the in-memory scheduler is only the execution mechanism.

`domain::Job` is a plain record (`id`, `kind`, `status`, `priority`, timestamps, `progress`, `attempt`/`recovery_attempts`/`max_attempts`,
`checkpoint`, `error`, lease fields, and any pending `pause_requested`/`cancel_requested`) persisted in SurrealDB.
Its status only ever changes through `Job::apply(event)`, an explicit transition table
(any `(status, event)` pair not listed here is rejected with a `TransitionError`):

```text
queued   -> running    (claimed)
running  -> paused     (cooperative)
paused   -> queued     (resumed)
running  -> completed
running  -> failed
queued | paused | running -> cancelled
running  -> queued     (crash recovery, crash-recovery budget permitting)
failed   -> queued     (retry; clears the error, keeps the checkpoint)
```

**Priority.** `Job.priority` is `domain::Priority`, a five-level enum (`Background < Low < Normal < High < Critical`) —
coarse buckets rather than an arbitrary integer, so callers can't invent incomparable numeric scales.
It serializes to/from the store's existing `job.priority` (`TYPE int`) column via fixed values
(`Critical`=100, `High`=75, `Normal`=50, `Low`=25, `Background`=0, with deliberate headroom between levels),
so introducing the enum took no migration.
A value read back that doesn't match one of the five is a surfaced `InvalidPriority` error,
never silently coerced to a default.
`SurrealStore::claim_next_job` claims the oldest, highest-priority `Queued` job first,
backed by the composite index `job_status_idx ON job FIELDS status, priority, created_at`.
Default priorities per submission path: `Mine` → `Background` (so mining never delays anything else), `Demo` → `Normal`,
`Audit`/`Repair` → `Normal`, `Checkpoint` → `High`, `Checkpoint` (emergency) → `Critical`.

`jobs::Scheduler` is a single sequential dispatcher loop (`store.claim_next_job`, a claim-and-transition)
that spawns bounded worker tasks (a `tokio::sync::Semaphore`) to execute claimed jobs.
The claim is a `SELECT` followed by an `UPSERT`, not a database-level atomic operation:
because exactly one scheduler owns the queue per daemon — the same "one daemon per palace" invariant as storage —
the sequential claim loop needs no distributed lock to be safe.

**Pause and cancel are cooperative, never a process kill.**
A handler (`jobs::demo`, `mining::run`) is written as a loop over discrete units of work (steps, files)
that checks `JobContext::should_pause`/`is_cancelled` between units,
persists a `checkpoint` before stopping, and returns — the scheduler transitions its status afterward.
Resuming a paused job re-reads that checkpoint and continues from there, not from zero.
`Audit` and `Repair` never check for pause, so a pause request only *requests* one and they run to completion;
an applied `Repair` does check for cancel before each delete.

**Resuming is replay-safe.**
A handler writes an item's records first and saves the checkpoint after, so a crash between the two
makes the resumed attempt redo that item.
Mining and checkpoint therefore derive each drawer's id (and each new fact edge's id) from the job id and item index
and skip a record that already exists, so the replay lands on the same record instead of storing a second copy.

**Crash recovery** (`Scheduler::recover`, run once at daemon startup):
any job left `Running` by an unclean shutdown is re-queued if its crash-recovery budget allows,
or marked `Failed` (with the reason recorded) otherwise — never silently forgotten.
The budget is `Job::max_attempts` (3), and what it counts is `Job::recovery_attempts`: crashes survived, and nothing else.
`Job::attempt` is a separate, informational count of every claim, including the one after a user's resume
or a shutdown re-queue, so a job that was merely paused twice is not one crash from failing.
`Queued` jobs need no recovery, and `Paused` jobs are deliberately left paused until someone resumes them.
A pause or cancel request does not depend on the in-memory `JobControl` surviving:
`request_pause`/`request_cancel` write `pause_requested`/`cancel_requested` on the `Running` job record
before the API answers, and `Job::apply` clears them when the job leaves `Running`.
`recover` honours them, so a job the user cancelled before a crash comes back `Cancelled` (cancel beats pause),
and one they paused comes back `Paused` — neither runs again, and neither spends attempt budget.
A user's pause also wins over a shutdown interrupt that is already under way.
A recovered job with no pending request runs again from its checkpoint.

**`checkpoint` vs `result`.** `Job.checkpoint` is handler-defined *resume* state
(`mining`/`checkpoint`'s per-item `{"next_index": n}`) —
it exists so a paused or crash-recovered job knows where to continue from,
and every job kind has one (defaulted to `{}`). `Job.result`, added for `JobKind::Audit`, is a separate field:
the final, once-set output of a job whose whole point is to produce a report rather than a resume position.
Most job kinds never set it; `Audit` and `Repair` do, on completion.
Keeping these as two fields — rather than overloading `checkpoint` for both purposes —
means "where do I read a job's progress from" and "where do I read what it found" never share one ambiguous field.

**`JobKind::Checkpoint`** (`src/checkpoint`) persists an already-classified batch of checkpoint items as durable drawers,
classified into destination buckets client-side, in the calling integration, not by MemCastle.
It structurally mirrors `mining::run`:
per-item cooperative pause/cancel, checkpointing `{"next_index": n}` after each item.
Every item gets a drawer written first, always, and *additionally* applies its `fact` mutation
(a knowledge-graph relationship, via `store::entities`) when one is present —
a checkpoint item is never only a graph mutation with no drawer to audit it.
Unlike `jobs::demo`, there is deliberately no artificial per-item delay:
an emergency checkpoint's entire purpose is saving state before a crash,
so synthetic latency would work against the feature.
See `docs/adr/003-checkpoint-as-a-durable-job.md` for why this runs as a job at all while diary writes (above) don't.

**`JobKind::Audit`** (`src/audit`) is a read-only palace consistency report,
scoped to what's structurally possible given the single-SurrealDB design described above —
not a port of `pi-palace`'s `/palace-audit` feature list,
most of which addresses a split-store desync failure mode that doesn't exist here.
It checks for orphan drawers (a `room` reference that no longer resolves), dangling `provenance.job_id` references,
`Failed` jobs that have exhausted their attempt budget,
a plain `Running`-job count (informational — there is no lease TTL yet to call any of them "stale"),
and drawers with no `embedding` (informational — semantic search doesn't exist yet, so this is never a defect).
Unlike `mining`/`checkpoint`, it does not chunk its work with a per-unit checkpoint:
a full scan is cheap and idempotent, so there is no meaningful partial state to resume from.

**`JobKind::Repair`** (`src/repair`) turns a subset of `Audit`'s findings into an actual fix:
dry-run-first (`dry_run = true` is the default at every CLI/API entry point,
and only ever records planned actions in the report without mutating anything),
and deliberately narrower than the issue that requested it — only orphan-drawer removal shipped.
A second candidate action, failing jobs stuck beyond an attempt-budget heuristic,
turned out to be redundant with what `Scheduler::recover` already does at every daemon startup
and was dropped rather than implemented for symmetry's sake
(see `src/repair/mod.rs`'s module doc for the full reasoning).
`based_on_job` narrows a repair run to what a specific prior audit job found, but never replaces a fresh live scan —
a destructive operation must never act on a report that might have gone stale since it was generated.

## The daemon lifecycle

`memcastle serve`/`daemon` runs in the **foreground**
(matching the `mempalace-server.service` systemd-unit pattern from the reference implementations) —
backgrounding is a supervisor's job (systemd, Docker, your shell), not this binary's.
On startup it: loads config, connects and migrates storage, recovers interrupted jobs,
starts the scheduler, binds the HTTP listener (serving both the REST API and MCP), and writes a small registry file.
On SIGINT/SIGTERM or `POST /api/shutdown`, it stops accepting new jobs and asks every running job to stop
at its next unit-of-work boundary.
Each one checkpoints and goes straight back to `Queued` (a job the user had paused stays `Paused`),
so the next daemon resumes it without anyone pressing resume.
The wait is bounded (10 seconds): a job that does not stop in time — one that never checks for pause,
like `Audit`/`Repair` — is left `Running` and re-queued by `Scheduler::recover` on the next start.
The daemon then removes its registry file and exits.

The registry file (`~/.memcastle/run/<hash of the canonical palace path>/daemon.json`)
is **operational metadata, never the source of truth** for "is a daemon running" —
that question is always answered by a live HTTP request.
The file's PID is checked with a liveness probe before it's trusted at all;
a stale file from a crashed daemon is simply overwritten by the next one that starts.

## MCP: another interface on the daemon, not a special process

`mcp::McpTools` implements `rmcp::ServerHandler` and is mounted at `/mcp` on the *same* axum router as the REST API,
via `rmcp`'s streamable-HTTP server transport. This is deliberately HTTP-only for the bootstrap:
it is natively multi-client (the actual goal — N agents sharing one daemon),
and most MCP clients already support a URL-based transport directly, so no bridging process is required to get there.
A stdio bridge for clients that only support spawning a local subprocess is real,
but explicitly deferred, future work (see below) —
tool logic itself never touches a transport type, so adding one is additive when it's needed.

## Non-goals for this bootstrap

Deliberately out of scope, and each is structurally possible without rework given the module boundaries above:

- Semantic/vector search, embeddings, hybrid ranking.
- Real entity/relationship extraction wired into mining (the schema exists; nothing populates it).
- A stdio MCP bridge/proxy for clients that can't speak HTTP.
- The CLI auto-starting a daemon on demand.
- Full `wings`/`rooms`/`drawers`/`maintenance` CRUD (currently stubs).
- Remote SurrealDB authentication beyond root sign-in.
- Robust cross-platform process supervision for `memcastle restart`
  (it's a best-effort respawn; use a real supervisor in production).
- A web dashboard (the API is shaped so one can be built entirely as an API client, same as the CLI).
