# Skills

A **skill** is reusable agent behavior/instruction — plain text an agent loads on demand.
It is not lifecycle glue and it is not code that calls MemCastle directly; that's an integration's job.
The same skill is usable, word for word, from Pi, OpenCode, and Claude Code alike —
if a skill needs to be rewritten per ecosystem, it wasn't generalized enough.

The user-facing guide, with the install workflow for each client, is the
[Agent skills](../docs/skills.md) page. The decision behind it is
[ADR-020](../docs/adr/020-skills-are-versioned-with-the-repository.md).
See [`integrations/README.md`](../integrations/README.md) for the corresponding distinction on the code side.

## Skills

```text
skills/
  memcastle-setup/          Detect, install, start and connect MemCastle. Works with no daemon running,
                            because it is what starts one.

  search-before-answer/     Search MemCastle before answering questions about past work, decisions, people,
                            or preferences — quote retrieved content verbatim, never paraphrase.
                            Relevant and generous, never a forced callback on every single message.

  checkpoint-instructions/  What is worth checkpointing (a decision, a solved problem, a discovery, a preference)
                            versus what to skip, how to classify it into a destination
                            (preference / project / diary / general) and how to check that it landed.

  wake-up/                  How to load the session-start context and use it: treat it as established fact,
                            don't re-ask what it already answers.

  diary/                    What belongs in a diary entry versus a checkpoint item, and how to write and read one.
```

Each skill is a directory with a `SKILL.md` (frontmatter `name` equal to the directory, `description`, `license`, and
`metadata.memcastle-version`, a semver range such as `>=0.2.0`).
The split follows what an agent does over a session, not the MCP tool list: there is no skill per tool.

## Rules for authors

- Reference only what this release exposes: `memcastle_*` tools, `memcastle` CLI commands and documented `/api/` routes.
  `tests/in_process/skills.rs` fails when a skill names something that does not exist.
- Never describe the database endpoint, storage internals or the credential routes.
- A skill that calls a mode-gated tool says to stop on `memcastle::mode::forbidden` and never to call
  `memcastle_set_mode` to get around it.
- A skill is self-contained: it links only to files in its own directory and names other skills by name.
- Carry no palace content, so a skill is safe to load into a session whose memory mode is off.
- Declare `metadata.memcastle-version` as a semver range, normally `>=x.y.z` with the release that introduced the last
  thing the skill relies on.
  Raise the floor when the skill starts to depend on a later release, or when a breaking change ships with the skill
  update, and only once `Cargo.toml` carries that release: `tests/in_process/skills.rs` requires the crate to satisfy the range.
- Write Markdown as the rest of the documentation is written (semantic linefeeds, 120 columns): `SKILL.md` files are
  linted with it. This README is a working document outside that lint scope.

## Non-goals

No skill registry, package manager or build step of their own — a skill is plain text, and installing is copying.
`memcastle integration install` does that copying into an integration's installed copy, for the skills its manifest names
([ADR-034](../docs/adr/034-agent-integration-distribution.md)); nothing else installs a skill, and a skill stays usable
without any integration.
The `skills` row of the [integration contract](../docs/integration-contract.md) requires each integration to load its
instructions from here instead of keeping its own copy, and never to load one that carries MemCastle-derived content
into a session whose memory mode is off.
