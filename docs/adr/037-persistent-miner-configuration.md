# ADR-037: Miners are `[[miners]]` in the configuration file, edited in place by the daemon, read-only over MCP

## Status

Accepted.
Builds on [ADR-023](023-unified-source-model-for-mining.md) (a source's cursor, which a miner points at and never owns)
and [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) (administrative operations are REST and CLI
only, so an agent cannot widen its own reach).
Builds on [ADR-010](010-unix-xdg-paths.md) (where the configuration file lives), which the daemon now writes as well as
reads, and only in the one section this ADR names.

## Context

Issue #208 asks for miner configuration to be a first-class, persistent capability: a named definition of what to mine
(a source, where in it, a scope, a trigger, a reference to its credentials) that survives a restart,
can be listed and changed from the CLI without editing TOML, and can be read and configured by an agent from a prompt.
Source-specific miners (Signal, Slack) will be built on it, and the trigger architecture (#189) will act on it.

Before this, mining was only ever a job submitted by hand: `memcastle mine` and `memcastle_mine` name a directory or a
source and a locator, and nothing is remembered about *what the user wants mined*.
The daemon's configuration was read once at startup and never written, and nothing in it could change while it ran.

Six decisions follow, and each has an alternative that is easier for one of the forces and worse for the others.

- Where definitions live.
- What an agent over MCP may do with them.
- What a definition is the identity of.
- What "reload" means when nothing else in the configuration reloads.
- How a definition refers to a secret.
- What a miner does today, when triggers and scope-aware sources do not exist yet.

## Decision

- **Definitions are `[[miners]]` in `memcastle.toml`, and the file is the one source of truth.**
  It is the declarative form the issue asks for, it can be written by hand, kept in version control and read by a human,
  and it needs no second store to fall out of sync with it.
  `miner set`, `enable`, `disable` and `remove` rewrite only that section, in place, with `toml_edit`:
  comments, the other tables and every entry that did not change are kept, the file is replaced atomically with its
  permissions, and nothing resolved from the environment or the command line is ever written into it.
  The daemon is the only writer; it refuses to write over a change it has not read.
- **Reading is offered to MCP; changing is not.**
  `memcastle_miner_list` and `memcastle_miner_get` let an agent say what is configured, under the memory mode like any read.
  Adding, changing, enabling, disabling, removing, reloading and running a miner are REST and CLI only,
  exactly as installing a source is (ADR-026), because what the daemon mines, from where and with whose credentials is the
  user's decision, and an agent that could configure it could widen its own reach.
  This deliberately goes less far than the issue's "configure Slack mining from a prompt":
  the agent can tell the user the command to run, and `memcastle_mine` remains for a one-off job.
  `tests/in_process/auth.rs` fails if any tool named for changing a miner appears.
- **A miner's identity is its name; the data's identity is its source and locator.**
  The cursor and documents belong to the source (`SourceRef`), which the adapter derives from the miner's `source` and
  `locator`.
  A miner is only configuration pointing at one, so renaming, re-scoping, disabling or changing its trigger, wing or
  credential cannot lose a cursor, and removing a miner never deletes what it mined.
  Changing `source` or `locator` points at a different source, and the change reports `identity_changed`.
- **Reload is by stamp and explicit, and an invalid file never takes the miners away.**
  The daemon reads the file again when its modification time or length changed, at the next miner request,
  and `miner reload` forces it and reports `added`, `removed`, `changed`, `enabled`, `disabled` and `broadened`.
  A file that does not parse or validate keeps the last good miners in force, reports its error beside them, and blocks
  writes until it is fixed, because writing would overwrite what the user is repairing.
  At startup an invalid file stops the daemon, like the rest of the configuration.
  Nothing else in the configuration reloads: this is not a general hot-reload mechanism.
- **A scope never widens silently.**
  A change through the daemon that removes a filter, adds a value to a list or changes a value is refused
  (`memcastle::miner::scope_broadened`) unless the caller says it is meant (`--allow-broaden`, `allow_broaden`).
  A hand edit is the user's and is applied, but it is logged at `warn` and listed under `broadened` by a reload.
- **A secret is a reference.**
  `credential` is the `CredentialRef` already used by sources (`env` or `file`), so no plaintext secret has a place in the
  file; a key in `scope`, `config` or `trigger` that reads like a secret is refused.
  Responses give a credential's kind and whether it resolves, never the variable's name or the file's path.
  The daemon checks that an enabled miner's credential resolves; handing it to a source needs a contract input that does
  not exist yet.
- **A miner is configuration plus a manual run today.**
  `miner run` submits an ordinary mine job for the miner's source and locator, so it continues from the cursor.
  `event` and `schedule` triggers are stored, validated and reported as not acted on: acting on them is #189.
  A scope and a `config` table are stored and reported, but no adapter applies them (an adapter receives only a
  locator), so `miner run` refuses a miner that has either, and `directory` refuses to be enabled with them:
  the daemon never mines more than a filter says.
  Validation is split the same way: the shape is checked when the file is loaded, and what running needs
  (the source is usable, the credential resolves, a directory's locator is absolute) is checked when an *enabled*
  miner is created or changed, so a disabled miner can be written ahead of the install of its source.

## Alternatives rejected

- **A database table, with the file only seeding it.**
  Runtime mutation is easier and there is already a precedent in installed source packages.
  But a miner is what the user declared, not state the daemon accumulated, and two homes would disagree after every hand
  edit.
  The issue also asks for the TOML file as the declarative form.
- **A database overlay merged with the file.**
  It gives hand-written and CLI-created miners separate homes, at the price of two sources of truth, a merge order to
  document, and a `list` whose answer depends on where an entry came from.
- **Serialising the resolved `Config` back to the file.**
  It would write the environment and command-line overrides into it, and lose every comment.
- **Writable MCP tools, mode-gated or with new miners created disabled.**
  Either still lets an agent decide what the daemon reads; "disabled until confirmed" moves the decision to a step the
  agent can describe but a prompt injection can also ask for.
  The cost of refusing is one command typed by the user.
- **Extending the adapter contract (and the WIT) with a scope and a config input now.**
  It is the right next step for scope-aware sources, but it changes the host, every built-in adapter and the component
  contract, and belongs with the first source that needs it.
- **Giving a miner its own cursor, keyed by name.**
  It would make `rename` lose progress and let two miners on one source re-read what the other had read.
- **Reloading the whole configuration on a signal or a file watcher.**
  Most settings (the listener, the store, the scheduler) cannot change under a running daemon;
  pretending they can is worse than saying only miners do.

## Consequences

- The daemon writes a file it owns only in part: a hand edit and a `miner set` in the same instant is possible, and the
  stamp check turns it into an error to retry, not a lost edit.
  Editing the section in place keeps the user's file recognisably theirs.
- The CLI, REST and the read-only MCP tools share one model, `app::miners`, as the issue requires,
  and `tests/in_process/miners.rs` checks that they agree.
- A configuration-file path becomes part of a running daemon (`Config::config_file`);
  a daemon built in code without one lists miners and refuses to change them.
- Credentials are checked but not yet used, scope and `config` are stored but not yet applied, and triggers are stored
  but not yet acted on.
  Each is reported as such rather than hidden, and each is the work of the issue that needs it.
- Disabling or removing a miner does not stop a job already queued or running: a job carries a source and a locator, not
  a miner's name.
- A name, `reload`, is reserved, because `POST /api/miners/reload` is a fixed route.

## Note, 2026-10-06: an OAuth credential

`credential` gains a third reference, `{ type = "oauth" }`, for a source that signs in with OAuth
([ADR-039](039-oauth-credentials-for-mining-sources.md)).
It names nothing, so the file still holds no secret, and it is shown as `oauth` with whether the source is signed in.
A miner for a source that declares a sign-in is unavailable until the source is signed in, whatever its `credential` says.
An `env` or `file` credential is still only checked and not yet handed to a source.
