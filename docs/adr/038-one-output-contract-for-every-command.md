# ADR-038: Every command with a data answer is readable in a terminal and JSON in a pipe, and `--json` forces JSON

## Status

Accepted.
Amends [ADR-016](016-cli-presentation-follows-the-output-stream.md) (the listings that stayed JSON everywhere, and the
absence of a flag),
[ADR-012](012-status-reports-a-stopped-daemon-and-exits-by-state.md) (`status` follows the stream, and `--json` becomes
global) and [ADR-015](015-database-admin-endpoint.md) (`db start` and `db status` follow it too).

## Context

ADR-016 gave `job list`, the palace hierarchy and the source commands a rendering for a person and JSON for a pipe,
but it left the rest as it was.
`memcastle mine .` therefore still printed the whole job as JSON in a terminal,
as did `audit`, `embed`, `extract`, `repair`, `checkpoint`, every `job` subcommand, `search`, `recall`, `wake-up`, `diary`,
`migrate` and `daemon stop`.
`status`, `db` and `integration` went the other way: they printed text even in a pipe, with a `--json` flag to opt out.

The result was three contracts for one binary, and a person had to know which one a command followed.
`memcastle mine .` is the first thing a new user runs, and it answered with a page of fields in which the one the user
needs next, the job's `id`, is somewhere in the middle.

## Decision

- **One rule for every command with a data answer:**
  a readable rendering when standard output is a terminal, and JSON otherwise.
  A pipe or a file never gets decoration, and a script never has to ask for JSON.
- **`--json` is a global flag** that forces JSON on a terminal.
  It is accepted before or after any subcommand and replaces the per-command flags of `status`, `db start`,
  `db status` and `integration`, which keep working in the same position.
  It never changes an exit code: `status` still exits `0`, `1` or `3`.
- **The rule has one implementation.**
  `term::pretty()` answers it from the flag and the stream (`term::is_pretty` is the pure form that tests cover),
  and `print_for_terminal_or_json` in `main.rs` is the one place a command with a renderer prints.
  A renderer is a function from the daemon's answer to a `String` in `client/`, so the CLI still has no logic that
  MCP or HTTP could not reuse.
- **A job has a card.**
  `mine`, `audit`, `embed`, `extract`, `repair`, `checkpoint` and `job demo` print the queued job as a few labelled lines
  and the command that follows it (`memcastle job show <id>`).
  `job show` prints the same card with its timestamps, error and result.
  `job pause`, `resume`, `cancel` and `retry` print one sentence that says a pause or a cancel is a request.
- **Retrieval is excerpted in a terminal.**
  `search` and `recall` print a numbered block per hit, with its score, where it came from and the first lines of its
  content, because a hit points at a drawer and `drawer show` and the JSON hold the whole of it.
  `diary read` prints each entry whole.
- **Four groups of commands have no answer to render and are unchanged on every stream:**
  `daemon start` and `daemon restart` (the `started:` line), the local `source` commands (their progress),
  `auth generate` (the token alone, so it can be captured, and a secret is never put in JSON) and `completions`.
  `--json` is accepted on them and changes nothing.
- **The readable form is not a contract.**
  It may change between releases, and the JSON is what a script depends on.

## Alternatives rejected

- **Keep ADR-016's exceptions (`search`, `recall`, `diary read` as JSON everywhere).**
  The reason was that free text does not fit a table, and an excerpt does not need one.
- **Keep `status`, `db` and `integration` as text in a pipe.**
  It is the one place the rule was reversed, and a script that wants those has to know it.
  A script that parsed the text must now read the JSON, which `--json` already gave it.
- **A JSON form for the four unchanged groups.**
  `daemon start` and the local `source` commands report progress on a machine, not an answer from the daemon,
  and a token in JSON would end up in logs.
- **A per-command `--json` flag on everything.**
  It would be thirty copies of one definition, and a command added later would forget it.
- **Following the job to completion (`--wait`).**
  Useful, and a different feature: the output rule holds for it too.

## Consequences

- Scripts that read the text of `status`, `db` or `integration` from a pipe break and must read JSON.
  This is a breaking change and is released as one.
- `CLICOLOR_FORCE` no longer puts colour into a command's result in a pipe, because colour belongs to the readable form.
  It still colours help, diagnostics and the progress of the local `source` commands.
- The readable forms are only exercised by unit tests: no test runs the binary on a pseudo-terminal,
  so the stream decision is covered through `term::is_pretty` and through the piped output of every command.
