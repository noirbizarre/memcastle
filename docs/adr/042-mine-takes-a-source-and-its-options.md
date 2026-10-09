# ADR-042: `mine` takes a source and its options, and the source contract carries them

## Status

Accepted.
Amends [ADR-023](023-unified-source-model-for-mining.md) (the CLI's `--source` and `--locator`),
[ADR-026](026-pluggable-source-adapters-as-webassembly-components.md) (contract `0.4.0`)
and [ADR-037](037-persistent-miner-configuration.md) (a miner's scope and settings are applied).

## Context

`memcastle mine --source pi --locator /backups/pi/sessions` was shaped for one built-in source and a path.
With installable sources the shape stopped helping:

- `--locator` is one string whose meaning differs by source (a directory for `directory` and `pi`, a name for `opencode`),
  so help text could not say what it was and a source could not ask for anything else.
- Nothing narrowed a run.
  Mining a coding agent's history from September, or one project's sessions out of years of them, meant mining everything.
- A miner's `scope` and `config` were stored and never applied, so `miner run` refused any miner that used them
  (ADR-037 left the contract extension "to the first source that needs it").

## Decision

- **The first word is the source, and what follows is that source's own.**
  `memcastle mine <source> [place] [key=value]...`, with `memcastle mine <path>` kept as the shorthand for
  `memcastle mine directory <path>`.
  `--wing` and `--full` stay flags, because they are the pipeline's and no source sees them.
  `--source` and `--locator` are removed, not deprecated.
- **Which word is a source is decided without guessing.**
  A word shaped like a source name (lowercase letters, digits, `-`) that is not `directory` is looked up among the daemon's
  sources; one that is not there but is a directory in the current directory is that directory, as `mine docs` always meant;
  anything else is refused with the list of sources and the way to say a directory (`./name`).
  A word with a `/`, or not shaped like a source name (`.`, `~/x`, a capital), is a directory without asking.
  A source wins over a directory of the same name, so a stray `pi/` cannot capture `memcastle mine pi`.
- **A word is an option when it is `key=value` with a key shaped like a source's option name.**
  A path that contains an `=` is still a path, a key given twice is an error, and there is at most one bare word after the
  source.
  This grammar is the CLI's convenience and lives in `client::mine`: the request that reaches the daemon,
  REST and MCP is the
  resolved one, `{source, locator?, options: {key: value}}`, so nothing about the command line is a second API.
- **The locator remains the identity's place and stops being a flag.**
  It is still the `SourceRef`'s `locator`, still what a manifest's `filesystem.read = ["locator"]` grants, and still a
  `MiningSource::Named` field on the wire.
  On the command line it is the bare word after the source.
- **Sources declare the options they accept, and the daemon refuses the rest before queuing.**
  A manifest has an optional `[options.<name>]` table (a `description` and a `type` of `string`, `path` or `date`) and a
  built-in source declares the same in code.
  `GET /api/sources` lists them, `submit_mine` checks the keys and answers `400` with the accepted ones,
  and the source's own
  `identify` judges the values.
  Options are not permissions: they ask for nothing and are no part of the consent digest, so `format` stays 1.
  A `path` option is made absolute by the CLI against the shell's directory, for the reason the directory's path is.
- **The contract is `0.4.0`: `identify` takes the options and `source-ref` carries them.**
  `identify(locator, options)` returns the identity with the options as the source normalised them, and `discover` and `read`
  receive them on the `source-ref` they were already given, so no other function changes.
  Passing them as a new `host` import was weighed and not chosen: it would leave the source free to ignore them,
  and identity is exactly what a source has to decide with the options in hand.
  Adding a field to a record changes the contract for every component, so it is a breaking change before 1.0 like the two
  before it: a source built for `0.3` is `unavailable` with the reason, and the three reference sources are rebuilt.
- **The source decides what is identity.**
  The cursor belongs to the identity (`source`, `account`, `locator`), and `SourceRef::id` hashes nothing else.
  An option that only *narrows* (`since`) must not change it: a later run continues from the same cursor and `--full` reads
  back past it, which costs reading and never duplicates.
  An option that *selects a slice* of the same place (`dir`) must be folded into `account` or `locator` by `identify`,
  or one slice's run would move a cursor past documents another has not read.
  The options themselves are never stored: they ride on the `SourceRef` of a run.
  This is the rule in the contract and the writing guide, and the conformance runner takes `options` in a case.
- **A miner's scope and settings are the options of its run.**
  `miner run` flattens `scope` then `config` to strings (a scalar as its text,
  a list of strings comma-joined) and submits the
  same job `mine` would.
  A key the source does not declare makes the miner not runnable rather than silently unapplied,
  a key in both tables or a
  nested table is refused, and `directory` no longer refuses a scope: it has `since`.
- **Applied by the three reference sources.**
  `directory` (built in and the WebAssembly reference) and `pi` take `since`, `pi` and `opencode` take `dir` too.
  `since` accepts `2026-09`, `2026-09-14` or an RFC 3339 time, in UTC.
  `dir` is a directory or a pattern in which `*` matches any run of characters, `/` included, and nothing else is a wildcard.
  It is compared as the tools record a working directory, which is how a path's spelling stops mattering:
  the CLI resolves a `path` option on the machine it runs on (absolute, `.` and `..` removed, links resolved in the part
  that exists, the part before a pattern's first `*`), and each source normalises the value lexically and compares it with
  the recorded directory stripped of trailing slashes.
  `opencode`'s is `rtrim(directory, '/') GLOB '<pattern>'` in its one query, which cannot bind parameters,
  so the pattern is
  quoted and `?` and `[` are bracketed to stay literal; `pi`'s is a match on the working directory in each session's header.
  A relative `dir` is refused, since it would match nothing and say so only by selecting nothing.

## Consequences

- `memcastle mine opencode since=2026-09 dir=/path/to/workspace` and `memcastle mine /some/path` both work, and help can
  say what a source accepts because the source says it.
- A job on disk or a request from a client that predates this has no `options`, and reads as having none.
  The wire is additive; the contract is not, and installed `0.3` sources need a rebuild.
- A `dir` run and an unfiltered run of the same history are different sources with different cursors, so mining a history
  both ways reads the filtered sessions twice (unchanged revisions are skipped, so nothing is filed twice).
- Option values are strings everywhere.
  A source that wants a list or a number parses it, and a miner's list becomes a comma-joined string, which is a rule each
  source must know.
- Dates are UTC and exact to the day; a `since` has no time zone of its own unless written as RFC 3339.
- The CLI asks the daemon for the list of sources before submitting when the first word could be one, so
  `memcastle mine ./project` costs the one request it always did and `memcastle mine pi` costs two.

## Alternatives rejected

- **A generic `--option key=value` flag.**
  It keeps `--source` and says nothing about what is accepted, and `directory <path>` still needs a flag for the path.
- **`--since` and `--dir` as flags of `mine`.**
  They would be the flags of two sources and mean nothing for a third, and help could not list them per source.
- **Options folded into the locator string.**
  It needs no contract change and defeats validation: the daemon could not refuse a typo,
  and the identity would depend on the
  order the user typed them in.
- **Options in the identity for every source.**
  Safe, and every `since` value would start a cursor of its own and read everything again.
- **Keeping `--source` and `--locator` as hidden aliases.**
  Two spellings of one request to document and test, for a project that is not yet 1.0.

## Note, 2026-10-09: one persistent options table

Miners now store source options in one `[miners.options]` table, replacing `[miners.scope]` and `[miners.config]`.
The old tables require a manual merge, and `miner run NAME key=value` can replace saved options for one run without
changing the miner definition.
An option's source-declared comparison semantics, rather than the table it came from, determines whether a change
requires `--allow-broaden`; unknown changes conservatively require approval.
