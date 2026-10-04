# Configuration

MemCastle runs with no configuration at all.
Everything below is for changing where it keeps things, or how it behaves.

## Where files live

MemCastle follows the **Unix XDG Base Directory convention** on Linux and macOS alike.
macOS does **not** use `~/Library/Application Support`: both platforms use the same paths,
so a dotfiles repository or a shell profile written once works on both.

| What | Default | Relocated by |
|---|---|---|
| Config file | `~/.config/memcastle/config.toml` | `XDG_CONFIG_HOME`, or `--config` / `MEMCASTLE_CONFIG` |
| Palace (persistent data) | `~/.local/share/memcastle/default` | `XDG_DATA_HOME`, or `--palace` / `MEMCASTLE_PALACE_PATH` / `palace.path` |
| Daemon registry (runtime state) | `~/.local/state/memcastle/run/` | `XDG_STATE_HOME` |
| Package assets (not user data) | `<prefix>/share/memcastle`, or none | `--assets-dir` / `MEMCASTLE_ASSETS_DIR` / `assets.dir` |

The last row is not a place you keep anything.
It is where an OS package may install read-only files such as a future web UI, and it is never under the XDG directories.
See [Runtime assets](#runtime-assets).

The XDG variables are honoured as the specification describes:
an unset, empty or relative value is ignored, and the default under your home directory is used instead.
For example, with `XDG_DATA_HOME=/srv/data` the palace is `/srv/data/memcastle/default`.

The config file is optional.
If the default file does not exist, built-in defaults are used.
A file named explicitly with `--config` must exist, because a typo there would otherwise be silently ignored.

### Palace and data path selection

A palace is one directory.
With the embedded store, the SurrealDB files live in its `db/` subdirectory.
The path is chosen by, highest precedence first: `--palace`, `MEMCASTLE_PALACE_PATH`, `palace.path` in the config file,
then `$XDG_DATA_HOME/memcastle/default`.
It must be an absolute path; a relative one is rejected at startup, because a daemon and a client started
from different directories would disagree about which palace they mean.

Every command resolves the palace the same way, so `serve`, `status`, `migrate`, `daemon stop` and the rest agree on it.
A client finds a running daemon through the registry file, which is keyed by the palace path.
`daemon start` and `daemon restart` pass the palace they resolved on to the new daemon.

The registry is runtime metadata, not palace data, so it lives in the state directory rather than beside the palace.
Backing up or copying a palace never carries a stale daemon record with it.
Each palace has its own file, `$XDG_STATE_HOME/memcastle/run/<palace-hash>/daemon.json`,
where `<palace-hash>` is derived from the palace's canonical path.
If no home directory can be determined, the registry falls back to `memcastle/run` under the system temporary directory.
[Storage and data](storage.md#the-registry-file) describes the file.

## Precedence

From lowest to highest, a later layer overrides an earlier one:

1. Built-in defaults.
2. The config file.
3. `MEMCASTLE_*` environment variables.
4. Command-line flags.

A malformed environment variable is an error naming the variable; it is never silently ignored.
The resolved configuration is validated once all layers are applied.

Logging has one more input.
Its precedence, highest first, is `MEMCASTLE_LOG`, `RUST_LOG`, the `-v` / `-vv` flags, then `logging.level`.
`MEMCASTLE_LOG`, `RUST_LOG` and `logging.level` accept a level (`error`, `warn`, `info`, `debug`, `trace`)
or a filter directive such as `memcastle=debug,warn`.

## Config file

TOML.
Every key is optional, and a file may set only some of them.

```toml
[palace]
path = "/home/alice/.local/share/memcastle/default"

[server]
bind = "127.0.0.1"
port = 8420

[logging]
level = "info"
# "text" for a terminal, "json" (one object per line) for journald and log shippers.
format = "text"

# Optional bearer-token authentication. Prefer MEMCASTLE_AUTH_TOKEN to `token` here,
# so the secret stays out of the file.
[auth]
enabled = false

# Defaults for `memcastle db start`. Nothing here opens the endpoint; see "The database admin endpoint".
[db]
bind = "127.0.0.1"
port = 8000
allow_remote = false
allowed_origins = []

# Where embeddings come from. Without a provider search stays lexical; see "Embeddings".
[embeddings]
provider = "none"          # "none", "command" or "http"
# command = ["/usr/local/bin/embed"]
# url = "http://localhost:11434/v1"
# model = "nomic-embed-text"
timeout_secs = 30
batch_size = 16

# Where entities and relationships are extracted from mined text. Off by default; see "Extraction".
[extraction]
provider = "none"          # "none", "heuristic", "command" or "http"
# command = ["/usr/local/bin/extract"]
# url = "http://localhost:11434/v1"
# model = "llama3.1"
timeout_secs = 120
batch_size = 8
max_entities = 32          # kept from one drawer
max_relations = 64
min_confidence = 0.3       # relationships below this are dropped

# How eagerly duplicate memories and entity spellings are recognised; see "Deduplication".
[dedup]
enabled = true             # false stores every drawer and links nothing
near_threshold = 0.9       # similarity, 0.5 to 1, at which a drawer is linked as a likely copy
entity_fuzzy = true        # a unique one-character typo converges on the entity it resembles

# How mining cuts and bounds what it reads; see "Mining sources".
[mining]
chunk_chars = 6000          # characters per drawer; a longer document becomes several drawers
max_file_bytes = 2097152    # directory mining skips files larger than this
max_documents = 2000        # documents one job files; the next job continues from the cursor
# Installed WebAssembly sources (see "Writing a mining source"): where they live, and what each call may use.
# sources_dir = "/home/alice/.local/share/memcastle/sources"
source_memory_mib = 256     # the most memory one call into an installed source may use
source_timeout_secs = 60    # the longest one call may run; a source's own limits can only lower these

# Only to serve assets from somewhere other than the installed or embedded ones.
[assets]
dir = "/home/alice/src/memcastle-web/dist"

[jobs]
max_concurrency = 4
drain_timeout_secs = 10
lease_ttl_secs = 30

# Embedded SurrealKV under palace.path (the default), or a remote SurrealDB.
[store]
mode = "embedded"
# When an embedded palace forces its writes to disk: "every" (each commit, the default),
# "never" (leave it to the operating system) or an interval over 100ms such as "500ms", "5s" or "1m".
sync = "every"
```

A remote store also needs a URL and credentials:

```toml
[store]
mode = "remote"
url = "ws://localhost:8000"
namespace = "memcastle"
database = "main"
username = "root"
password = "..."
```

Remote store settings are file-only; there is no environment variable for them.
Keep secrets out of version control: put this file outside any repository, and restrict it with `chmod 600`.

## Environment variables

| Setting (TOML key) | Environment variable | Default |
|---|---|---|
| `palace.path` | `MEMCASTLE_PALACE_PATH` | `~/.local/share/memcastle/default` |
| `server.bind` (an IP address) | `MEMCASTLE_BIND` | `127.0.0.1` |
| `server.port` (0 to 65535) | `MEMCASTLE_PORT` | `8420` |
| `assets.dir` (an absolute path) | `MEMCASTLE_ASSETS_DIR` | none: installed, then embedded assets |
| `logging.level` | `MEMCASTLE_LOG` | `info` |
| `logging.format` | `MEMCASTLE_LOG_FORMAT` | `text` (`text` or `json`) |
| `jobs.max_concurrency` (at least 1) | `MEMCASTLE_JOBS_MAX_CONCURRENCY` | `4` |
| `jobs.drain_timeout_secs` (1 to 86400) | `MEMCASTLE_JOBS_DRAIN_TIMEOUT_SECS` | `10` |
| `jobs.lease_ttl_secs` (3 to 86400) | `MEMCASTLE_JOBS_LEASE_TTL_SECS` | `30` |
| `auth.enabled` (`true` or `false`) | `MEMCASTLE_AUTH_ENABLED` | `false` |
| `auth.token` (at least 16 characters) | `MEMCASTLE_AUTH_TOKEN` | none |
| `db.bind` (an IP address) | `MEMCASTLE_DB_BIND` | `127.0.0.1` |
| `db.port` (0 to 65535) | `MEMCASTLE_DB_PORT` | `8000` |
| `db.allow_remote` (`true` or `false`) | `MEMCASTLE_DB_ALLOW_REMOTE` | `false` |
| `db.allowed_origins` (a list) | `MEMCASTLE_DB_ALLOWED_ORIGINS` (comma-separated) | none |
| `embeddings.provider` (`none`, `command` or `http`) | `MEMCASTLE_EMBEDDINGS_PROVIDER` | `none` |
| `embeddings.url` (an `http://` or `https://` URL) | `MEMCASTLE_EMBEDDINGS_URL` | none |
| `embeddings.model` | `MEMCASTLE_EMBEDDINGS_MODEL` | none |
| `embeddings.api_key` | `MEMCASTLE_EMBEDDINGS_API_KEY` | none |
| `embeddings.timeout_secs` (1 to 3600) | `MEMCASTLE_EMBEDDINGS_TIMEOUT_SECS` | `30` |
| `embeddings.batch_size` (1 to 1024) | none | `16` |
| `embeddings.command` (a list) | none | none |
| `extraction.provider` (`none`, `heuristic`, `command` or `http`) | `MEMCASTLE_EXTRACTION_PROVIDER` | `none` |
| `extraction.url` (an `http://` or `https://` URL) | `MEMCASTLE_EXTRACTION_URL` | none |
| `extraction.model` | `MEMCASTLE_EXTRACTION_MODEL` | none |
| `extraction.api_key` | `MEMCASTLE_EXTRACTION_API_KEY` | none |
| `extraction.timeout_secs` (1 to 3600) | `MEMCASTLE_EXTRACTION_TIMEOUT_SECS` | `120` |
| `extraction.batch_size` (1 to 256) | `MEMCASTLE_EXTRACTION_BATCH_SIZE` | `8` |
| `extraction.min_confidence` (0 to 1) | `MEMCASTLE_EXTRACTION_MIN_CONFIDENCE` | `0.3` |
| `extraction.max_entities` (1 to 1000) | none | `32` |
| `extraction.max_relations` (1 to 10000) | none | `64` |
| `extraction.command` (a list) | none | none |
| `dedup.enabled` (`true` or `false`) | `MEMCASTLE_DEDUP_ENABLED` | `true` |
| `dedup.near_threshold` (0.5 to 1) | `MEMCASTLE_DEDUP_NEAR_THRESHOLD` | `0.9` |
| `dedup.entity_fuzzy` (`true` or `false`) | `MEMCASTLE_DEDUP_ENTITY_FUZZY` | `true` |
| `mining.chunk_chars` (200 to 100000) | `MEMCASTLE_MINING_CHUNK_CHARS` | `6000` |
| `mining.max_file_bytes` (1 to 67108864) | `MEMCASTLE_MINING_MAX_FILE_BYTES` | `2097152` |
| `mining.max_documents` (1 to 1000000) | `MEMCASTLE_MINING_MAX_DOCUMENTS` | `2000` |
| `mining.sources_dir` (an absolute path) | `MEMCASTLE_MINING_SOURCES_DIR` | `$XDG_DATA_HOME/memcastle/sources` |
| `mining.source_memory_mib` (16 to 4096) | `MEMCASTLE_MINING_SOURCE_MEMORY_MIB` | `256` |
| `mining.source_timeout_secs` (1 to 3600) | `MEMCASTLE_MINING_SOURCE_TIMEOUT_SECS` | `60` |
| `store.sync` (`every`, `never` or an interval over 100ms) | `MEMCASTLE_STORE_SYNC` | `every` |
| `store.mode` and remote settings | none | `embedded` |

`auth.token` is a secret, and is handled as one: MemCastle never logs it, prints it, serialises it, or writes it anywhere,
and the client commands read it from the environment or the config file, never from a flag.
Whitespace around `MEMCASTLE_AUTH_TOKEN` is ignored,
but a token in the config file with leading or trailing whitespace is refused at start,
because a client's token is trimmed too and the two could never match.
See [Authentication](authentication.md).

`embeddings.api_key` and `extraction.api_key` are secrets too: they are never logged, printed or serialised,
and an invalid value is never echoed.

Some variables are read by the command line rather than the config file:

| Variable | Equivalent flag |
|---|---|
| `MEMCASTLE_CONFIG` | `--config` |
| `MEMCASTLE_MODE` | `--mode` |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` | none |
| `RUST_LOG` | none |

## Command-line flags

| Flag | Applies to | Overrides |
|---|---|---|
| `--config <FILE>` | every command | the default config file location |
| `--palace <PATH>` | every command | `palace.path`, `MEMCASTLE_PALACE_PATH` |
| `--bind <IP>` | `serve`, `daemon start`, `daemon restart` | `server.bind`, `MEMCASTLE_BIND` |
| `--port <PORT>` | `serve`, `daemon start`, `daemon restart` | `server.port`, `MEMCASTLE_PORT` |
| `--assets-dir <DIR>` | `serve`, `daemon start`, `daemon restart` | `assets.dir`, `MEMCASTLE_ASSETS_DIR` |
| `--mode <MODE>` | every command (acted on by client commands) | the memory mode of the session |
| `-v`, `-vv` | every command | the log level of memcastle itself |

## The listener: address and port

The daemon serves the REST API and MCP on one TCP listener, `127.0.0.1` port `8420` unless configured otherwise:

```sh
memcastle serve --bind 127.0.0.1 --port 8787
```

The address and the port are separate settings, so either can be changed alone.
Each is chosen by, highest precedence first: the flag, the environment variable, the config file, the default.
`serve`, `daemon start`, `daemon restart` and a supervisor such as systemd all start the daemon through the same path,
so the same three sources work everywhere.
Client commands (`status`, `search`, `job` and the rest) read the config file and the environment,
but not the flags, so a daemon started on a non-default port with `--port` is found through its registry file.
`daemon start` and `daemon restart` pass their `--bind`, `--port` and `--assets-dir` on to the new daemon.

`memcastle status` shows which of the two it used (`endpoint_source`: `registry` or `config`).
It exits 0 for a healthy daemon, 1 for a degraded one and 3 when none is running, and takes `--json` for scripts:

```sh
memcastle status                      # human-readable report
memcastle status --json | jq .daemon.datastore
memcastle status || echo "exit $?"    # 3 means not running
```

`server.bind` is an IP address, IPv4 or IPv6, and not a host name.
A `host:port` value, as `bind` accepted before the port became its own setting, is refused with a message naming the port
setting to use instead.
Port `0` asks the OS for a free port, which is useful for tests and scripts;
the port actually chosen is in the daemon's registry file and its log.

The default never listens on all network interfaces.
Authentication is off unless you enable it, so use a non-loopback address such as `0.0.0.0` only on a network you trust,
or with [authentication](authentication.md) enabled.
The daemon logs a warning when it listens beyond loopback without it.
The MCP endpoint additionally refuses a non-loopback `Host` header, see [Authentication](authentication.md#exposing-the-daemon).

The listener is bound before anything that changes state: the runtime assets are only located, not yet used.
If the address is taken, needs privileges, or does not exist on this machine,
the start fails with `memcastle::server::bind_failed`, naming the address and what to change,
and has not created the palace, migrated it or touched its jobs.

## Embeddings

Semantic and hybrid search rank by vectors, and a vector has to come from a model.
MemCastle links no model and does not want your API keys, so the `[embeddings]` section only says how to reach one.
Without it the palace works exactly as before and `search` is lexical.
A vector is *derived* data: it is stored beside the drawer, never replaces its content, and can be recomputed at any time.

Every stored vector has **768** numbers, because the vector index's dimension is part of the palace's schema.
Pick a model that produces 768, or one that can be asked for 768 (OpenAI's `text-embedding-3-*` models accept a
`dimensions` setting, which MemCastle sends).
Anything else is refused with `memcastle::embed::dimension_mismatch`.

### The `command` provider

```toml
[embeddings]
provider = "command"
command = ["/usr/local/bin/embed", "--quiet"]
model = "my-model"      # passed to the program, optional
```

MemCastle runs the program, without a shell, once per batch, writes one JSON object to its standard input
and reads one from its standard output:

```json
{"model": "my-model", "dimension": 768, "texts": ["first text", "second text"]}
{"embeddings": [[0.01, 0.02, "… 768 numbers"], [0.03, 0.04, "…"]]}
```

The program returns one vector per text, in order, and exits `0`.
A non-zero exit, output that is not that JSON, or no answer within `timeout_secs` fails the call, and the program is killed.
This is the provider to use when the model needs a credential or a client library: the program owns them, MemCastle never
sees them, and the daemon's own `MEMCASTLE_*` variables (its auth token included) are removed from the program's environment.
Everything else is inherited, so `PATH` and the program's own variables work.

### The `http` provider

```toml
[embeddings]
provider = "http"
url = "http://localhost:11434/v1"
model = "nomic-embed-text"
```

MemCastle posts to `{url}/embeddings` in the OpenAI format, so OpenAI, Ollama, llama.cpp's server and vLLM all work.
A local server needs no key.
A hosted one takes `MEMCASTLE_EMBEDDINGS_API_KEY`, sent as a bearer token; prefer that to `api_key` in the file.

### How embedding happens

- The daemon embeds in the background.
  After anything that writes drawers (mining, a checkpoint, a diary entry, a created drawer) and once at startup,
  it queues a low-priority `embed` job that embeds every drawer without a vector.
  Queued sweeps are coalesced, so a burst of writes leaves one waiting job.
  `memcastle embed` queues one by hand, for example after you first configure a provider.
- A drawer is embedded from its first 8,000 characters, because providers cap their input.
  Its content is stored whole and verbatim whatever the provider sees.
- A query is embedded on the fly.
  If the provider fails, a default `auto` search falls back to lexical and logs a warning,
  while an explicit `semantic` or `hybrid` search fails with `memcastle::embed::failed`.
- You can skip the provider altogether and send vectors yourself, see
  [Searching](mcp-and-api.md#searching).

## Extraction

The knowledge graph links entities (people, projects, tools, ...) to each other and to the drawers that mention them,
and graph-aware search follows those links.
The `[extraction]` section says how MemCastle finds them in mined text.
Nothing is extracted by default, because two of the providers send mined text somewhere else:
choose one on purpose.
Extraction is *derived* data: it adds entities, links and relationships beside the drawers and never rewrites,
supersedes or deletes one.
See [ADR-024](adr/024-entity-extraction-as-an-enrich-job.md).

| `provider` | What reads the text | Leaves the process? |
|---|---|---|
| `none` | nothing | no |
| `heuristic` | a small built-in extractor | no |
| `command` | a program you run | whatever it does |
| `http` | an OpenAI-compatible chat endpoint | yes, to that endpoint |

### The `heuristic` extractor

```toml
[extraction]
provider = "heuristic"
```

Deterministic, with nothing to configure and no model.
It finds runs of capitalised words (`Ada Lovelace`), `@handles` and `` `code spans` ``, and relates two of them when the
words between them in one sentence are a known phrase (`works on`, `is a member of`, `depends on`, `uses`, `owns`,
`is part of`, `is based in`, ...).
It never guesses: two names that merely share a sentence are not related, and a sentence it does not recognise yields
entities and no relationship.
For more than that, use a model.

### The `command` extractor

```toml
[extraction]
provider = "command"
command = ["/usr/local/bin/extract"]
model = "my-model"      # passed to the program, optional
```

MemCastle runs the program, without a shell, once per batch, writes one JSON object to its standard input
and reads one from its standard output:

```json
{"model": "my-model",
 "vocabulary": {"kinds": ["person", "…"], "predicates": ["works_on", "…"]},
 "texts": ["first text", "second text"]}
{"extractions": [
  {"entities": [{"name": "Ada", "kind": "person"}, {"name": "MemCastle", "kind": "project"}],
   "relations": [{"subject": "Ada", "predicate": "works_on", "object": "MemCastle", "confidence": 0.9}]},
  {"entities": [], "relations": []}]}
```

One extraction per text, in order, and exit `0`.
A non-zero exit, output that is not that JSON, or no answer within `timeout_secs` fails the call, and the program is killed.
The program owns the model and any credential, and the daemon's own `MEMCASTLE_*` variables are removed from its environment.

### The `http` extractor

```toml
[extraction]
provider = "http"
url = "http://localhost:11434/v1"
model = "llama3.1"
```

MemCastle posts to `{url}/chat/completions` in the OpenAI format, one request per drawer, telling the model the
vocabulary and asking for JSON.
A hosted endpoint takes `MEMCASTLE_EXTRACTION_API_KEY`, sent as a bearer token.
An endpoint that cannot be reached or answers with an error fails the job.
A reply that is not the JSON asked for is read as "nothing found" and logged, so one odd answer cannot block the drawers
behind it.

### What is kept

Whatever a provider says is read into a closed vocabulary, so the graph does not fill with synonyms.
Entity kinds are `person`, `organization`, `project`, `tool`, `place`, `concept` and `other`;
predicates are `works_on`, `member_of`, `depends_on`, `uses`, `owns`, `part_of`, `located_in` and `related_to`.
Anything else becomes `other` or `related_to`.
A relationship needs both ends among the entities of the same answer, and is dropped when it points at itself,
has no sensible confidence or falls below `min_confidence`.
`max_entities` and `max_relations` bound what one drawer contributes.

### How extraction happens

- The daemon extracts in the background.
  After a mining job completes, and once at startup, it queues a low-priority `extract` job,
  coalesced like the embedding sweep.
  `memcastle extract` queues one by hand.
- Only mined drawers are read (they carry a source origin), and only while they are current.
  A drawer is read once: changing the provider later does not re-read what was already read.
- A drawer's first 8,000 characters are sent, whatever it holds.
- Every fact records the drawer, source document, job and extractor it came from, and holds from the document's own
  date when the source has one.
  When a re-mine replaces a drawer, the facts it supported stop being current and the new drawer is read.

## Deduplication

The `[dedup]` section controls how eagerly MemCastle recognises the same memory, or the same entity, written twice.
It never merges anything: the most it does is not store an exact copy, link a likely copy to what it resembles,
and let an entity spelled differently converge on the one it is a variant of.
The rules, and what each writer gets back, are on the [Deduplication](deduplication.md) page.

- `enabled = false` stores every drawer and records no link.
  Entity names still converge when they differ only in case, punctuation or spacing, which is spelling and not a
  judgement.
- `near_threshold` is the similarity at which a drawer is linked as a `near` duplicate of another in its room.
  Raise it towards `1` to link only the closest copies; below `0.5` it would link unrelated drawers, so it is refused.
  It never makes MemCastle skip a write: only an exact copy does.
- `entity_fuzzy = false` leaves casing, punctuation and alias convergence on,
  and turns off the one-character typo rule and the `possibly_same_as` candidates.

## The database admin endpoint

`memcastle db start` opens a second, SurrealDB-compatible listener inside the running daemon, so SurrealDB Studio can
inspect the live embedded database.
The `[db]` settings are only the defaults for that command; no setting starts the endpoint.
Each flag of `memcastle db start` (`--bind`, `--port`, `--allow-remote`, `--allow-origin`) overrides its setting,
and `--allow-origin` adds to `db.allowed_origins`.

The address is `127.0.0.1` port `8000` unless configured otherwise, and the port must differ from `server.port`.
A `db.bind` that is not a loopback address is refused when the configuration loads unless `db.allow_remote` is true,
and the daemon refuses to open it at all unless `auth.enabled` is also true.
`db.allowed_origins` lists web page origins, such as `https://app.surrealdb.com`, that may connect from a browser,
in addition to pages served from this machine and the SurrealDB Studio desktop app, which are always allowed.
The values are compared whole, never as patterns.

See [Database access](database-access.md) for the workflow and the security model.

## Runtime assets

Some files a release carries are not your configuration or your data.
They are read-only, come with the version of MemCastle you installed, and are found by one fixed rule, tried in order:

1. **An explicit directory**, from `--assets-dir`, `MEMCASTLE_ASSETS_DIR` or `assets.dir`, by the usual precedence.
   It must be an absolute path to an existing directory.
   If it does not exist the daemon refuses to start with `memcastle::assets::not_found`;
   it never quietly uses another source in its place.
2. **The installed directory**, which an OS package creates: `share/memcastle` next to the executable's `bin/`
   (`/usr/share/memcastle` for `/usr/bin/memcastle`), then `/usr/local/share/memcastle`, then `/usr/share/memcastle`.
   The first that exists is used.
   A candidate inside your XDG data directory is ignored, because that is where your palace lives.
3. **The assets built into the binary.**
   This is what a standalone download uses, and it needs no directory and no network.

The daemon logs which source it resolved at startup (`runtime assets resolved`).
The schema and the data migrations are always built into the binary and are not affected by any of this.
[Installation](installation.md#standalone-binary-or-native-package) describes the package layout.

Nothing in 0.1 installs files into the assets directory, so you only need the setting when developing a web UI locally.

## Not supported: other platforms' conventions

Windows is built and tested, but has no dedicated layout: the same dot-directories are used under your home directory.
Any platform-specific behaviour would be an explicit design decision, recorded in an ADR;
see [ADR-010](adr/010-unix-xdg-paths.md).

## See also

- [Running the daemon](daemon.md) for starting, stopping and supervising it.
- [Storage and data](storage.md) for what lives in the palace and the registry.
- [CLI reference](cli.md) for every flag.
- [Troubleshooting](troubleshooting.md) for configuration errors.
