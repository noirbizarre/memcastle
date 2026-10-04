# MCP tools and REST API

The daemon serves two interfaces on the same listener (`127.0.0.1:8420` by default, see
[Configuration](configuration.md#the-listener-address-and-port)):

- **MCP** at `/mcp`, over streamable HTTP, for agents.
  [Connect an MCP client](mcp-clients.md) shows how to point a client at it.
- **REST** under `/api`, which is what the CLI uses and what a script or dashboard can call.

Both are thin layers over the same application services, so they accept the same arguments,
enforce the same [memory modes](memory-modes.md) and fail with the same error body.

## MCP tools

Every tool returns pretty-printed JSON as text.
A failure comes back as an MCP error result carrying the [error body](#errors) below.

| Tool | Arguments | Does |
|---|---|---|
| `memcastle_set_mode` | `mode` | Choose this session's [memory mode](memory-modes.md). |
| `memcastle_status` | none | Daemon health: version, uptime, pid, address, palace, datastore and migration state, counts. |
| `memcastle_search` | `query`, `limit?`, `wing?`, `room?`, `ranking?`, `tags?`, `source_kind?`, `as_of?`, `include_historical?`, `expand?` | Search drawer content: lexical, semantic or hybrid, see [Searching](#searching). |
| `memcastle_recall` | the same, without `room` | Verbatim recall of matching content. |
| `memcastle_wake_up` | `agent_identity`, `wing?`, `max_items?`, `max_bytes?` | Session-start context for an agent. |
| `memcastle_diary_write` | `agent_identity`, `wing`, `content` | Write a diary entry. |
| `memcastle_diary_read` | `agent_identity`, `wing`, `limit?` | Read an agent's newest diary entries. |
| `memcastle_checkpoint` | `payload`, `emergency?` | Submit a durable checkpoint job. |
| `memcastle_mine` | `path` or `source`, `locator?`, `full?`, `wing?` | Submit a job that mines a directory, or a [source](mining-sources.md) such as `pi-sessions`. |
| `memcastle_audit` | `scope?` | Submit a read-only consistency audit job. |
| `memcastle_repair` | `dry_run?`, `based_on_job?` | Submit a repair job; a dry run unless `dry_run` is `false`. |
| `memcastle_job_list` | `status?` | List jobs, newest first. |
| `memcastle_job_get` | `id` | Show one job. |
| `memcastle_job_pause` | `id` | Ask a running job to pause at its next checkpoint. |
| `memcastle_job_resume` | `id` | Resume a paused job. |
| `memcastle_job_cancel` | `id` | Cancel a queued, paused or running job. |
| `memcastle_job_retry` | `id` | Retry a failed job. |

Defaults are the same as the CLI's: `limit` is 10 for `memcastle_search` and `memcastle_recall`, 20 for
`memcastle_diary_read`, and every read limit is capped at 200.
`memcastle_wake_up` defaults to 10 items and 8192 bytes.
`memcastle_mine` (and a `mine` job over `POST /api/jobs`) needs an absolute `path`,
because the daemon does not share your shell's working directory.
Give exactly one of `path` and `source`: a source is mined incrementally from where its last run stopped,
and `full` reads it again from the beginning.
Mining, checkpoint, audit and repair return the submitted job immediately;
poll it with `memcastle_job_get` to see its progress and result.

The instructions an MCP client receives at `initialize` are generated from this list of tools,
and a test compares the two, so what a client is told cannot drift from what the daemon offers.

### Checkpoint payload

`memcastle_checkpoint` and `memcastle checkpoint` take an already-classified batch of memories.
MemCastle does not decide what is worth remembering; the calling integration does, and this is the result:

```json
{
  "items": [
    {
      "destination": "preference",
      "wing": null,
      "content": "Always run the formatter before committing.",
      "name": null,
      "tags": ["workflow"],
      "source": { "kind": "manual", "uri": null, "agent": "opencode" },
      "fact": null
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `destination` | One of `preference`, `project`, `diary` or `general`. |
| `wing` | Optional wing override. Without it, the item is filed under `preferences`, `projects`, `diary` or `general`. |
| `content` | The text to store, verbatim. |
| `tags` | Free-form labels stored with the drawer. Required, may be empty. |
| `source` | Where the memory came from: `kind` is `file` or `manual` (the MCP schema allows only these; REST and the CLI store any other value as `other`), `uri` and `agent` are optional. |
| `name` | Optional name for the drawer, unique within its room, so it can be addressed as `wing/room/name`. A name held by another drawer fails the item with `memcastle::palace::drawer_name_taken`; an unusable one is refused at submission. |
| `fact` | Optional knowledge-graph change made alongside the drawer: `{"op": "add" \| "supersede" \| "invalidate", ...}`; a `confidence` outside 0 to 1 is refused at submission. |

Over MCP, `payload` is a JSON object, and the tool's input schema describes its shape.
A JSON-encoded string of that object is accepted too, because some clients serialise it before sending.
A string that is not valid JSON is refused as `memcastle::input::invalid`, blamed on `payload`.

Every item produces a drawer.
A payload with no items, or an item whose `content` is blank, is refused at submission as `memcastle::input::invalid`,
blamed on `payload`, so a job never completes having stored nothing.
A job that is interrupted resumes where it stopped and never stores an item twice.

## REST API

Requests and responses are JSON.
The routes that read or write memory (including `GET /api/jobs*`) accept the optional `X-MemCastle-Mode` header,
and `/api/status` accepts it only to report the mode
(`full`, `read_only` or `disabled`; `full` when absent), and an unrecognized value is a `400`, never silently treated
as `full`.
`/api/health`, job control (pause, resume, cancel, retry) and `/api/shutdown` are never gated and ignore it.

A mode the daemon refuses is a `403` with the code `memcastle::app::mode_forbidden`.
A request without a valid token, on a daemon with [authentication](authentication.md) enabled,
is a `401` with the code `memcastle::auth::unauthorized` and a `WWW-Authenticate: Bearer` header.
Other statuses are `400` for invalid input, a transition the job's state does not allow, or a database admin endpoint
that would be unsafe (`memcastle::db::unsafe_bind`), `404` for an unknown job, wing, room or drawer,
`409` for a job recorded as running that has no worker (restart the daemon), a job that kept changing state
under the request (run it again), a database admin endpoint that is already
open or cannot bind its address (`memcastle::db::bind_failed`), a drawer name already held by other content,
a wing or room delete while a job that writes to the palace is pending, or superseding a drawer that was already
superseded (`memcastle::palace::drawer_superseded`), `502` when the embedding provider fails
(`memcastle::embed::failed`), and `500` for a server failure.
A search that needs a vector it cannot get (`memcastle::search::semantic_unavailable`) and a request that needs an embedding
provider when none is configured (`memcastle::embed::not_configured`) are `400`s.

| Route | Purpose | Parameters |
|---|---|---|
| `GET /api/health` | Liveness: `{"status": "ok"}`. Touches nothing else. | none |
| `GET /api/status` | The full status report, also used by `memcastle status`. | none |
| `GET /api/search` | Search drawers. | query string: `q` (or `query`), `limit`, `wing`, `room`, `ranking`, `tags`, `source_kind`, `as_of`, `include_historical`, `expand` |
| `POST /api/search` | The same search as a JSON [`SearchQuery`](#searching), the only way to send a `query_embedding`. | JSON body |
| `GET /api/recall` | Recall drawers. | as `GET /api/search`, and `room` is ignored |
| `POST /api/recall` | The JSON form of recall. | JSON body |
| `GET /api/wake-up` | Session-start context. | `agent_identity`, `wing`, `max_items`, `max_bytes` |
| `GET /api/diary` | Read diary entries. | `agent_identity`, `wing`, `limit` |
| `POST /api/diary` | Write a diary entry. | JSON body: `agent_identity`, `wing`, `content`, `requested_by?` |
| `GET /api/jobs` | List jobs. | `status` |
| `POST /api/jobs` | Submit a job. | JSON body, see [below](#submitting-jobs) |
| `GET /api/jobs/{id}` | Show one job. | none |
| `POST /api/jobs/{id}/pause` | Request a pause. | none |
| `POST /api/jobs/{id}/resume` | Resume a paused job. | none |
| `POST /api/jobs/{id}/cancel` | Cancel a job. | none |
| `POST /api/jobs/{id}/retry` | Retry a failed job. | none |
| `POST /api/shutdown` | Shut the daemon down gracefully. | none |
| `GET /api/wings` | List wings with their room and drawer counts. | none |
| `POST /api/wings` | Create a wing, or find it: `201` when created, `200` when it existed. | JSON body: `name`, `description?` |
| `GET /api/wings/{wing}` | One wing with its totals and rooms. | none |
| `DELETE /api/wings/{wing}` | Delete the wing, its rooms and their drawers: `{wings, rooms, drawers}` removed. | none |
| `GET /api/wings/{wing}/rooms` | List a wing's rooms with their drawer counts. | none |
| `POST /api/wings/{wing}/rooms` | Create a room (and its wing), or find it. | JSON body: `name`, `description?` |
| `GET /api/wings/{wing}/rooms/{room}` | One room. | none |
| `DELETE /api/wings/{wing}/rooms/{room}` | Delete the room and its drawers. | none |
| `GET /api/wings/{wing}/rooms/{room}/drawers` | List the newest drawers, with a preview of each. | query string: `limit` (default 50, at most 200) |
| `POST /api/wings/{wing}/rooms/{room}/drawers` | Write a drawer, creating its wing and room if needed. | JSON body: `content`, `name?`, `requested_by?` |
| `GET /api/wings/{wing}/rooms/{room}/drawers/{drawer}` | One drawer in full. | none |
| `DELETE /api/wings/{wing}/rooms/{room}/drawers/{drawer}` | Delete one drawer. | none |
| `POST /api/drawers/{id}/supersede` | End a drawer's validity now, with a replacement when `content` is given: `{superseded, replacement}`. | JSON body: `content?`, `tags?`, `requested_by?` |
| `PUT /api/drawers/{id}/embedding` | Attach a vector you computed to a drawer. | JSON body: `embedding` (768 numbers) |
| `POST /api/drawers/{id}/mentions` | Record that a drawer mentions an entity, for graph expansion: `201` when linked, `200` when it already was. | JSON body: `name`, `kind` |
| `POST /api/auth/token` | Generate a token, replacing any previous one: `{token, algorithm, version, created_at}`. | none |
| `DELETE /api/auth/token` | Revoke the generated token: `{"revoked": true}`. | none |
| `GET /api/db` | Whether the [database admin endpoint](database-access.md) is listening, and where. | none |
| `POST /api/db` | Open the database admin endpoint, or report it when it is already open. | JSON body, all optional: `bind`, `port`, `allow_remote`, `allowed_origins` |
| `DELETE /api/db` | Close the database admin endpoint. Succeeds when it was not open. | none |

`requested_by` records which channel a write came through; it defaults to `http` (the CLI sends `cli`, MCP uses `mcp`).
The status report keeps `/api/health` cheap: an unhealthy datastore is reported inside `/api/status` with a `200`,
so a supervisor restarting on `/api/health` does not restart a daemon that would recover on its own.

```sh
curl -s http://127.0.0.1:8420/api/health
curl -s 'http://127.0.0.1:8420/api/search?q=formatter&limit=5'
curl -s http://127.0.0.1:8420/api/jobs?status=running
```

### Searching

`memcastle_search`, `memcastle_recall`, `GET /api/search` and `GET /api/recall` take the same options.
Only the query is required, so a plain `?q=word` means what it always did.

| Option | Meaning |
|---|---|
| `ranking` | `auto` (the default), `lexical`, `semantic` or `hybrid`. |
| `wing`, `room` | Restrict to a wing or room by name. `recall` ignores `room`. |
| `tags` | Drawers carrying every one of these tags: a list over MCP, comma-separated in a query string (`tags=a,b`). |
| `source_kind` | `file`, `manual`, `transcript` or `other`. |
| `as_of` | An RFC 3339 instant, such as `2026-01-31T12:00:00Z`: search the memory that was valid then. |
| `include_historical` | Also return memory that has been superseded. Cannot be combined with `as_of`. |
| `expand` | Append drawers related to the hits through the knowledge graph. |
| `limit` | At most this many hits, 10 by default and 200 at most. |

**Ranking.**
`lexical` is BM25 over the words, stemmed and without synonyms, matching every word first and any of them only when
nothing matched.
`semantic` ranks by vector similarity, and `hybrid` fuses the two by reciprocal rank.
`auto` is hybrid when the query can be embedded and lexical otherwise: a palace with no
[embedding provider](configuration.md#embeddings) keeps searching exactly as before, and a provider that is down
degrades `auto` to lexical with a warning in the daemon log.
An explicit `semantic` or `hybrid` that cannot get a vector fails with `memcastle::search::semantic_unavailable`
instead of answering lexically.

**Time.**
By default a search sees only what is valid now.
A drawer is valid at an instant from its `valid_from` until, but not including, its `valid_to`, so a drawer superseded at
that instant and its replacement are never both found.
`as_of` and `include_historical` reach older memory, which is how a corrected belief is still found as it stood.

**Expansion.**
With `expand`, drawers that share an entity with a hit (or sit one currently valid `relates_to` hop away) follow the direct
hits, best first, up to `limit` more.
Each carries a `graph` signal and `via`, the entities that connect it.
Expansion never reorders or replaces a direct hit.
Link drawers to entities with `POST /api/drawers/{id}/mentions` or `memcastle drawer mention`.

**Results.**
Every hit is the stored drawer, verbatim, plus these fields:

```json
{
  "id": "…", "content": "…", "tags": [], "valid_from": "…", "valid_to": null,
  "score": 0.0328,
  "signals": { "lexical": 1.2, "semantic": 0.91 },
  "via": ["alice"]
}
```

`score` means what the ranking says (BM25, cosine similarity or reciprocal-rank fusion) and is comparable only within one
response.
`signals` shows which legs matched and how strongly, and is absent when none did; `via` appears only on expanded hits.
Equal scores order by drawer id, so a query ranks identically every time.
A hit never carries the embedding vector.

**Your own vectors.**
`POST /api/search` takes the request as JSON, so it can carry a `query_embedding`:

```json
{ "text": "…", "ranking": "semantic", "limit": 5,
  "filter": { "wing": "work", "tags": ["style"], "temporal": "current" },
  "query_embedding": [0.01, "… 768 numbers"] }
```

`temporal` is `"current"`, `"all"` or `{"as_of": "2026-01-31T12:00:00Z"}`.
Attach a document vector with `PUT /api/drawers/{id}/embedding`, whose body is `{"embedding": [...]}`.
Both need exactly 768 numbers: the dimension is part of the palace's schema.

**Correcting a drawer.**
`POST /api/drawers/{id}/supersede` closes the drawer and, when the body has `content`, files a replacement in the same room
from the same instant.
The replacement takes over the drawer's name (the old one stays reachable by id) and its tags unless `tags` says otherwise.
Without `content` the drawer is only invalidated.
The old content is never rewritten.
These three routes take a drawer id, which every search hit carries, and have no MCP tool.

### Wings, rooms and drawers

`{wing}` and `{room}` are a name or a UUID, and `{drawer}` is a name or a UUID within its room.
A drawer name may contain `/`, so `{drawer}` takes the rest of the path.
Creating something that exists is not an error: the answer is `200` instead of `201`, and its body says `"created": false`.
The answers of `wing` and `room` carry their counts, computed when asked, and a listing never returns drawer content.
Reads are gated as reads and creates and deletes as writes, see [Memory modes](memory-modes.md).

There is deliberately no MCP tool for these routes, as for token generation and the database endpoint:
deleting a wing is a human decision, and an agent that can file memories can already do so through
`memcastle_checkpoint` and `memcastle_diary_write`.
The decision is recorded in [ADR-018](adr/018-palace-hierarchy-management.md).

### Authentication

Authentication is optional and off by default.
When the daemon has it enabled, every request, REST and `/mcp` alike, must carry `Authorization: Bearer <token>`.
The one exception is `GET /api/health`, which stays open for liveness probes.

```sh
curl -s -H "Authorization: Bearer $MEMCASTLE_AUTH_TOKEN" http://127.0.0.1:8420/api/status
```

The two `/api/auth/token` routes are open while authentication is disabled, which is how the first token is made,
and need a valid token once it is enabled.
The token in the response to `POST /api/auth/token` is the only time it is ever shown, and the response is sent with
`Cache-Control: no-store`.
Neither route has an MCP tool: MCP exposes memory capabilities and never credential management.
The status report's `auth_enabled` says whether the daemon requires a token, and never includes one.

### The database admin endpoint

The three `/api/db` routes open and close a separate listener that speaks SurrealDB's own protocol, so SurrealDB Studio
can inspect the live database.
They are guarded by [authentication](authentication.md) like every other route, and have no MCP tool:
an agent must not be able to open a database console onto the palace.
The listener itself is not part of this API: it is not MemCastle's REST or MCP, and its protocol is SurrealDB's.
The routes answer with `{running, addr, url, namespace, database, user, remote, auth_required, started_at, already_running}`.
`already_running` is `true` only on a `POST` that found the endpoint already open and asked for nothing different:
that answer is a `200` with the open endpoint's details.
A `POST` asking for another `bind`, `port` or origin than the open endpoint has is a `409` with
`memcastle::db::already_running`; a `port` of `0` means "any free port" and so never conflicts.
[Database access](database-access.md) describes the workflow and the security model.

### Submitting jobs

`POST /api/jobs` takes a body tagged by `type`:

| `type` | Other fields |
|---|---|
| `mine` | `path` (absolute directory) or `provider` (a [source](mining-sources.md)) with `locator?`, then `wing?` and `full?` |
| `checkpoint` | `payload` (see [above](#checkpoint-payload)), `emergency?` |
| `audit` | `scope?` |
| `repair` | `dry_run?` (default `true`), `based_on_job?` |
| `demo` | `steps` |

```sh
curl -s -X POST http://127.0.0.1:8420/api/jobs \
  -H 'Content-Type: application/json' \
  -d '{"type": "mine", "path": "/home/alice/project", "wing": "project"}'
```

```sh
curl -s -X POST http://127.0.0.1:8420/api/jobs \
  -H 'Content-Type: application/json' \
  -d '{"type": "mine", "provider": "pi-sessions"}'
```

An unknown `provider` is a `400` that names the known ones.
`GET /api/sources` lists the providers and the sources that have been mined:

```json
{
  "providers": [{"name": "pi-sessions", "description": "...", "capabilities": {"incremental": true, "retains_raw": true, "needs_credentials": false}}],
  "sources": [{"id": "...", "provider": "pi-sessions", "account": null, "locator": "/home/alice/.pi/agent/sessions",
               "cursor": {"mtime_ns": 1784039240000000000, "key": "..."}, "last_job": "...", "last_run_at": "...", "documents": 12}]
}
```

It is a read, so a `disabled` session is refused, and it is guarded like every route but the health check.
There is no MCP tool for it, and none for credentials.

The `demo` job kind is REST and CLI only; it is not offered over MCP.

## Errors

Whichever interface you use, a failure has the same three fields:

```json
{
  "error": "`diary_write` is not permitted in read_only mode",
  "code": "memcastle::app::mode_forbidden",
  "help": "switch the session/request to Full mode to allow writes, or to Full/ReadOnly to allow reads"
}
```

`error` says what failed, `help` says what to do, and `code` is a stable identifier to match on.
The REST API sends it as the response body with a matching HTTP status,
MCP returns it as the text of an error result, and the CLI prints it as a diagnostic.
See [Troubleshooting](troubleshooting.md) for the codes you are most likely to meet.
