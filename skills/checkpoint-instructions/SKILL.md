---
name: checkpoint-instructions
description: Decide what is worth persisting to MemCastle and how to write it as a checkpoint. Use after a decision is made, a problem is solved, something is discovered, the user states a preference, or before a long session ends or its context is lost.
license: MIT
compatibility: Needs the MemCastle MCP server connected, and a memory mode of `full`.
metadata:
  memcastle-version: ">=0.2.0"
---

# Checkpoint durable context

A checkpoint is how an agent hands something to MemCastle to keep.
MemCastle stores what it is given and decides nothing, so what to keep, and how to word it, is this skill's job.

## What is worth a checkpoint

Persist what a future session would be worse off without:

- a decision, with the reason and the alternative that was rejected
- a problem solved, with its cause and the fix
- a discovery about the project, its tools or its constraints
- a preference the user expressed, or a correction they made

Skip what is mechanical or cheap to rediscover: commands that were run, file listings, intermediate attempts,
anything already in the repository, and anything the user asked not to keep.
Never store secrets, tokens or credentials.

## Write each item for a reader with no context

An item is read months later, by a session that saw none of this one.
State the fact in full sentences, name the project and the subject, and include the reason.
One idea per item: several small items are found more reliably by a keyword search than one long one.

## Classify each item

Pick the `destination` before calling the tool:

- `preference`: how the user likes things done, across projects
- `project`: a decision or fact about one project
- `diary`: a first-person note on the session, which the `diary` skill writes with its own tool
- `general`: anything durable that fits none of the above

## Call `memcastle_checkpoint`

The `payload` argument is a JSON object with an `items` list:

```json
{
  "items": [
    {
      "destination": "project",
      "content": "memcastle: the search is lexical, so queries use short keywords. Chosen over embeddings to keep one datastore.",
      "tags": ["search", "architecture"],
      "source": { "kind": "manual", "agent": "my-agent" },
      "fact": null
    }
  ]
}
```

- `content` is required and must not be blank.
- `tags` is required, and `[]` is fine.
- `source.kind` is `manual` for text you wrote, or `file` when it comes from a file, with its path as `source.uri`.
- Leave `fact` as `null`: it needs entity and relationship ids that no MCP tool creates.
- `wing` overrides the default wing for the destination, and should be left out unless the user named one.
- `emergency: true` jumps the queue and is only for context that is about to be lost, such as a session being cut off.

The call returns a job straight away and does not mean the items are stored.
Call `memcastle_job_get` with the job `id` when it matters that they landed, for example before a session ends:
a `completed` status means stored, and a `failed` one carries the reason in `error`.
Retry a failed job with `memcastle_job_retry` rather than submitting the same items again.

## When it is refused

- `memcastle::input::invalid`: the payload is malformed, so fix it from the message and its `help` line.
- `memcastle::mode::forbidden`: the session is `read_only` or `disabled`.
  Stop, do not retry, and do not call `memcastle_set_mode` to get around it.
  In these modes, do not queue the content anywhere else on the user's behalf.
