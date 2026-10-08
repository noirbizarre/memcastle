# MCP tools and REST API

The daemon serves two interfaces on the same listener (`127.0.0.1:8420` by default, see
[Configuration](configuration.md#the-listener-address-and-port)):

- **MCP** at `/mcp`, over streamable HTTP, for agents.
  [Connect an MCP client](mcp-clients.md) shows how to point a client at it.
- **REST** under `/api`, which is what the CLI uses, what the [web UI](web.md) calls, and what a script can call.

Both are thin layers over the same application services, so they accept the same arguments,
enforce the same [memory modes](memory-modes.md) and fail with the same error body.

## MCP tools

Every tool returns pretty-printed JSON as text.
A failure comes back as an MCP error result carrying the [error body](#errors) below.

| Tool | Arguments | Does |
|---|---|---|
| `memcastle_set_mode` | `mode` | Choose this session's [memory mode](memory-modes.md). |
| `memcastle_status` | none | Daemon health: version, uptime, pid, address, palace, datastore and migration state, counts. |
| `memcastle_search` | `query`, `limit?`, `wing?`, `room?`, `ranking?`, `tags?`, `source_kind?`, `as_of?`, `from?`, `until?`, `include_historical?`, `expand?` | Search drawer content: lexical, semantic or hybrid, see [Searching](#searching). |
| `memcastle_recall` | the same, without `room` | Verbatim recall of matching content. |
| `memcastle_history` | `drawer_id` | How one piece of knowledge evolved: every version of a drawer's supersession chain, oldest first, see [History](#history). |
| `memcastle_fact_history` | `relationship_id`, `as_of?` | An assertion and its linked confirmations, conflicts or corrections, with provenance and reasons. Read-only. |
| `memcastle_wake_up` | `agent_identity`, `wing?`, `max_items?`, `max_bytes?` | Session-start context for an agent. |
| `memcastle_diary_write` | `agent_identity`, `wing`, `content` | Write a diary entry. |
| `memcastle_diary_read` | `agent_identity`, `wing`, `limit?` | Read an agent's newest diary entries. |
| `memcastle_checkpoint` | `payload`, `emergency?` | Submit a durable checkpoint job. |
| `memcastle_mine` | `path` or `source`, `locator?`, `options?`, `full?`, `wing?` | Submit a job that mines a directory, or a [source](mining-sources.md) such as `pi`. |
| `memcastle_miner_list` | none | The configured [miners](configuration.md#miners): name, source, state, scope and last run. Read-only. |
| `memcastle_miner_get` | `name` | One configured miner, and why it cannot run when it cannot. Read-only. |
| `memcastle_trigger_list` | none | The configured [triggers](triggers.md): miner, type, whether the user enabled them, whether they work, last fired and last error. Read-only. |
| `memcastle_trigger_get` | `name` | One configured trigger, and what it still needs when it cannot be enabled yet. Read-only. |
| `memcastle_audit` | `wing?` | Submit a read-only consistency audit job. |
| `memcastle_repair` | `dry_run?`, `based_on_job?` | Submit a repair job; a dry run unless `dry_run` is `false`. |
| `memcastle_job_list` | `status?` | List jobs, newest first. |
| `memcastle_job_get` | `id` | Show one job. |
| `memcastle_job_pause` | `id` | Ask a running job to pause at its next checkpoint. |
| `memcastle_job_resume` | `id` | Resume a paused job. |
| `memcastle_job_cancel` | `id` | Cancel a queued, paused or running job. |
| `memcastle_job_retry` | `id` | Retry a failed job. |

Defaults are the same as the CLI's: `limit` is 10 for `memcastle_search` and `memcastle_recall`, 20 for
`memcastle_diary_read`, and every read limit is capped at 200.
A `limit` of `0` means the default, on the CLI and REST as well.
`memcastle_wake_up` defaults to 10 items and 8192 bytes.
`memcastle_mine` (and a `mine` job over `POST /api/jobs`) needs an absolute `path`,
because the daemon does not share your shell's working directory.
Give exactly one of `path` and `source`: a source is mined incrementally from where its last run stopped,
and `full` reads it again from the beginning.
`options` is an object of strings for the source (`{"since": "2026-09", "dir": "/work/app"}`);
each source declares the keys it accepts in `GET /api/sources`, and any other key is refused with a `400` that lists them.
Mining, checkpoint, audit and repair return the submitted job immediately;
poll it with `memcastle_job_get` to see its progress and result.

### Tool annotations

Every tool advertises a `title` and all four [MCP behaviour hints](https://modelcontextprotocol.io/specification/2025-06-18/schema#toolannotations)
as explicit booleans, so a host can warn before it runs one.
The protocol's defaults assume the worst (destructive, open-world, not idempotent),
and some directories reject a tool whose hint is missing.

| Tool | `readOnlyHint` | `destructiveHint` | `idempotentHint` | `openWorldHint` |
|---|---|---|---|---|
| `memcastle_status`, `memcastle_history`, `memcastle_fact_history`, `memcastle_wake_up`, `memcastle_diary_read`, `memcastle_job_list`, `memcastle_job_get`, `memcastle_miner_list`, `memcastle_miner_get` | true | false | true | false |
| `memcastle_search`, `memcastle_recall` | true | false | true | true |
| `memcastle_set_mode` | false | false | true | false |
| `memcastle_mine` | false | false | true | true |
| `memcastle_checkpoint`, `memcastle_repair` | false | true | false | false |
| `memcastle_audit`, `memcastle_diary_write` | false | false | false | false |
| `memcastle_job_pause`, `memcastle_job_resume`, `memcastle_job_retry` | false | false | true | false |
| `memcastle_job_cancel` | false | true | true | false |

A few of these are judgement calls:

- Search and recall are open-world because a configured embedding provider (a command or an HTTP endpoint)
  receives the query text.
- `memcastle_mine` is open-world because it reads the local filesystem and an installed source may be granted the network.
  It is idempotent because mining skips unchanged documents.
- `memcastle_checkpoint` is destructive because a fact can supersede or invalidate an earlier one.
  `memcastle_repair` is destructive because `dry_run=false` removes orphan drawers, though it defaults to a dry run.
- The job-control tools are idempotent because repeating one is refused and leaves the job as it was.
  Cancelling drops the work, so it is destructive.
- `memcastle_audit` and `memcastle_diary_write` are not idempotent because each call adds a job record or a drawer.

A test pins this table, and fails for a tool that has no row.

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
| `source` | Where the memory came from: `kind` is `file` or `manual` (the MCP schema allows only these; REST and the CLI also accept `transcript` and `note`, and store any other value as `other`), `uri` and `agent` are optional. |
| `name` | Optional name for the drawer, unique within its room, so it can be addressed as `wing/room/name`. A name held by another drawer fails the item with `memcastle::palace::drawer_name_taken`; an unusable one is refused at submission. |
| `fact` | Optional knowledge-graph change made alongside the drawer: `{"op": "add" \| "supersede" \| "invalidate" \| "link", ...}`; a `confidence` outside 0 to 1 is refused at submission. |

`supersede` and `invalidate` accept an optional `reason` and record the checkpoint drawer as the correction's evidence.
`link` takes `relationship_id`, `other_id`, `kind` (`confirms`, `contradicts` or `refines`) and a required nonblank `reason`.
It relates two existing assertions without closing either; use `supersede` to replace one atomically.

Over MCP, `payload` is a JSON object, and the tool's input schema describes its shape.
A JSON-encoded string of that object is accepted too, because some clients serialise it before sending.
A string that is not valid JSON is refused as `memcastle::input::invalid`, blamed on `payload`.

Every item produces a drawer.
A payload with no items, or an item whose `content` is blank, is refused at submission as `memcastle::input::invalid`,
blamed on `payload`, so a job never completes having stored nothing.
A job that is interrupted resumes where it stopped and never stores an item twice.
An unnamed item whose content is identical to a drawer already valid in its room is not stored again;
it still counts as done, and the job's result reports `{"items": n, "duplicates": d}`.
A fact-bearing item always keeps its own drawer, even if the content matches, so its assertion or correction has evidence.
A named item is always stored, and a likely copy (a typo, a case variant) is stored and linked, never merged:
see [Deduplication](deduplication.md).

## REST API

Requests and responses are JSON.
The routes that read or write memory (including `GET /api/jobs*`) accept the optional `X-MemCastle-Mode` header,
and `/api/status` accepts it only to report the mode
(`full`, `read_only` or `disabled`; `full` when absent), and an unrecognized value is a `400`, never silently treated
as `full`.
`/api/health`, job control (pause, resume, cancel, retry) and `/api/shutdown` are never gated and ignore it.
`GET /api/events` is a read: it accepts the header and refuses a `disabled` session.

A mode the daemon refuses is a `403` with the code `memcastle::mode::forbidden`.
A request without a valid token, on a daemon with [authentication](authentication.md) enabled,
is a `401` with the code `memcastle::auth::unauthorized` and a `WWW-Authenticate: Bearer` header.
Other statuses are `400` for invalid input, a transition the job's state does not allow, or a database admin endpoint
that would be unsafe (`memcastle::db::unsafe_bind`), `404` for an unknown job, wing, room or drawer,
`409` for a job recorded as running that has no worker (restart the daemon), a job that kept changing state
under the request (run it again), a database admin endpoint that is already
open or cannot bind its address (`memcastle::db::bind_failed`), a drawer name already held by other content,
a wing or room delete while a job that writes to the palace is pending, or superseding a drawer that was already
superseded (`memcastle::palace::drawer_superseded`), `502` when the embedding or extraction provider fails
(`memcastle::embed::failed`, `memcastle::extract::failed`), and `500` for a server failure.
A search that needs a vector it cannot get (`memcastle::search::semantic_unavailable`) and a request that needs an embedding
provider when none is configured (`memcastle::embed::not_configured`, `memcastle::extract::not_configured`) are `400`s.

| Route | Purpose | Parameters |
|---|---|---|
| `GET /api/health` | Liveness: `{"status": "ok"}`. Touches nothing else. | none |
| `GET /api/status` | The full status report, also used by `memcastle status`. | none |
| `GET /api/events` | A stream of change notices (server-sent events), so a client can read again instead of polling: see [The event stream](#the-event-stream). A read, so a `disabled` session gets a `403`. There is no MCP tool. | none |
| `GET /api/config` | The configuration in effect, for the [dashboard's](web.md) settings page: listener, palace, datastore, whether authentication and the dashboard are on, where the runtime assets come from, scheduler limits, provider names and models, mining limits. It carries no token, key, URL or command line, and is daemon information like `status`, so no memory mode gates it. There is no MCP tool. | none |
| `GET /api/search` | Search drawers. | query string: `q` (or `query`), `limit`, `wing`, `room`, `ranking`, `tags`, `source_kind`, `as_of`, `from`, `until`, `include_historical`, `expand` |
| `POST /api/search` | The same search as a JSON [`SearchQuery`](#searching), the only way to send a `query_embedding`. | JSON body |
| `GET /api/recall` | Recall drawers. | as `GET /api/search`, and `room` is ignored |
| `POST /api/recall` | The JSON form of recall. | JSON body |
| `GET /api/wake-up` | Session-start context. | `agent_identity`, `wing`, `max_items`, `max_bytes` |
| `GET /api/diary` | Read diary entries. | `agent_identity`, `wing`, `limit` |
| `POST /api/diary` | Write a diary entry. | JSON body: `agent_identity`, `wing`, `content`, `requested_by?` |
| `POST /api/notes` | Capture a note: an unnamed drawer of source kind `note`. `201` when stored, `200` when an identical note was already in the room. There is no MCP tool: agents write memory through `memcastle_checkpoint` and the diary. | JSON body: `wing`, `room`, `content`, `uri?`, `requested_by?` |
| `GET /api/sources` | The mining adapters this daemon can run and the sources that have been mined: `{adapters, sources}`. A read. | none |
| `GET /api/miners` | The configured [miners](configuration.md#miners): `{miners, config_file?, error?}`. `error` says why the configuration file cannot be read, and `miners` is then the last good copy. A read. | none |
| `GET /api/miners/{name}` | One miner with its state (`ready`, `disabled`, `unavailable` and why), its credential as a kind and an availability, and the source it has mined. A read. | none |
| `PUT /api/miners/{name}` | Create the miner, or change the fields the body names: `{miner, created, changed, identity_changed}`. A change that widens the scope is a `409` unless `allow_broaden` is set. | JSON body, all optional once the miner exists: `source`, `enabled`, `locator`, `wing`, `credential`, `scope`, `unset_scope`, `trigger`, `config`, `unset_config`, `unset`, `allow_broaden` |
| `POST /api/miners/{name}/enable` | Switch a miner on, after checking that it can run. Idempotent. | none |
| `POST /api/miners/{name}/disable` | Switch a miner off. Idempotent. | none |
| `DELETE /api/miners/{name}` | Remove a miner's definition; what it mined stays: `{"removed": name}`. | none |
| `POST /api/miners/reload` | Read the configuration file again now: `{miners, added, removed, changed, enabled, disabled, broadened}`. | none |
| `POST /api/miners/{name}/run` | Submit the miner's mining job, from the cursor its source already has. Answers the job. A write, so a read-only session is refused. | optional JSON body: `full`, `requested_by` |
| `GET /api/triggers` | The configured [triggers](triggers.md): `{triggers, webhook, config_file?, error?}`. `webhook` is the listener's state (`enabled`, `bind`, `port`, `allow_remote`, `listening?`). A read. | none |
| `GET /api/triggers/{name}` | One trigger: `status` (`disabled`, `active`, `failing`, `unavailable`), `reason`, `setup` (what enabling still needs), `running`, `endpoint` (a listening webhook's URL), counters, `next_due`, `last_error`. A read. | none |
| `PUT /api/triggers/{name}` | Create the trigger **disabled**, or change the fields the body names: `{trigger, created, changed}`. Enabling needs every prerequisite, else `409`. | JSON body, all optional once the trigger exists: `miner`, `type`, `enabled`, `credential`, `settings`, `unset_settings`, `unset` |
| `POST /api/triggers/{name}/enable` | Switch a trigger on, after checking that everything it needs is in place. Idempotent. | none |
| `POST /api/triggers/{name}/disable` | Switch a trigger off. Idempotent. | none |
| `DELETE /api/triggers/{name}` | Remove a trigger and what the daemon remembered about it: `{"removed": name}`. | none |
| `POST /api/triggers/reload` | Read the configuration file again now: `{triggers, added, removed, changed}`. | none |
| `POST /api/triggers/{name}/fire` | Ask for a run through the trigger now: `{outcome: "queued" \| "coalesced", job}`. A write, and refused (`409`) while the trigger is disabled. | none |
| `GET /api/jobs` | List jobs, newest first. Without `kind` and `limit` it answers every job; with either it answers a bounded page. | query string: `status`, `kind` (the job's `type`, such as `mine` or `audit`), `limit` (default 50, at most 200) |
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
| `POST /api/wings/{wing}/rooms/{room}/drawers` | Write a drawer, creating its wing and room if needed: `201` when stored, `200` with `created: false` when an unnamed drawer with identical content was already in the room. | JSON body: `content`, `name?`, `requested_by?` |
| `GET /api/wings/{wing}/rooms/{room}/drawers/{drawer}` | One drawer in full. | none |
| `DELETE /api/wings/{wing}/rooms/{room}/drawers/{drawer}` | Delete one drawer. | none |
| `POST /api/drawers/{id}/supersede` | End a drawer's validity now, with a replacement when `content` is given: `{superseded, replacement}`. | JSON body: `content?`, `tags?`, `requested_by?` |
| `PUT /api/drawers/{id}/embedding` | Attach a vector you computed to a drawer. | JSON body: `embedding` (768 numbers) |
| `GET /api/drawers/{id}/duplicates` | The drawers this one was recorded as a likely duplicate of, or that resemble it, with the evidence: `{drawer, similar}`. See [Deduplication](deduplication.md). | none |
| `GET /api/drawers/{id}/history` | How the knowledge a drawer belongs to evolved: `{drawer, versions}`, every version of its supersession chain oldest first, from any version of it. See [History](#history). | none |
| `POST /api/drawers/{id}/mentions` | Record that a drawer mentions an entity, for graph expansion: `201` when linked, `200` when it already was. The name converges on an entity it is a variant of. | JSON body: `name`, `kind` |
| `GET /api/entities` | List entities of the [knowledge graph](#the-knowledge-graph), by name. | query string: `name` (contains, any case), `kind`, `limit` (default 50, at most 200) |
| `GET /api/entities/{id}/relationships` | The relationships touching an entity, with provenance, validity and lifecycle explanation. | query string: `include_expired` or `as_of` (mutually exclusive) |
| `GET /api/relationships/{id}/history` | A fact and the facts linked to it, including historical assertions, decisions and evidence. | query string: `as_of?` (RFC 3339 or date) |
| `GET /api/entities/{id}/mentions` | The drawers that mention an entity, with the provenance of each link and the name as that drawer spelled it. | none |
| `GET /api/entities/{id}/candidates` | The entities this one resembles without having been equated with, or that resemble it. | none |
| `POST /api/entities/{id}/aliases` | Record another spelling of an entity, so later sightings converge on it. | JSON body: `alias` |
| `GET /api/graph` | A piece of the [knowledge graph](#the-knowledge-graph) in one answer, for drawing: `{nodes, edges, truncated}`. With `entity` it is that entity's neighbourhood (open facts, either direction); without it, the first entities by name and the facts among them. A read; there is no MCP tool. | query string: `entity`, `depth` (1 to 3, default 1), `limit` (entities, default 50, at most 200) |
| `POST /api/auth/token` | Generate a token, replacing any previous one: `{token, algorithm, version, created_at}`. | none |
| `DELETE /api/auth/token` | Revoke the generated token: `{"revoked": true}`. | none |
| `POST /api/source-packages` | Install a [source package](writing-sources.md): the body is the `.tar.gz` itself. Answers `{source, replaced}`. | query string: `consent` (the digest of the permissions agreed to), `enable` |
| `GET /api/source-packages/{name}` | One source, built in or installed: capabilities, state, permissions. | none |
| `POST /api/source-packages/{name}/enable` | Allow an installed source to be mined. Idempotent. | none |
| `POST /api/source-packages/{name}/disable` | Stop an installed source from being mined. Idempotent. | none |
| `DELETE /api/source-packages/{name}` | Remove an installed source, its files and its stored credentials: `{"removed": name}`. | none |
| `POST /api/source-packages/{name}/auth` | Start signing an installed source in with OAuth, and say what the user must do: `{flow, kind, user_code?, verification_uri?, url?, expires_in}`. Replaces a sign-in already waiting for the same source. | none |
| `POST /api/source-packages/{name}/auth/{flow}/wait` | Wait, up to `timeout` seconds, for the sign-in `flow` to finish: `{"status": "pending"}` until it does, then `{"status": "signed_in", source, signed_in, expires_at, scopes, stored_in}`. A sign-in that was declined or expired is an error answer. | query string: `timeout` (seconds, at most 30) |
| `GET /api/source-registry/search` | Search the configured registries: `{entries, warnings}`, each entry with the version an install would take and what is installed. | query string: `q`, `registry` |
| `GET /api/source-registry/sources/{name}` | Download and verify a source and say what installing it would do, installing nothing: `{name, version, description, origin, registry, signed_by, archive_digest, permissions, consent_digest, replaces}`. | query string: `version`, `registry` |
| `POST /api/source-registry/install` | Install a source by name from a registry. Answers `{source, replaced}`; a name the release ships is refused (`memcastle::source::bundled`). | JSON body: `name`, `version?`, `registry?`, `consent?`, `enable?` |
| `GET /api/source-registry/updates` | The installed sources that have a newer version: `{updates, warnings}`. | none |
| `POST /api/source-registry/update` | Update one source, or every source with an update. Answers a list of `{name, from, to, status}`, where `status` is `updated`, `current`, `needs_consent` (with `permissions` and `digest`) or `failed` (with `message`). | JSON body: `name?`, `consent?` |
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

### The dashboard's files

With [`web.enable`](configuration.md#web-ui), the same listener also serves the dashboard's static files under
`/ui` (`GET` and `HEAD` only).
They are not part of the API and carry no data; see [Web UI](web.md) and [Authentication](authentication.md#what-is-protected)
for why they are the one thing besides the liveness probe that needs no token.

### The event stream

`GET /api/events` is a [server-sent events](https://html.spec.whatwg.org/multipage/server-sent-events.html) stream
on the main listener, for the [dashboard](web.md) and any other client that would rather be told than poll.
It is an ordinary route: it needs the `Authorization: Bearer` token when [authentication](authentication.md) is on,
and a browser's `EventSource`, which cannot send one, is not the way to read it (use `fetch`, as the dashboard does).
It accepts `X-MemCastle-Mode`: a `disabled` session is refused with a `403`, and a `read_only` one gets the same stream.

Each frame is named by its kind and carries one JSON object:

```text
event: job
data: {"kind":"job","action":"updated","id":"0b2e…","job_kind":"mine","status":"running"}
```

| Frame | Sent when | Fields beyond `kind` and `action` |
|---|---|---|
| `open` | first, once the stream is live | none |
| `job` | a job is queued, claimed, makes progress, changes state or finishes | `id`, `job_kind`, `status` |
| `drawer` | a drawer is written, superseded or deleted; a mining run sends one without an `id` per document | `id`, except for a batch |
| `wing`, `room` | one is created or deleted | `id` |
| `entity` | an entity or its aliases changed; an extraction sweep sends one without an `id` per pass | `id`, except for a batch |
| `trigger` | a trigger fired, joined a waiting run, failed, or was defined, changed or removed | `id` (the trigger's name), `status`: `queued`, `coalesced`, `failed` or `changed` |
| `resync` | this connection fell behind and missed events | none |

`action` is `created`, `updated` or `deleted`.
An event names what changed and never what it holds: no title, text, job input, path or progress line.
Read the change through the ordinary route (`GET /api/jobs/{id}`, `GET /api/wings`, ...), which applies the memory mode.
After a `resync`, and after any reconnect, read everything you show, since events were missed while the stream was down.
A comment line is sent every 15 seconds so a proxy does not close a quiet stream (behind nginx, turn response buffering
off for this route).
The stream ends when the daemon shuts down.

Events come from this daemon only.
With several daemons on one remote palace, another daemon's writes are not announced here, so keep a manual refresh.

```sh
curl -N -H "Authorization: Bearer $MEMCASTLE_AUTH_TOKEN" http://127.0.0.1:8420/api/events
```

### Searching

`memcastle_search`, `memcastle_recall`, `GET /api/search` and `GET /api/recall` take the same options.
Only the query is required, so a plain `?q=word` means what it always did.
`as_of`, `from` with `until`, and `include_historical` each say which memory to look at, so they cannot be combined.

| Option | Meaning |
|---|---|
| `ranking` | `auto` (the default), `lexical`, `semantic` or `hybrid`. |
| `wing`, `room` | Restrict to a wing or room by name. `recall` ignores `room`. |
| `tags` | Drawers carrying every one of these tags: a list over MCP, comma-separated in a query string (`tags=a,b`). |
| `source_kind` | `file`, `manual`, `transcript`, `note` or `other`. |
| `as_of` | An RFC 3339 instant (`2026-01-31T12:00:00Z`) or a date (`2026-01-31`, midnight UTC): search the memory that was valid then. |
| `from`, `until` | An interval, in the same forms: search the memory that was valid at some moment of `[from, until)`. Both are required together. |
| `include_historical` | Also return memory that has been superseded. |
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
Time here is *validity time*: `valid_from` and `valid_to` say when a piece of knowledge was true,
and `created_at` says when MemCastle recorded it.
Only validity is searched, so a memory discovered long after the period it describes is found by asking about that period.
A drawer is valid from its `valid_from` until, but not including, its `valid_to`, and with no `valid_to` it stays valid.
Four questions are asked with the same options on every interface:

| You want | Ask with | Finds |
|---|---|---|
| What is true now? | nothing (the default) | What is valid at this moment. |
| What was true at date X? | `as_of=X` | What was valid at the instant X. |
| What was true during X to Y? | `from=X` and `until=Y` | Everything valid at some moment of the window `[X, Y)`, so a belief held only part of it is included. |
| Every version ever recorded? | `include_historical` | Current and superseded alike, with no time filter. |

An instant is an RFC 3339 timestamp or a `YYYY-MM-DD` date, read as midnight UTC at the start of that day:
`as_of=2026-01-01` is the very start of the year, and `from=2026-01-01&until=2026-02-01` is exactly January.
Both ends of an interval are required, `from` must be before `until`, and a request that breaks either rule fails with
`memcastle::input::invalid` naming the option.

The end of validity, and the end of a window, are exclusive.
A drawer superseded at an instant and its replacement are never both valid at that instant and never both absent,
and a window only touching a drawer's validity does not overlap it:
a drawer closed exactly at `from` is not found, and neither is one opened exactly at `until`.
A single instant is the window `[t, t + 1ns)`, so `as_of` and an interval are one rule and cannot disagree at a boundary.

The same rule applies to every ranking (lexical, semantic and hybrid) and to `expand`:
related drawers must be valid in the requested time too, and a `relates_to` hop is followed only when the relationship held
then.
For example, to learn what the database was at the start of the year and how that changed during it:

```sh
memcastle search "database we use" --as-of 2026-01-01
memcastle search "database we use" --from 2026-01-01 --until 2027-01-01
```

The first returns the one belief that was true on 1 January.
The second returns every belief held at some point in 2026, which is the way to notice that one changed.
Each hit carries its `valid_from`, its `valid_to` (absent while it is still true) and its `id`, so a hit can be followed
to its [history](#history).

**Expansion.**
With `expand`, drawers that share an entity with a hit (or sit one currently valid `relates_to` hop away) follow the direct
hits, best first, up to `limit` more.
Each carries a `graph` signal and `via`, the entities that connect it.
Expansion never reorders or replaces a direct hit.
Link drawers to entities with `POST /api/drawers/{id}/mentions` or `memcastle drawer mention`,
or let the [`extract` job](#the-knowledge-graph) do it for mined content.

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
`POST /api/search` takes the request as JSON, so it can carry a `query_embedding`.
Its text is `text` (or `query`, like everywhere else), and the scope is a nested `filter` rather than the flat
`wing`, `room` and `tags` of the `GET` form:

```json
{ "text": "…", "ranking": "semantic", "limit": 5,
  "filter": { "wing": "work", "tags": ["style"], "temporal": "current" },
  "query_embedding": [0.01, "… 768 numbers"] }
```

`temporal` is `"current"`, `"all"`, `{"as_of": "2026-01-31T12:00:00Z"}` or
`{"between": {"from": "2026-01-01T00:00:00Z", "until": "2026-02-01T00:00:00Z"}}`.
An interval whose `from` is not before its `until` is refused with `memcastle::input::invalid`.
Attach a document vector with `PUT /api/drawers/{id}/embedding`, whose body is `{"embedding": [...]}`.
Both need exactly 768 numbers: the dimension is part of the palace's schema.

**Correcting a drawer.**
`POST /api/drawers/{id}/supersede` closes the drawer and, when the body has `content`, files a replacement in the same room
from the same instant.
The replacement takes over the drawer's name (the old one stays reachable by id) and its tags unless `tags` says otherwise.
Without `content` the drawer is only invalidated.
The old content is never rewritten.
The two drawers are linked: the old one gains `superseded_by` and the replacement gains `supersedes`,
set together in the same transaction.
Superseding is a REST and CLI operation with no MCP tool, which is why an agent cannot rewrite what is true,
only read how it changed.
These routes take a drawer id, which every search hit carries.

### History

`memcastle_history`, `GET /api/drawers/{id}/history` and `memcastle drawer history` answer *how did this knowledge evolve?*
Given the id of any version of a piece of knowledge, current or superseded, they return the whole chain,
oldest first, by following the `supersedes` and `superseded_by` links in both directions:

```json
{
  "drawer": "…the id you asked about…",
  "versions": [
    { "id": "…", "content": "we run the database on postgres",
      "valid_from": "2025-01-01T00:00:00Z", "valid_to": "2025-06-01T00:00:00Z",
      "provenance": { "requested_by": "cli", "job_id": null }, "source": { "kind": "manual" },
      "created_at": "…", "superseded_by": "…" },
    { "id": "…", "content": "we run the database on surrealdb",
      "valid_from": "2025-06-01T00:00:00Z", "valid_to": null, "supersedes": "…" }
  ]
}
```

Each version is the drawer verbatim, so its identity, validity period, provenance and content are all kept.
A version's `valid_to` is the next version's `valid_from`, so the periods tile the timeline.
The last version has no `valid_to` while it is still true, and it has one when the knowledge was later invalidated
without a replacement.
A drawer nobody corrected is a history of one version, and an unknown id is `404` with `memcastle::palace::drawer_not_found`.
History is a read, so a read-only session may use it and a disabled one may not.

Neither `created_at` nor `updated_at` orders the chain: those are when MemCastle recorded each version,
which can be long after the period it describes, and the order is the order of the links.
Drawers that were superseded before MemCastle recorded the links are paired when the daemon first starts on the new version,
when exactly one successor qualifies; an unpaired one reads as a history of one version.

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

### Installing sources

The `/api/source-packages` routes install, inspect, enable, disable and remove [mining sources](writing-sources.md):
WebAssembly components that run in the daemon, in a sandbox, with only the permissions their manifest lists.
They are guarded by [authentication](authentication.md) like every other route, accept a body up to 64 MiB,
and have no MCP tool: an agent must not be able to install code or widen its own reach
([ADR-026](adr/026-pluggable-source-adapters-as-webassembly-components.md)).
Installing, enabling, disabling, removing and updating are not gated by a memory mode, which guards access to memory;
listing and showing (`GET /api/sources` and `GET .../{name}`) are reads, so a `disabled` session is refused them.

The `/api/source-registry` routes find sources in the registries configured under `mining.registries`
(the official one by default), and install or update them by name ([Publishing and installing sources](publishing-sources.md)).
They are the only routes through which the daemon reaches out for code, so they are held to the same rules:
guarded by authentication, no MCP tool, and the same consent to the permissions of exactly the package that was fetched
([ADR-033](adr/033-source-distribution.md)).
Searching and previewing are reads of an index and an archive, and nothing is installed until `install` or `update`.
A package that fails verification is `502` with `memcastle::source::integrity`, an unreadable registry is `502` with
`memcastle::source::registry_unavailable`, a name or version nobody offers is `404` with
`memcastle::source::not_in_registry`, and one the trust policy refuses is `400` with `memcastle::source::untrusted`.
A package that asks for permissions is refused with `memcastle::source::consent_required` (a `400`) unless `consent`
carries the digest of exactly those permissions, which the refusal's `help` names.
A built-in source cannot be disabled or removed (`memcastle::source::builtin`, `400`).
A bundled source is installed from the start, so it can be enabled and disabled like any other, and without consent,
but it cannot be installed from a registry, updated or removed (`memcastle::source::bundled`, `400`);
an unknown name is a `404`
(`memcastle::source::not_found`); enabling a source that is `unavailable`, or mining one that is not enabled, is a `409`
(`memcastle::source::not_enabled`).
`POST /api/jobs` refuses a `mine` for such a source the same way, at submission.

### Signing sources in

A source that cannot be reached with a static token declares an OAuth sign-in in its manifest ([Writing a mining source](writing-sources.md#signing-in-with-oauth)),
and two routes under `/api/source-packages/{name}` run it ([ADR-039](adr/039-oauth-credentials-for-mining-sources.md)).
`POST .../auth` starts the flow, which the daemon runs and finishes on its own, and answers what the user must do:
for the device flow (`kind: "device"`) a `user_code` to type at `verification_uri`,
with a `url` that has the code filled in when the provider offers one;
for the browser flow (`kind: "browser"`) a `url` to open, whose redirect lands on a one-shot listener the daemon opens on
the loopback address of its own machine, so that flow needs a browser on the daemon's machine.
`POST .../auth/{flow}/wait` is a long poll a client repeats until it is told the outcome.
Both answers carry no token, and are sent with `Cache-Control: no-store`.

They are guarded by [authentication](authentication.md) like every other route, and have no MCP tool:
starting a sign-in lets installed code act on someone's account, so an agent must not be able to begin one.
A source that does not declare a sign-in (a built-in one included) is `400` with `memcastle::credential::oauth_unsupported`,
a sign-in that was declined, expired or refused is `502` with `memcastle::credential::flow_failed`,
and a `wait` for a sign-in that was replaced, forgotten or already reported is the same code.
A run of a source that is not signed in fails with `memcastle::credential::required`,
which a `409` reports when a miner is asked to run,
and a credential that could not be renewed just now with `memcastle::credential::refresh_failed`, a `502`.
`GET /api/sources` and `GET /api/source-packages/{name}` add `auth` to a source that signs in:
`{signed_in, expires_at?, scopes, provider, flows}`.

### Miners

The `/api/miners` routes manage the `[[miners]]` section of the daemon's configuration file
([Configuration](configuration.md#miners), [ADR-037](adr/037-persistent-miner-configuration.md)).
Reading is offered to MCP (`memcastle_miner_list`, `memcastle_miner_get`), so an agent can say what is configured.
Changing a miner is REST and CLI only, like installing a source: no MCP tool adds, changes, enables, disables, removes,
reloads or runs one, so an agent cannot widen what the daemon mines.
Every route is guarded by [authentication](authentication.md).
Listing and showing are reads, so a `disabled` session is refused them (`403`); `run` is a write, because it files
drawers; creating, changing, enabling, disabling, removing and reloading are not gated by a memory mode,
which guards access to memory and not what the daemon is configured to do.

A definition that does not validate, or an enabled miner whose source is not usable or whose credential does not resolve,
is `400` with `memcastle::miner::invalid` and nothing is written.
An unknown miner is `404` (`memcastle::miner::not_found`).
A `409` is a change that would widen a scope (`memcastle::miner::scope_broadened`), a run of a disabled miner
(`memcastle::miner::disabled`), a run of one that cannot run as configured (`memcastle::miner::not_runnable`),
and a configuration file that cannot be read or was edited under the request (`memcastle::miner::config_file`).
A credential is shown as `{kind, available}`: the variable's name or the file's path is not given back.
`kind` is `env`, `file` or `oauth`, and for `oauth` `available` says whether the source is signed in.

### Triggers

The `/api/triggers` routes manage the `[[triggers]]` section of the daemon's configuration file
([Triggers](triggers.md), [ADR-043](adr/043-source-triggers.md)).
Reading is offered to MCP (`memcastle_trigger_list`, `memcastle_trigger_get`).
Defining, changing, enabling, disabling, removing, reloading and firing a trigger are REST and CLI only: no MCP tool does
any of them, so an agent cannot decide what the daemon does unattended.
Every route is guarded by [authentication](authentication.md).
Listing and showing are reads and `fire` is a write; the rest are not gated by a memory mode.

A definition that does not validate is `400` with `memcastle::trigger::invalid` and nothing is written.
A trigger that is enabled, or asked to be, while something it needs is missing is `409` with
`memcastle::trigger::not_activatable`, whose message says what to set up; nothing is written and nothing is started.
Firing a disabled trigger is `409` with `memcastle::trigger::disabled`, and an unknown one is `404`
(`memcastle::trigger::not_found`).
The trigger's shared secret is shown as `{kind, available}`, never its name, its path or its value.

A webhook **delivery is not one of these routes**.
It is `POST /hooks/{trigger}` on the separate [webhook listener](triggers.md#the-webhook-listener), which is off by
default, loopback by default and authenticates each delivery with its trigger's own secret, so every route of this API
stays behind the daemon's token.
It answers `202` with `{status: "queued" | "coalesced" | "duplicate", job}`, `401` (empty) for anything it will not
accept, `413` for a body over `webhook.max_body_bytes` and `429` with `Retry-After` while `webhook.max_concurrent`
deliveries are in flight.

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

### The knowledge graph

Entities and the relationships between them are derived from drawers, never the other way round.
Three things write them: a checkpoint's `fact`, `POST /api/drawers/{id}/mentions`,
and the `extract` job, which reads every mined drawer and note it has not read and adds what the text names.
`extract` is queued after a mining job completes, after a note is written and at startup, and `memcastle extract` or
`POST /api/jobs {"type": "extract", "wing": "docs"}` runs one by hand.
It needs an [`[extraction]` provider](configuration.md#extraction) and is a `400` with
`memcastle::extract::not_configured` without one; it is a write, so a read-only session is refused.
A provider that fails is a `502` with `memcastle::extract::failed` on the job, and the drawers it did not reach are read
next time.
There is no MCP tool for it, as there is none for embedding.

The `GET /api/entities` routes read the graph.
A relationship says what it relates and when it holds, and where an extractor derived it, what from:

```json
{
  "id": "…", "from": "…", "to": "…", "predicate": "works_on", "confidence": 0.6,
  "valid_from": "2026-07-14T14:27:12Z", "valid_to": null,
  "provenance": {
    "drawer": "…", "extractor": "heuristic", "job_id": "…", "extracted_at": "…",
    "origin": {"source_id": "…", "source": "directory", "document": "team.md", "chunk": 0, "revision": "…"}
  }
}
```

`provenance` is absent on a fact somebody asserted directly.
For a directly asserted checkpoint fact, `assertion` names its checkpoint drawer instead.
A fact stops being current (`valid_to` is set) when the drawer it was read from is superseded, and `include_expired`
returns that history too.
Each graph assertion also has `lifecycle: {state, links, preferred?, basis?}`.
States are `current`, `conflicting`, `superseded`, `invalidated` and `historical`.
Each link has `from`, `to`, `kind`, `origin`, `reason`, `at` and optional `evidence` (a drawer ID).
`preferred` is a deterministic ranking hint among conflicting assertions, never a declaration that the others are false.
The policy ranks direct assertions first, then confidence, validity start and fact ID; both sides remain visible.
The same metadata appears in `/api/graph`, the entity relationships route and `memcastle fact history`.
An `as_of` request computes the state at that instant and cannot be combined with `include_expired` on an entity listing.
The exact fact-history route accepts the ID of any related assertion, even one no longer current.
Drawer search/recall still returns canonical evidence and uses the existing validity filters.
A conflict does not delete or hide its source drawer.
Extracted facts use a closed vocabulary: kinds `person`, `organization`, `project`, `tool`, `place`, `concept` and `other`;
predicates `works_on`, `member_of`, `depends_on`, `uses`, `owns`, `part_of`, `located_in` and `related_to`.
An unknown entity is a `404` with `memcastle::graph::entity_not_found`.

An entity carries the `aliases` it has been seen under, and a name that differs only in spelling from a known entity
converges on it instead of adding a second one.
Each `mentions` entry has an `observation`, the name as the drawer wrote it, the rule that tied it to the entity and a
confidence, so resolving two spellings to one entity loses neither.
Names that could not be settled stay separate entities, listed by `GET /api/entities/{id}/candidates`.
There is no MCP tool for any of it.
[Deduplication](deduplication.md) has the rules.
The reasoning is in [ADR-024](adr/024-entity-extraction-as-an-enrich-job.md) and
[the lifecycle decision](adr/046-fact-lifecycle-and-contradictions.md).

### Submitting jobs

`POST /api/jobs` takes a body tagged by `type`:

| `type` | Other fields |
|---|---|
| `mine` | `path` (absolute directory) or `source` (a [source](mining-sources.md) by name) with `locator?`, then `options?` (an object of strings), `wing?` and `full?` |
| `checkpoint` | `payload` (see [above](#checkpoint-payload)), `emergency?` |
| `audit` | `wing?` |
| `embed` | `wing?`; fills the embeddings that are missing, and needs an `[embeddings]` provider (REST and CLI only, no MCP tool) |
| `extract` | `wing?`, see [the knowledge graph](#the-knowledge-graph) |
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
  -d '{"type": "mine", "source": "opencode", "options": {"since": "2026-09", "dir": "/work/app"}}'
```

An unknown `source` is a `400` that names the known ones, and an `options` key the source does not declare is a `400` that
names the ones it accepts.
Each entry of `adapters` lists them as `options: [{name, description, type}]`.
`GET /api/sources` lists the adapters and the sources that have been mined:

```json
{
  "adapters": [{"name": "pi", "description": "...", "capabilities": {"incremental": true, "retains_raw": true, "needs_credentials": false},
                 "origin": "package", "version": "0.1.0", "state": "enabled", "permissions": {...}}],
  "sources": [{"id": "...", "source": "pi", "account": null, "locator": "/home/alice/.pi/agent/sessions",
               "cursor": {"mtime_ns": 1784039240000000000, "key": "..."}, "last_job": "...", "last_run_at": "...", "documents": 12}]
}
```

An adapter's `origin` is `builtin` or `package`; an installed one also has a `version`, and a `state` of `installed`,
`enabled`, `disabled` or `unavailable` (with an `unavailable_reason`).
It is a read, so a `disabled` session is refused, and it is guarded like every route but the health check.
There is no MCP tool for it, and none for credentials or for installing sources.

The `demo` job kind is REST and CLI only; it is not offered over MCP.

## Errors

Whichever interface you use, a failure has the same three fields:

```json
{
  "error": "`diary_write` is not permitted in read_only mode",
  "code": "memcastle::mode::forbidden",
  "help": "switch the session/request to Full mode to allow writes, or to Full/ReadOnly to allow reads"
}
```

`error` says what failed, `help` says what to do, and `code` is a stable identifier to match on.
The REST API sends it as the response body with a matching HTTP status,
MCP returns it as the text of an error result, and the CLI prints it as a diagnostic.
See [Troubleshooting](troubleshooting.md) for the codes you are most likely to meet.
