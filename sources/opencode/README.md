# OpenCode history source

The conversation history of the [OpenCode](https://opencode.ai) coding agent, as a MemCastle mining source:
a WebAssembly component built from `src/lib.rs`, one document per session (docs/adr/030).

It is acquisition only.
MemCastle owns normalisation into drawers, chunking, deduplication, provenance, the cursor, idempotency and the durable
job, and the live OpenCode integration (`integrations/opencode/`) is separate: it talks to the daemon over MCP and only
decides when to ask for mining.

## Requirements

OpenCode 1.2 or later (the version that moved history into a database), installed so that `opencode` is on the daemon's
`PATH`; it was written against 1.18.
The source never opens OpenCode's database: it runs `opencode db` and `opencode export`, which own it, so no SQLite code is
part of this component or of MemCastle.

## Use it

```sh
# `fixtures/bin` holds a stand-in `opencode` for the conformance cases; they need it first on the PATH.
PATH="$PWD/sources/opencode/fixtures/bin:$PATH" memcastle source test sources/opencode
memcastle source package sources/opencode                # sources/opencode/dist/opencode-0.1.0.tar.gz
memcastle source install sources/opencode/dist/opencode-0.1.0.tar.gz --enable
memcastle mine opencode                       # or: memcastle mine opencode since=2026-09 dir=/path/to/workspace
```

The build needs the `wasm32-wasip2` target (`rustup target add wasm32-wasip2`).
[Mining sources](../../docs/mining-sources.md#opencode) describes what is filed, what is left out and the limits.

## Capabilities and permissions

| Declared | Value | Why |
|---|---|---|
| `incremental` | yes | A `time_updated` watermark finds the sessions that changed; only their new tail is filed again. |
| `retains_raw` | yes | OpenCode's database is the user's to prune and compact, so the transcript is kept once read. |
| `needs_credentials` | no | OpenCode is run as the user, with whatever access it already has. |
| `process` | `opencode` | The one program run, by exact name and without a shell. It runs with the daemon's own authority. |
| `env` | `XDG_DATA_HOME` | Read by OpenCode to find its data directory when it has been moved. |
| `filesystem`, `network` | none | The component reads no file and reaches no network. |

## Identity and provenance

| Field | Value |
|---|---|
| `external_id` | The OpenCode session id, such as `ses_fffdd730a0010XFnQ4chp5K3uS`. |
| `revision` | A hash of the whole export, so it changes exactly when the session does. |
| `occurred_at` | When the session was created. |
| metadata | `session_id`, `directory`, `project_id`, `title`, `version` and `parent_id`. |
| `uri` | `opencode://session/<id>`. |
| room, name, tags | The working directory's name, the session id, `transcript` and `opencode`. |

## Tests

`fixtures/opencode-history/` is the conformance case `memcastle source test` runs: four sessions in two working
directories and the usual noise.
`fixtures/bin/opencode` stands in for the real command and answers from `fixtures/data/`, so nothing here needs OpenCode
or a database.
`tests/wasm_opencode.rs` in the repository runs the same component through the sandbox and a real daemon.
