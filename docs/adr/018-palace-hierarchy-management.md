# ADR-018: The palace hierarchy is managed through REST and the CLI, with cascading deletes, drawer names and no MCP tool

## Status

Accepted, extends [ADR-007](007-memory-mode-gate-follows-data-access.md)
(wing, room and drawer management follows the same gate) and [ADR-016](016-cli-presentation-follows-the-output-stream.md)
(the new `list` and `show` commands and the new confirmations follow its rules)

## Context

Wings, rooms and drawers were created as a side effect of mining, checkpoints and diary writes,
and there was no way to look at them or remove one.
The `wings`, `rooms` and `drawers` commands were reserved names that returned `not_implemented`.
The only deletion in the store was `delete_drawer`, used by `repair` for orphans.

Three properties of the existing design shape the answer.
The foreign keys (`room.wing`, `drawer.room`) are plain strings with no `ON DELETE`, so nothing cascades by itself.
A drawer has no name, only a UUID, which makes `work/project-x/context` unaddressable.
And mining, checkpoint and diary files into wings and rooms by name, creating them when absent.

## Decision

- **Addressing is a path of names or UUIDs.**
  `wing`, `wing/room` and `wing/room/drawer`.
  A wing or room is looked up by name first, then by UUID, and a room UUID only resolves inside its own wing.
  Names cannot look like a UUID, so the two never collide, and a wing or room name cannot contain `/`.
  A drawer name may contain `/`: everything after the second `/` is the drawer, and the REST drawer segment is a wildcard.
- **A drawer may have a name,** unique within its room, held in an optional `drawer.name`
  with a unique index on `(room, name)`.
  Unnamed drawers do not collide on the index.
  Four writers set it: `drawer create`, a checkpoint item's `name`, and mining, which names a file's drawer after its path.
  A re-mine finds the name taken by the first copy and leaves the new one unnamed, because failing a re-mine
  would turn an existing duplication into an error.
  A checkpoint item that asks for a taken name fails, because the caller named it on purpose.
- **Create is idempotent.**
  Creating a wing or room that exists returns it with `created: false`.
  `drawer create` is a no-op for the same name and content, and `drawer_name_taken` for other content,
  because content is immutable.
  A missing parent is created on the way (`room create`, `drawer create`), as every other writer does.
- **Delete cascades, in one transaction.**
  A wing delete removes its drawers, then its rooms, then the wing, atomically,
  so the hierarchy is never observed (or left, by a crash) with children whose parent is gone.
  The counts reported are read just before the delete and are informational.
  Counts shown for a wing or room are computed when asked, never stored.
- **A wing or room delete is refused while a palace-writing job is pending.**
  A mining job, a checkpoint job or an applying repair that is queued, running or paused makes it fail with
  `memcastle::palace::busy` (HTTP 409).
  The rule is coarse because a checkpoint names wings per item, so the target of a job is not known up front,
  and it is not atomic with the delete.
  A dry-run repair, an audit and a demo job only report and do not block.
- **The gate follows ADR-007.**
  Listing and showing are reads, because names and counts are palace content.
  Creating and deleting are writes.
- **REST and CLI only, no MCP tool.**
  The routes sit under `/api/wings` and are guarded by the authentication layer like every other route.
  An agent can already file memories; deleting a wing is a human decision, as minting a token and opening the database
  console are (ADR-014, ADR-015).
  `memcastle_checkpoint` gains the optional item `name` because it is an existing write path.
- **The CLI confirms deletes the way ADR-016 describes.**
  In a terminal it prints what will be removed and asks, defaulting to "no", and `--yes` skips it.
  Without a terminal it proceeds.

## Alternatives rejected

- **Addressing drawers by UUID only.**
  No schema change, but `work/project-x/context` in the interface would be a UUID, and a drawer one has just
  written cannot be found again without listing.
- **A UUID prefix, as git does for commits.**
  Needs ambiguity handling and still gives no stable, human-chosen handle.
- **A mandatory drawer name.**
  Diary entries, checkpoint items and every mined re-run would have to invent one.
- **A strict `create` that fails when the name exists.**
  Makes the command unsafe to repeat in a script, and disagrees with every automatic writer, which is get-or-create.
- **Stored counts on wings and rooms.**
  Each of mining, checkpoint, diary and repair would have to maintain them, and the first to forget would make them
  lie.
- **Deleting through a job.**
  The scheduler exists for work that is long, resumable or pausable.
  A transactional delete is none of these, and the person running it wants the answer now.
- **Refusing a delete only when a job targets that wing.**
  A checkpoint's wings are per item, and a mine's wing defaults from the directory name, so "targets" is not known
  without parsing every payload and still races.
- **MCP tools for delete, behind the memory mode.**
  `read_only` and `disabled` stop an agent from deleting, but `full` is the default and what most agents run in.
- **Failing a re-mine on a taken name.**
  It would make a command that works today fail on its second run.

## Consequences

- A palace gains a visible, scriptable structure, and `audit` and `repair` keep their job for damage this
  transaction cannot cause.
- `drawer` has a new optional field and index, applied by the ordinary schema sync, with nothing to backfill.
- The busy rule can refuse a delete that would have been safe (a mine of an unrelated wing),
  and cannot catch a job submitted between the check and the delete.
  Both are documented; neither loses data silently.
- A wing created before the name rules may contain `/` and is reachable only by UUID.
- Re-mined files beyond the first copy have no name.
- An agent cannot manage the hierarchy, and an operator who wants it to must use the REST API directly.
