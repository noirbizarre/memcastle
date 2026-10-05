# ADR-020: Agent skills are plain files versioned with the repository, and checked against the surface they name

## Status

Accepted, builds on [ADR-019](019-shared-integration-contract.md) (the contract's `skills` capability has no daemon
operation) and [ADR-013](013-release-packaging-and-asset-resolution.md) (a package asset needs a consumer, and there is
none for skills).
Amended by [ADR-034](034-agent-integration-distribution.md): an installed integration now reads the skills, so releases
package them under `share/memcastle/skills/` and the integration installer copies them beside the integration.

## Context

MemCastle's MCP tools expose what it can do.
Knowing when to call them is a different thing: search before answering, checkpoint a decision, read the wake-up context
at the start.
That advice is useful to every agent client and has to be the same text in each, or the clients drift.

[ADR-019](019-shared-integration-contract.md) assigns this text to `skills/`, and Pi, OpenCode and Claude Code are meant
to load it instead of keeping a copy.
What was missing is a place where users get the skills without an integration, a rule for keeping them in step with the
binary, and anything that notices when a skill names a tool that no longer exists.

Skills are text, so nothing compiles them.
A renamed tool leaves an agent following instructions that fail at run time, and no test would say so.

## Decision

- **A skill is a directory with a `SKILL.md`** under `skills/`, in the Agent Skills layout, with a `name` equal to the
  directory and a `description` that says when to use it.
  Clients that scan a skills directory discover it with no adapter.
- **The repository at a release tag is the distribution.**
  Installing is copying the directories into the client's skills location, which `docs/skills.md` lists for each client.
  MemCastle adds no installer, registry or command, and nothing is attached to a release.
- **A skill declares `metadata.memcastle-version`,** a semver range of the MemCastle versions it is valid for.
  MemCastle keeps backward compatibility where it can, so the usual value is a lower bound such as `>=0.2.0`.
  A change that breaks a skill ships with the updated skill in the same repository change, which raises the bound.
  A test requires a parsable range with a lower bound that the crate's own version satisfies.
  The `memcastle-setup` skill compares the installed binary with the range, so an agent notices skills that are newer
  than its binary.
- **`tests/in_process/skills.rs` holds a skill to the surface it names.**
  Every `memcastle_*` tool must be registered by a real daemon, every command must be in the CLI's help, and every route
  must be in the documented table.
  A skill may not point an agent at the database endpoint, storage or the credential routes.
- **A skill that calls a gated tool says what a refusal means** and that the agent must not change its own mode to get
  around it.
  Skills carry instructions and no palace content, so loading one does not leak memory into a `disabled` session.
- **The setup skill needs no daemon.**
  It detects the binary, starts the daemon and configures the MCP client, and its only tool call is the final check.
- **The skills are linted as documentation,** with the same Markdown rules as `docs/`.

## Alternatives rejected

- **A skills archive attached to each release.**
  It means editing the publish workflow, which belongs to the project template, for something `git clone --branch <tag>`
  already gives.
- **Installing the skills with the OS packages,** under `share/memcastle/` as [ADR-013](013-release-packaging-and-asset-resolution.md)
  describes.
  No part of MemCastle reads them, so it would be an asset with no consumer, and packages would have to be rebuilt for a
  wording change.
- **Requiring the skill version to equal the crate version, or a `major.minor` line that tracks it.**
  The release workflow rewrites the crate version and nothing else, so every release would fail CI until someone
  edited all the skills by hand, whether or not any of them changed.
  A range with a lower bound needs no edit for a release that changes nothing a skill relies on.
- **No version in the skills at all.**
  The tag already identifies a release, but a skill copied out of its checkout would then carry no record of what it was
  written for.
- **The daemon serving the skills,** for example as MCP prompts or resources.
  It would put a behavioural policy into the daemon, which stays a memory runtime, and it would make the setup skill
  depend on the daemon it is meant to start.
- **A skill per tool.**
  The tools are already described to the agent by the daemon, so a skill that restates one only adds a place to go
  stale.
  The skills follow what an agent does over a session instead.

## Consequences

- Users get the skills at the version of the binary they run, by a copy that works for any client that scans a skills
  directory, with nothing to build.
- A tool, command or route that is renamed or removed fails the build until the skills are updated, which is the check
  this decision exists to add.
- The version field is a compatibility claim and not a pin: nothing stops a skill from being stale in prose after a later
  release changed a behaviour it describes, unless that change also raised the bound.
  Review has to catch that.
- The check is strict, so a skill cannot declare a floor above `Cargo.toml`'s version.
  A skill that needs a feature of an unreleased change keeps the previous floor until the Release PR bumps the crate,
  and its bound is raised after that.
- A client that installs one skill alone gets a working one, because skills name each other and do not link.
  The cost is that a reference to a skill that was not installed is a name an agent cannot resolve.
- The skills are checked against the daemon, the CLI and the documented routes, and not against any client's own tool
  naming, such as the prefix OpenCode adds to each tool.
  A skill therefore names the tool as the daemon does.
