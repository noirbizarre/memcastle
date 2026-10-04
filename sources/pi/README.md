# Pi history source

The conversation history of the [Pi](https://github.com/badlogic/pi-mono) coding agent, as a MemCastle mining source:
a WebAssembly component built from `src/lib.rs`, one document per session file (docs/adr/027).

It is acquisition only.
MemCastle owns normalisation into drawers, chunking, deduplication, provenance, the cursor, idempotency and the durable
job, and the live Pi integration (`integrations/pi/`) is separate: it talks to the daemon over MCP and only decides when
to ask for mining.

## Use it

```sh
memcastle source test sources/pi                         # build, then run fixtures/ in the daemon's own sandbox
memcastle source package sources/pi                      # sources/pi/dist/pi-0.1.0.tar.gz
memcastle source install sources/pi/dist/pi-0.1.0.tar.gz --enable
memcastle mine --source pi                               # ~/.pi/agent/sessions
memcastle mine --source pi --locator /backups/pi/sessions
```

The build needs the `wasm32-wasip2` target (`rustup target add wasm32-wasip2`).
[Mining sources](../../docs/mining-sources.md#pi) describes what is filed and what is left out.

## Capabilities and permissions

| Declared | Value | Why |
|---|---|---|
| `incremental` | yes | A modification-time watermark finds the sessions that grew; only their new tail is filed again. |
| `retains_raw` | yes | Pi's session files are rotated and deleted by the user, so the transcript is kept once read. |
| `needs_credentials` | no | It reads local files. |
| `filesystem.read` | `locator`, `~/.pi/agent/sessions` | The folder being mined, and Pi's own, the default. Read-only. |
| `env` | `HOME` | Only to name `~/.pi/agent/sessions` when no locator is given. |
| `network`, `process` | none | Nothing else is reachable. |

The source lists only `*.jsonl` files directly in the sessions folder or one folder down, never follows a symlink, and so
never opens Pi's credentials file (`auth.json`).
It follows Pi's session format version 3 and skips a line or an entry it does not understand instead of failing.

## Identity and provenance

| Field | Value |
|---|---|
| `external_id` | The path under the sessions folder, such as `--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl`. |
| `revision` | A hash of the whole file, so it changes exactly when the session does. |
| `occurred_at` | The session header's `timestamp`, when it is RFC 3339. |
| metadata | `path`, and the header's `id` (session id), `cwd` and `version`. |
| room, name, tags | The working directory's name, the file's name, `transcript` and `pi`. |

## Tests

`fixtures/pi-sessions/` is the conformance case `memcastle source test` runs: three sessions in two working directories
and the usual noise.
`tests/wasm_pi.rs` in the repository runs the same component through the sandbox and a real daemon.
