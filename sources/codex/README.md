# Codex history source

The Codex source reads one document per `rollout-*.jsonl` transcript under `~/.codex/sessions/YYYY/MM/DD/`.
It is a sandboxed WebAssembly component and is separate from the Codex integration, which only configures live MCP use.

It has read-only access to the selected locator and the default sessions directory, plus `HOME` to locate that directory.
It has no network, process, write, or credential permission.

The source files user and assistant messages plus tool-call names.
It deliberately excludes model reasoning, tool output, command/file contents, injected context, and unrecognised record types.

```sh
memcastle source enable codex
memcastle mine codex
memcastle mine codex /backups/codex/sessions
```
