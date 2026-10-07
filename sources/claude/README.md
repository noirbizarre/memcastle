# Claude Code history source

This WebAssembly source reads Claude Code's documented JSONL session transcripts from
`~/.claude/projects/<project>/<session>.jsonl`.
It is separate from any live Claude Code integration: it reads finished or active local transcript files directly and never
starts Claude Code, contacts a service, or reads Claude settings or credentials.

## Use it

```sh
memcastle source install sources/claude --enable
memcastle mine claude
memcastle mine claude /backups/claude/projects since=2026-09 dir=/work/app
```

It requests read-only access to the supplied locator and `~/.claude/projects`, plus `HOME` only to locate that default.
It has no process, network, write, or credential permission.

Each transcript is one `transcript` document tagged `claude` and `transcript`.
The source retains user and assistant text plus compact `[tool: name]` markers, and deliberately drops thinking, tool results,
system and compaction records, attachments, patches, malformed lines, and unknown records.
The source never follows symlinks and only considers `.jsonl` files exactly one directory below the projects root.

The `watch` capability is declarative only.
Define and enable a miner and trigger yourself; installing this package does not start background mining.
