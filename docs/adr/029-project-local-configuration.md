# ADR-029: A project declares its memory scope in a local file that clients read, and the daemon reads only for mining

## Status

Accepted, amended by [ADR-031](031-note-capture.md) (the CLI now reads the project file, for `note` only:
the statements below that the CLI does not read it describe the original decision).
Applies [ADR-019](019-shared-integration-contract.md) (client lifecycle logic does not move into MemCastle) and
[ADR-022](022-integrations-are-bun-packages-tested-against-a-real-daemon.md) (each integration carries its own code).

## Context

Nothing tied a repository to a part of the palace.
Pi and OpenCode each guessed a wing from the working directory's name, checkpoints went to shared wings,
and a project that wanted its memory elsewhere had to be told so in every integration's own settings.
Each new integration would have invented its own convention.

A project needs one place to say where its memory belongs, and the answer has to be the same for every client.
It also has to stay apart from the daemon's configuration, which is about a machine and not about a repository.

Two facts shape the design.
One daemon serves one palace, chosen by its path, so a palace is not something a request or a project can choose.
And the daemon never sees a client's working directory, so it cannot resolve a project for anyone.

## Decision

- **A project declares its scope in `.config/memcastle.toml`**, with `[project] name`, `[memcastle] wing` and `room`,
  and a reserved empty `[mining]` table.
  The schema is deliberately small and closed: unknown keys are errors, which also means there is no key a secret could
  go in.
- **There is no `palace` key.**
  A palace is one daemon, and which daemon a client talks to is the user's decision about their machine.
  `MEMCASTLE_PALACE_PATH` and `--palace` stay the only palace selectors, and the file cannot override them.
- **`MEMCASTLE_WING` and `MEMCASTLE_ROOM` override the file**, for CI, wrappers and temporary overrides.
  Per field the order is environment, then file, then the integration's own setting, then no scope.
  A blank variable is unset, and an invalid one is an error that names it.
- **Discovery is nearest-wins and bounded.**
  The walk takes the nearest file, inherits nothing from a parent project, stops at a git root and at `$HOME`,
  and never reads `$HOME/.config/memcastle.toml`.
  A project with no file is a project with no scope, never the scope of an unrelated parent.
- **Integrations read the file and the environment themselves** and pass the result to MemCastle as ordinary `wing` and
  `room` arguments.
  No endpoint, command or MCP tool resolves a project, and the daemon learns nothing about it from a client.
- **The daemon reads the file in exactly one place:** the directory adapter's default wing, because mining is the one
  operation where the daemon itself reads a directory and has to choose a wing.
  A file it cannot use is logged and the directory's name is used, as before.
- **Two implementations, one set of cases.**
  The Rust reader and the TypeScript reader (shared by Pi and OpenCode as identical copies) replay
  `tests/fixtures/project-config/cases.json`, so they cannot disagree about discovery, precedence or what is refused.
- **The project is not an authorization boundary.**
  The daemon's own checks stay authoritative, and the file holds no credentials.
- **A room scopes search only.**
  Recall ignores a room, and checkpoint, diary and mining rooms are fixed by their operation, so a project room would
  otherwise promise more than it does.

## Alternatives rejected

- **A `memcastle project show --json` command that integrations shell out to.**
  One implementation, but every integration would depend on the binary being on its `PATH` and on a process spawn per
  session, and the CLI would grow a command whose only caller is another client.
  The reader is small enough to carry twice, with the fixtures keeping the copies honest.
- **A REST endpoint resolving a project from a path.**
  It only works when the daemon and the client share a filesystem, which a remote daemon does not.
  It would also teach the daemon a notion of project that it does not otherwise have.
- **A `palace` key naming a palace or a path.**
  A name has nothing to resolve to, because the daemon holds one palace.
  A path would let a repository choose which database an agent writes to.
- **The CLI defaulting its wing from the project file.**
  It would put the same logic in a third place, and the CLI takes explicit flags today.
  It can be added later through the same fixtures.
- **Project mining settings now.**
  The `[mining]` table is reserved, but what a project should be able to configure there is not known yet.
  Accepting it empty means it can grow without breaking a file that already has it.

## Consequences

- A repository that commits `.config/memcastle.toml` gets the same wing for wake-up, checkpoints, searches and directory
  mining, in every integration, without any setting in the integration.
- The reader exists twice.
  Pi and OpenCode keep identical copies, held by an identity test, and the Rust reader is held to them by the fixtures.
  A change to the contract is a change to the fixtures first.
- The TypeScript packages gain one dependency, a TOML parser, because Pi runs on Node and has no built-in one.
- Resolution happens once per working directory, so a project file edited mid-session takes effect in the next session.
- `off` sessions read no project file, so a broken file cannot make an `off` session do or say anything.
- The CLI and the MCP tools are unchanged: a client that wants a project scope passes the wing and room itself.

## Note, 2026-10-05: an explicit wake-up source outranks the file

The order above is per field for the project context: environment, then file.
An integration's own `wakeUp.source` is not a later fallback to that context.
Only the default `project` source follows the project context;
`user`, `custom` and `none` are the user's explicit choices for the client and outrank both the environment and the file.
The integration contract and [Project configuration](../project-config.md#precedence) say so,
and the integrations behave that way.
