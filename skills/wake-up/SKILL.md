---
name: wake-up
description: Load MemCastle's session-start context and treat it as established fact. Use once at the start of a session, before the first substantive answer, and again after a context reset.
license: MIT
compatibility: Needs the MemCastle MCP server connected, and a memory mode that allows reading.
metadata:
  memcastle-version: ">=0.2.0"
---

# Wake up with context

Wake-up is the briefing a session starts with: the agent's latest diary entry and the most recent highlights
that earlier sessions checkpointed.
Reading it first means the user does not have to repeat themselves.

## Call `memcastle_wake_up`

- `agent_identity` is required: a stable name for this agent, reused every session, because the diary is read by identity.
  Use the same string as the `diary` skill does.
- `wing` selects the project whose diary and highlights to load.
  The diary is only included when a `wing` is given, so pass the project's wing when there is one.
- `max_items` and `max_bytes` bound the result, to 10 items and 8192 bytes by default.
  The defaults suit a session start, and a larger budget costs context for little gain.

The reply has a `diary` entry, `recent_highlights` newest first, and `generated_at`.
Highlights come from checkpoints, are filtered by wing and not by identity,
and an item that does not fit the byte budget is dropped whole.

## Use what came back

- Treat it as established fact about the user and the project, and act on it without re-asking.
  Do not ask the user a question the wake-up already answers.
- Quote it verbatim when you repeat it, and do not paraphrase.
- It is a briefing and not the whole memory: for anything it does not cover, use the `search-before-answer` skill.
- If it conflicts with what the user says now, the user wins: say so, and offer to checkpoint the correction.
- An empty result is normal for a new palace or wing.
  Carry on without comment.

## Memory modes

In `disabled` the call is refused with `memcastle::app::mode_forbidden`.
Stop there, do not retry, and behave as if MemCastle did not exist.
`read_only` allows wake-up.
Do not call `memcastle_set_mode` to get around a refusal.
