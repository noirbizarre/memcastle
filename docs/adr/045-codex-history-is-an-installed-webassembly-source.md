# ADR-045: Codex history is an installed WebAssembly source

## Status

Accepted. Builds on ADR-023 (the source model), ADR-026 (WebAssembly sources), ADR-033 (distribution), and issue #89.

## Context

Codex stores session rollout transcripts as JSONL below `~/.codex/sessions`.
They must be mineable after a session ends without adding Codex-specific storage code to MemCastle or granting a source
access to Codex credentials, configuration, a network, or a shell.

## Decision

- Codex history is the `codex` source, a WebAssembly component built from `sources/codex/`.
- It reads only `rollout-*.jsonl` files at the dated session depth, either below its default directory or a supplied
  locator.
- It receives read-only access to that directory and `HOME` only to resolve the default; it has no process, network,
  write, or wider `~/.codex` permission.
- It files user and assistant messages with tool-call markers, and omits reasoning, tool output, command and file
  contents, injected context, and unknown records.
- A modification-time/path cursor and content revision make the source incremental and idempotent.

## Alternatives rejected

Reading all of `~/.codex` would make a transcript source an ambient-authority reader and risk exposing credentials.
Making it a native adapter would put Codex's evolving record format in the daemon core.
Using the live Codex integration to acquire history would conflate lifecycle glue with archival acquisition.

## Consequences

Codex can change its JSONL schema without destabilising the daemon: unsupported lines are skipped.
The source may need updating when Codex changes the session path or the useful record shapes.
The release bundles it like Pi and OpenCode, so it is installed from the start and only needs enabling.
