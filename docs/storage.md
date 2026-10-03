# Storage and data

Everything MemCastle remembers lives in one SurrealDB database, which the daemon owns.
There is no second index file to fall out of sync with it.
By default the database is **embedded**: it runs inside the daemon and stores its files in a directory on disk,
so a local setup needs no external database service.

## Where things live

MemCastle uses the Unix XDG layout on Linux and macOS alike; macOS does not use `~/Library`.
Windows has no dedicated layout and uses the same dot-directories under your home directory.
[Configuration](configuration.md#where-files-live) explains how each location can be moved.

| What | Default location | Moved by |
|---|---|---|
| Palace (your memory) | `~/.local/share/memcastle/default/` | `XDG_DATA_HOME`, `--palace`, `MEMCASTLE_PALACE_PATH`, `palace.path` |
| Config file (optional) | `~/.config/memcastle/config.toml` | `XDG_CONFIG_HOME`, `--config`, `MEMCASTLE_CONFIG` |
| Daemon registry (runtime state) | `~/.local/state/memcastle/run/` | `XDG_STATE_HOME` |

Inside an embedded palace directory:

```text
~/.local/share/memcastle/default/
└── db/          the SurrealKV database: this is your data
```

Only `db/` matters.
It includes the daemon's own small tables, such as the stored [authentication](authentication.md) token digest
(`auth_state`), which therefore travels with a backup of the palace.
It is a SHA-256 digest of the generated token, never the token, and a secret configured through the environment or the
config file is not stored in it at all.
The registry file is deliberately kept outside the palace, so copying or backing up a palace never carries a stale
daemon record with it.

## The data model

A palace is organized as wings, rooms and drawers.

```mermaid
erDiagram
    PALACE ||--o{ WING : contains
    WING ||--o{ ROOM : contains
    ROOM ||--o{ DRAWER : holds
    DRAWER }o--o| JOB : "written by"
    ENTITY ||--o{ RELATES_TO : "subject of"
    ENTITY ||--o{ RELATES_TO : "object of"
```

- A **wing** is a project or a source, such as a repository or an agent's diary space.
- A **room** is a topical bucket inside a wing.
- A **drawer** is one verbatim piece of text, the atomic unit of memory.
  Its content is never rewritten, paraphrased or truncated.
  It also records where it came from (a file, or a manual entry by an agent), free-form tags,
  and which job wrote it and through which channel (`cli`, `http` or `mcp`).
  A drawer may also have a **name**, unique within its room, which makes it addressable as `wing/room/name`
  instead of by UUID.
  Most drawers have none: mining names a file's drawer after its path, and `drawer create` and a checkpoint
  item's `name` set one explicitly.

Where a write ends up:

| Written by | Wing | Room |
|---|---|---|
| `mine` | the `--wing` you give, or the directory's name | `files` |
| `diary write` | the wing you give | `diary` |
| `checkpoint` item | the item's `wing`, or `preferences`, `projects`, `diary` or `general` by destination | `diary` for diary items, `entries` otherwise |

Wings and rooms are managed with `memcastle wing`, `room` and `drawer`, see [CLI reference](cli.md#wings-rooms-and-drawers).
Nothing in the database cascades, so deleting a wing or a room removes its rooms and drawers explicitly, in one
transaction: the hierarchy is never left with children whose parent is gone, which is the very state `audit` reports
as orphans.
Counts shown for wings and rooms are computed when asked, never stored.

Entities and relationships form a knowledge-graph layer that a checkpoint item's `fact` can write to.
Nothing extracts them from mined content yet.

Search today is lexical (BM25 full-text) over drawer content; there are no embeddings.
That is why `embedding` is empty in every drawer you see.

### Limits

- Read commands return 10 results by default (20 for the diary) and never more than 200.
- A `wake-up` returns at most 10 highlights and 8192 bytes unless told otherwise.

### What mining reads

`memcastle mine <dir>` creates one drawer per file, in name order, with these rules:

- It skips directories named `.git`, `target`, `node_modules`, `.venv`, `venv`, `dist`, `build` and `.cache`, at any depth,
  along with any other directory whose name starts with `.` and any symlink.
- It skips files larger than 256 KiB, empty files, and files that are not valid UTF-8.
- It stops at 2000 files, and says so: the job's progress message names the limit and its result records
  `"truncated": true`.

Each mining job files its own drawers, so mining the same directory twice stores its files twice.
Only the first copy of a file keeps its name: the name is unique within the room, so the later copies are unnamed
and are reached by their UUID.
A job that is paused or interrupted and then resumed does not: it continues where it stopped
and never stores a file twice.

## Embedded and remote stores

The store is chosen with `store.mode` in the config file.

**Embedded** (the default) keeps the database in `<palace>/db`.
SurrealKV's file lock lets only one daemon open it at a time, which is what guarantees a single writer.
`memcastle migrate` while the daemon runs fails with `memcastle::store::backend_failed`,
and so does a second `memcastle serve` on the same palace that listens on a different port.
A second `serve` on the same address never gets that far: the listener is bound first,
so it fails with `memcastle::server::bind_failed`.
Stop the daemon first, or point at a different palace.

**Remote** connects to a SurrealDB server over `ws://` or `wss://`:

```toml
[store]
mode = "remote"
url = "ws://localhost:8000"
namespace = "memcastle"
database = "main"
username = "root"
password = "..."
```

Several daemons may share a remote palace.
Job leases keep them from running the same job twice, and a daemon that stalls loses its jobs to another after
`jobs.lease_ttl_secs` ([ADR-006](adr/006-job-leases.md)).
Only root sign-in is supported today, and these settings can only be set in the config file.
`memcastle status` never prints the password.

An embedded store uses the SurrealDB namespace `memcastle` and database `palace`;
a remote store uses the `namespace` and `database` set in `[store]`.

To look inside an embedded store with SurrealDB Studio, use `memcastle db start`
([Database access](database-access.md)), never a separate `surreal start` on the directory:
SurrealKV admits one process, and that process is the daemon.

## The registry file

A running daemon writes `daemon.json` so clients can find it:

```text
~/.local/state/memcastle/run/<palace-hash>/daemon.json
```

`<palace-hash>` is the first 16 hex characters of the SHA-256 of the palace's canonical path,
so each palace has its own file and several daemons can coexist.

```json
{
  "pid": 2529296,
  "bind_addr": "127.0.0.1:8420",
  "started_at": "2026-09-30T01:32:24.049797506+00:00",
  "version": "0.1.0"
}
```

The file is written once the daemon is ready and removed when it shuts down cleanly.
If no home directory can be determined, the registry falls back to a `memcastle/run` directory under the system
temporary directory.
It is safe to delete by hand when no daemon is running; `memcastle status` calls a leftover one `stale`.

## Back up and move a palace

The embedded database is a directory of files that only the running daemon should write.
To make a consistent copy, stop the daemon first:

```sh
memcastle daemon stop
cp -a ~/.local/share/memcastle/default /path/to/backup/
memcastle serve
```

To move a palace, copy the directory and start the daemon with `--palace` (or `palace.path`) pointing at the new place.
Run a newer MemCastle over an older palace as usual; migrations bring it up to date on start,
see [Migrations and upgrades](migrations.md).

To start over, stop the daemon and delete the palace directory.
The next `serve` creates a fresh one.
