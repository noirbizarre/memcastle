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
| `memcastle_search` | `query`, `limit?`, `wing?`, `room?` | Full-text search over drawer content. |
| `memcastle_recall` | `query`, `limit?`, `wing?` | Verbatim recall of matching content. |
| `memcastle_wake_up` | `agent_identity`, `wing?`, `max_items?`, `max_bytes?` | Session-start context for an agent. |
| `memcastle_diary_write` | `agent_identity`, `wing`, `content` | Write a diary entry. |
| `memcastle_diary_read` | `agent_identity`, `wing`, `limit?` | Read an agent's newest diary entries. |
| `memcastle_checkpoint` | `payload`, `emergency?` | Submit a durable checkpoint job. |
| `memcastle_mine` | `path`, `wing?` | Submit a job that mines a directory. |
| `memcastle_audit` | `scope?` | Submit a read-only consistency audit job. |
| `memcastle_repair` | `dry_run?`, `based_on_job?` | Submit a repair job; a dry run unless `dry_run` is `false`. |
| `memcastle_jobs_list` | `status?` | List jobs, newest first. |
| `memcastle_job_get` | `id` | Show one job. |
| `memcastle_job_pause` | `id` | Ask a running job to pause at its next checkpoint. |
| `memcastle_job_resume` | `id` | Resume a paused job. |
| `memcastle_job_cancel` | `id` | Cancel a queued, paused or running job. |
| `memcastle_job_retry` | `id` | Retry a failed job. |

Defaults are the same as the CLI's: `limit` is 10 for `memcastle_search` and `memcastle_recall`, 20 for
`memcastle_diary_read`, and every read limit is capped at 200.
`memcastle_wake_up` defaults to 10 items and 8192 bytes.
`memcastle_mine` (and a `mine` job over `POST /api/jobs`) needs an absolute `path`, because the daemon does not share your shell's working directory.
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
| `source` | Where the memory came from: `kind` is `file` or `manual`, `uri` and `agent` are optional. |
| `fact` | Optional knowledge-graph change made alongside the drawer: `{"op": "add" \| "supersede" \| "invalidate", ...}`. |

Every item produces a drawer.
A job that is interrupted resumes where it stopped and never stores an item twice.

## REST API

Requests and responses are JSON.
The routes that read or write memory, and `/api/status`, accept the optional `X-MemCastle-Mode` header
(`full`, `read_only` or `disabled`; `full` when absent), and an unrecognized value is a `400`, never silently treated
as `full`.
`/api/health`, job control (pause, resume, cancel, retry) and `/api/shutdown` are never gated and ignore it.

A mode the daemon refuses is a `403` with the code `memcastle::app::mode_forbidden`.
A request without a valid token, on a daemon with [authentication](authentication.md) enabled,
is a `401` with the code `memcastle::auth::unauthorized` and a `WWW-Authenticate: Bearer` header.
Other statuses are `400` for invalid input or a transition the job's state does not allow, `404` for an unknown job,
`409` for a job recorded as running that has no worker (restart the daemon), and `500` for a server failure.

| Route | Purpose | Parameters |
|---|---|---|
| `GET /api/health` | Liveness: `{"status": "ok"}`. Touches nothing else. | none |
| `GET /api/status` | The full status report, also used by `memcastle status`. | none |
| `GET /api/search` | Search drawers. | query string: `q` (or `query`), `limit`, `wing`, `room` |
| `GET /api/recall` | Recall drawers. | `q` (or `query`), `limit`, `wing` |
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
| `POST /api/auth/token` | Generate a token, replacing any previous one: `{token, algorithm, version, created_at}`. | none |
| `DELETE /api/auth/token` | Revoke the generated token: `{"revoked": true}`. | none |

`requested_by` records which channel a write came through; it defaults to `http` (the CLI sends `cli`, MCP uses `mcp`).
The status report keeps `/api/health` cheap: an unhealthy datastore is reported inside `/api/status` with a `200`,
so a supervisor restarting on `/api/health` does not restart a daemon that would recover on its own.

```sh
curl -s http://127.0.0.1:8420/api/health
curl -s 'http://127.0.0.1:8420/api/search?q=formatter&limit=5'
curl -s http://127.0.0.1:8420/api/jobs?status=running
```

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

### Submitting jobs

`POST /api/jobs` takes a body tagged by `type`:

| `type` | Other fields |
|---|---|
| `mine` | `path` (absolute directory), `wing?` |
| `checkpoint` | `payload` (see [above](#checkpoint-payload)), `emergency?` |
| `audit` | `scope?` |
| `repair` | `dry_run?` (default `true`), `based_on_job?` |
| `demo` | `steps` |

```sh
curl -s -X POST http://127.0.0.1:8420/api/jobs \
  -H 'Content-Type: application/json' \
  -d '{"type": "mine", "path": "/home/alice/project", "wing": "project"}'
```

The `demo` job kind is REST and CLI only; it is not offered over MCP.

## Errors

Whichever interface you use, a failure has the same three fields:

```json
{
  "error": "`diary_write` is not permitted in ReadOnly mode",
  "code": "memcastle::app::mode_forbidden",
  "help": "switch the session/request to Full mode to allow writes, or to Full/ReadOnly to allow reads"
}
```

`error` says what failed, `help` says what to do, and `code` is a stable identifier to match on.
The REST API sends it as the response body with a matching HTTP status,
MCP returns it as the text of an error result, and the CLI prints it as a diagnostic.
See [Troubleshooting](troubleshooting.md) for the codes you are most likely to meet.
