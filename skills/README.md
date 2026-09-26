# Skills

A **skill** is reusable agent behavior/instruction — plain text (or a small
bundle of text + examples) an integration injects into an agent's context or
prompt. It is not lifecycle glue and it is not code that calls MemCastle
directly; that's an integration's job. The same skill should be usable, word
for word or close to it, from Pi, OpenCode, and Claude Code alike — if a skill
needs to be rewritten per ecosystem, it wasn't generalized enough.

See [`integrations/README.md`](../integrations/README.md) for the corresponding
distinction on the code side, and `PLAN.md` for the full rationale.

## Planned skills

```text
skills/
  search-before-answer/     Search MemCastle before answering questions about
                             past work, decisions, people, or preferences —
                             quote retrieved content verbatim, never paraphrase.
                             Reinjected every turn, not just session start.
                             Relevant and generous, never a forced callback on
                             every single message.

  checkpoint-instructions/  How to decide what's worth checkpointing (a
                             decision made, a problem solved, a discovery, an
                             expressed preference) versus what to skip
                             (mechanical actions, trivial detail), and how to
                             classify it into a destination bucket
                             (preference / project / diary / fact) before
                             calling MemCastle's checkpoint operation.

  wake-up/                  How to use a MemCastle wake-up context once
                             injected: treat it as established fact, don't
                             re-ask the user things it already answers.

  diary/                    Conventions for what belongs in a diary entry
                             versus a general checkpoint item.
```

## Status

No skill content exists yet. Authoring `search-before-answer` and
`checkpoint-instructions` is tracked under the "Phase 2 — Pi integration"
milestone (they are authored once, against Pi, then reused as-is by OpenCode
and Claude Code in Phases 3–4).
