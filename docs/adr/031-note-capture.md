# ADR-031: A note is an unnamed drawer written through its own service, and the CLI resolves its project scope

## Status

Accepted.
Amends [ADR-029](029-project-local-configuration.md): the CLI now reads the project file, for `note` only,
and [ADR-024](024-entity-extraction-as-an-enrich-job.md): extraction also reads notes.

## Context

Issue #187 asks for a lightweight way to capture a thought from a shell: `memcastle note "..."`, with no folder, tag or
graph to maintain, filed under the project the user is in, kept as typed, and treated like any other memory afterwards.

Three existing paths write a drawer, and none fits.
A checkpoint is a durable job and answers with a job, not a drawer, so the caller cannot be given the stable id it
should keep.
Mining is built around documents that are discovered, revised and tracked by a cursor, and a one-line thought is none of
those.
A diary entry is scoped to an agent identity, which a person at a prompt does not have.

One fact about enrichment shapes the design.
The extraction sweep reads only drawers that carry a source origin ([ADR-024](024-entity-extraction-as-an-enrich-job.md)),
and a note has no document to cut from, so a note written through any existing path would be searchable and never read
for entities.

## Decision

- **A note is an unnamed drawer** in an ordinary room, with `source.kind = note`, the capture directory as `source.uri`,
  and `provenance.requested_by` naming the channel.
  There is no note table, no vault and no second index: search, recall, embedding, deduplication, supersession and
  point-in-time queries all apply as they do to any drawer.
- **`AppServices::note_write` and `POST /api/notes` write it synchronously**, like a diary entry, so the answer carries
  the drawer's id.
  The text is stored exactly as given, the capture time is the drawer's `created_at` and `valid_from`, and an exact copy
  in the same room is not stored twice
  ([ADR-025](025-memory-deduplication-and-entity-resolution.md), the `MEMORY` rules):
  the existing note is returned with `created = false`.
- **Enrichment is queued after the write**: the embedding sweep, as for every writer, and an extraction sweep.
  The extraction sweep's selection widens from "has a source origin" to "has a source origin, or is a note".
  Extraction stays derived data that never writes a drawer.
- **`SourceKind::Note` is a new kind** (`note` on the wire, accepted by `--source-kind` and `source_kind`),
  so notes can be asked for or excluded without a tag the user would have to type.
- **The CLI resolves the project scope itself**, per field: `--wing` / `--room`, then `MEMCASTLE_WING` /
  `MEMCASTLE_ROOM`, then `.config/memcastle.toml`, then the working directory's name for the wing and `notes` for the room.
  It sends the daemon an ordinary wing and room, so the daemon still never learns what a project is.
  The reader moves from the directory adapter to `src/project.rs`, which both call, so mining and noting cannot disagree
  about discovery, and the shared fixtures now replay the environment cases in Rust too.
- **A project that cannot be read is an error for `note`**, `memcastle::project::invalid`,
  unlike mining's log and carry on:
  the user asked for this note to be filed, a wrong scope would put it somewhere unintended, and `--wing` with `--room`
  is the way out.
- **Text comes from arguments, `--file` (`-` for standard input), piped standard input or `$VISUAL` / `$EDITOR`.**
  The editor runs on a scratch file that is removed afterwards, a failing editor saves nothing, and text from a file,
  standard input or an editor loses its trailing whitespace.
- **There is no MCP tool.**
  An agent writes memory through `memcastle_checkpoint` and the diary, which carry the agent identity and the
  classification a note deliberately has none of.
  A tool can be added over the same service later without changing it.

## Alternatives rejected

- **A checkpoint job with a `general` item.**
  Durable and already there, but it returns a job id, derives the drawer id where the caller cannot see it, and still has
  no origin for extraction.
- **A built-in `note` mining adapter.**
  It would give a note an origin and a document record, but the pipeline's discovery, cursor and revision machinery
  exists to track documents that change, and a note is written once.
  It would also make the capture asynchronous.
- **Giving every note a synthetic origin so the extraction query stays unchanged.**
  It would invent a source and a document that do not exist, and pretend provenance the note does not have.
- **The daemon resolving the project from a path.**
  It only works when the daemon and the CLI share a filesystem, and it would teach the daemon a notion of project that
  [ADR-029](029-project-local-configuration.md) deliberately keeps out of it.
- **A `note.wing` setting in the daemon's configuration.**
  The project file already says where a project's memory goes, and a machine-wide setting would override the project
  the user is standing in.

## Consequences

- `memcastle note` works from any directory with no flags, and files under the project's wing when the project says so,
  so a note, a mined file and a checkpoint of one project meet in one wing.
- A note is searchable at once and, with an embedding provider, semantically after the next sweep.
  With an extraction provider its entities appear in the graph with the note as provenance, valid from the capture time.
- Extraction now reads two kinds of drawer, and a test holds the selection to exactly those.
- `tempfile` is a normal dependency, for the editor's scratch file.
- The CLI reads a directory and the environment for one command.
  It still touches neither the store nor the jobs, which the `store-isolation` hook keeps true.
- The TypeScript readers are unchanged: the contract and the fixtures are the same.
