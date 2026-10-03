---
name: diary
description: Keep a personal diary in MemCastle, written in the agent's own voice, and read it back. Use at the end of a meaningful session or work stage to leave a note for the next session, or when the user asks what the agent noted before.
license: MIT
compatibility: Needs the MemCastle MCP server connected. Writing needs a memory mode of `full`, reading one that allows reads.
metadata:
  memcastle-version: ">=0.2.0"
---

# Keep a diary

The diary is the agent's own running notes, one identity at a time.
It differs from a checkpoint, which records durable facts for anyone to find:
a diary entry says what this session was like and what the next one should pick up.
The next wake-up loads the latest entry, so write for that reader.

## What belongs in an entry

- what was worked on and where it stands, including what is half-done
- what was tried and did not work, so it is not tried again
- open questions and what to do first next time
- how the collaboration is going, when it affects how to work with the user

Facts and decisions that should outlive the session belong in a checkpoint instead,
see the `checkpoint-instructions` skill.
Never write secrets, tokens or credentials in an entry.
Write one entry per meaningful stage, not one per message, and do not write when nothing happened.

## Write with `memcastle_diary_write`

- `agent_identity`: the stable name of this agent, the same string every session and the same one given to wake-up.
- `wing`: the project the entry belongs to.
- `content`: the entry, in first person and full sentences, readable with no other context.

The write is immediate and not a job, and the entry is filed in the wing's fixed diary room.
Use this tool for diary entries rather than a checkpoint with destination `diary`,
because it is the one that takes the wing and the identity directly.

## Read with `memcastle_diary_read`

Pass `agent_identity` and `wing`, and optionally `limit` (20 by default).
Entries come newest first.
Read the diary when the user asks what was noted earlier, or when the wake-up entry is not enough to continue.

## Memory modes

Writing is refused with `memcastle::app::mode_forbidden` in `read_only` and `disabled`, and reading in `disabled`.
Stop at the first refusal, do not retry, and do not call `memcastle_set_mode` to get around it.
