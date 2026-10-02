# ADR-016: CLI presentation follows the output stream: tables, colour and prompts for a terminal, plain data for a pipe

## Status

Accepted, amends [ADR-012](012-status-reports-a-stopped-daemon-and-exits-by-state.md)
(a command may choose its form by stream instead of by flag)

## Context

Apart from `status` and `db`, every client command printed pretty JSON.
That is right for a script and poor for a person: `jobs list` was a wall of nested objects, a destructive command
(`repair --apply`, `jobs cancel`, `auth revoke`) ran the moment it was typed, and the help text was monochrome.

The same output is also read by machines.
`memcastle jobs list | jq` is in the quickstart, `auth generate` prints a token that is captured into a secret manager,
and the test-suite parses stdout.
Anything that changes what a pipe sees breaks them silently.

ADR-012 settled `--json` as a per-command flag for the commands with a human rendering.
Adding the same flag to `jobs list` would make scripts opt in to what they always had.

## Decision

- **A person gets decoration, a pipe gets plain data, and the stream decides.**
  The decision is made from whether the stream is a terminal, never from a flag, so a script needs no change.
- **Colour** is on for a stream that is a terminal and wants it.
  `NO_COLOR` turns it off, `CLICOLOR=0` turns it off, `CLICOLOR_FORCE` turns it on, and `TERM=dumb` is respected.
  stdout and stderr are decided independently.
  Colour surrounds words and never replaces them: with escapes stripped, a coloured report is the plain report.
  Help and clap's own errors use a palette from the same rules.
- **`jobs list` is a table on a terminal and JSON otherwise.**
  There is no `--json` flag on it: a pipe already gets JSON, and a person who wants JSON on a terminal can pipe to `cat`.
  The other listings (`search`, `recall`, `diary read`) return free text that a table would truncate,
  so they stay JSON everywhere.
  `status`, `db start` and `db status` keep their `--json` flag, which predates this decision and is a scripting contract.
- **Destructive commands ask first, on a terminal only.**
  `repair --apply`, `auth generate` (which replaces the previous token), `auth revoke` and `jobs cancel` prompt on stderr,
  default to "no", and accept `--yes`.
  When stdin or stderr is not a terminal they never prompt and proceed, because a script that runs
  `memcastle repair --apply` has already decided.
  A declined prompt is the error `memcastle::cli::aborted`, so a chained command stops.
- **Stdout stays clean.**
  The token printed by `auth generate` is the only thing on its stdout, and its prompt and guidance go to stderr.
- **Completion** is a subcommand, `memcastle completions <shell>`, generated from the command definition.
  It needs no daemon and no valid configuration.
  Enumerated arguments (`--mode`, `jobs list --status`) are declared with their values so completion can offer them.
- **There is no `help` subcommand.**
  `memcastle help` and `memcastle jobs help` only repeated `--help`.
- **One colour stack.**
  `console` does the terminal detection and styling for `dialoguer` and `comfy-table` alike, and clap styles its own help.

## Alternatives rejected

- **A `--json` flag on `jobs list`, table by default.**
  It breaks every existing pipe until it is edited, and asks scripts to opt in to what they already had.
- **A global `--format` or `--color`.**
  ADR-012 already rejected the former for promising a human form most commands lack.
  The latter duplicates `NO_COLOR` and `CLICOLOR_FORCE`, which every terminal user and CI system already knows.
- **Prompt unless `--yes` is given, even without a terminal.**
  The safest default and the one that hangs CI until a timeout kills it, or fails every script on its first run.
  The protection a prompt gives is against a typo at a keyboard, which is exactly the case a terminal check covers.
- **Tables for every listing.**
  Search hits and recalled content are paragraphs, and a table either truncates them or wraps into noise.
- **Dynamic completion that asks the daemon for job ids.**
  Useful, but unstable in `clap_complete`, and a completion that needs a running daemon is slow and surprising
  when it is not.
- **Generating completions in `build.rs`.**
  Releases are bare executables per platform, so there is no archive to carry the files, and a build script cannot
  produce them for a cross-compiled target without running it.

## Consequences

- `jobs list` on a terminal is not stable to parse, and a script that wants JSON from a terminal session must pipe.
- An interactive session that wanted to skip a confirmation must say `--yes`, and a script that relied on a
  command executing without a question is unaffected.
- A terminal narrower than the table lets the free-text column shrink first and then overflows,
  rather than breaking identifiers across lines.
- The package builds (Homebrew, deb and rpm, AUR) generate the completion files by running the released binary.
- Four new dependencies: `clap_complete`, `console`, `dialoguer` and `comfy-table` (with `crossterm` behind it).
