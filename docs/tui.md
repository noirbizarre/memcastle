# Terminal operations console

Run `memcastle daemon start` (or `memcastle serve`), then `memcastle tui` in an interactive terminal.
It uses the same palace configuration, daemon discovery, token and memory-mode header as the ordinary CLI.
Both stdin and stdout must be terminals; a pipe or `--json` is refused with a diagnostic.
Use `memcastle job list`, `memcastle search` and other one-shot commands in scripts.

The console subscribes to the **same authenticated `GET /api/events` stream** as the web UI.
An event contains a job identifier, not its progress: the console rereads the job through REST after a notice.
It reconciles on reconnect, after a missed-events notice, periodically and when you press `r`.
That last refresh matters when more than one daemon serves a remote palace: each daemon's event bus is local to its process.
A disconnected indicator means displayed data might be stale.

| View | Purpose | Keys |
|---|---|---|
| Jobs (`1`) | Active and recent mining jobs, progress, elapsed time, errors and an activity feed. | `↑`/`↓` or `k`/`j` select; `n` enter `<source> [place] [key=value]... [--wing NAME] [--full]`; `p` pause a running job; `u` resume a paused job; `c` request graceful cancellation; `f` confirm best-effort force-cancel; `t` retry a failed job. |
| Search test (`2`) | Run a diagnostic query on the existing search path and inspect hits and scores. | `/` enter a query; `g` set ranking; `w` limit to a wing (empty input resets it). Elapsed time is measured by this client, not the daemon. |
| Readiness (`3`) | Installed-source lifecycle, source sign-in, configured-miner availability and configured provider names. | `s` enter a source name to begin sign-in; `m` enter a configured miner name to run it; `↑`/`↓` scroll long status or browser instructions. |
| Maintenance (`4`) | Read-only consistency audit, repair dry run, and embedding/entity extraction sweeps. | `a` audit; `d` repair dry run; `e` embed; `x` extract; `↑`/`↓` select a job and Enter inspect its result; `Esc` returns to the list. |

`Tab` advances views, `r` refreshes, `q` or Ctrl-C exits, `Esc` abandons a prompt, and Enter submits its input.
Quote mining paths with spaces as you would in a shell; the same source-option validation as `memcastle mine` applies.
Only actions applicable to the selected job's current status are accepted; the daemon still validates every action.
The result appears after the daemon acknowledges it, and job status is reread instead of changed optimistically.
An indeterminate `◌` means the job has no meaningful total; a bar and completed/total count appear only when it does.
The job's progress message is the latest phase-like detail available, not an estimated throughput.

Force-cancel asks for confirmation, durably requests cancellation, aborts a task owned by this daemon and
persists the cancelled state under the worker's lease before reporting success.
It cannot undo work already filed or instantly stop blocking adapter work; use graceful cancellation when a checkpoint
between documents matters.
For a mining job owned by another daemon, use that daemon or request graceful cancellation instead.

OAuth runs on the daemon and keeps tokens there.
The sign-in prompt shows the provider's URL and, for a device flow, its user code;
complete browser flows on the machine running the daemon so its loopback callback can be reached.
A source listing marked unsigned-in can also mean the credential store could not be read;
retry the flow or inspect the daemon's diagnostics to distinguish the two.
Provider sweeps are submitted as jobs and require their corresponding provider to be configured.
The provider names show configuration, not a live connection or authentication check; the search test and sweep job
report whether the provider actually works.
The maintenance menu intentionally includes audit (read-only), repair dry run (no destructive write),
and provider-backed derived-data sweeps; applying repairs, editing configuration and memory browsing remain separate commands.
