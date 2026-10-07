# ADR-043: Triggers are opt-in `[[triggers]]` that only ask for a run, and webhooks have a listener of their own

## Status

Accepted.
Amends [ADR-037](037-persistent-miner-configuration.md) (a miner's `trigger` was stored and never acted on).
Builds on [ADR-015](015-database-admin-endpoint.md) (an opt-in, loopback-by-default listener on the daemon's own
process), [ADR-014](014-optional-token-authentication.md) (every route of the main router is behind the daemon's token),
[ADR-023](023-unified-source-model-for-mining.md) and [ADR-026](026-pluggable-source-adapters-as-webassembly-components.md)
(a source acquires, the pipeline is idempotent, administrative operations are REST and CLI only) and
[ADR-041](041-server-sent-events-for-dashboard-updates.md) (change notices carry identifiers, never content).
Supersedes the integration-specific background-mining stories #26 (Pi) and #125 (OpenCode).

## Context

Issue #189 asks for a generic way to ask for mining runs on a signal other than a person typing a command: a
timetable, a poll, a file change, a webhook.
Sources differ in what they can offer, some offer nothing, and starting any of these has consequences a user has to own:
a webhook needs a reachable endpoint and a secret, a watcher holds file handles, a timetable keeps a daemon busy.
Until now `[miners.trigger]` stored an unvalidated `event` or `schedule` that nothing acted on, and the Pi and OpenCode
integrations each planned a private daily timer.

Seven decisions follow, and each has an alternative that is easier for one of the forces and worse for the others.

- Where a trigger is defined, and what is its own.
- What a trigger does when it fires.
- How a source takes part.
- Where a webhook is received.
- What is remembered, and what is not.
- How bursts, repeats and restarts are survived.
- Who may change any of it.

## Decision

- **A trigger is a `[[triggers]]` entry, separate from the miner, disabled by default.**
  It has a name, the miner it asks to run, a type (`schedule`, `poll`, `webhook`, `watch`) and that type's settings.
  Several triggers may point at one miner and are enabled and disabled independently, which one `[miners.trigger]`
  table could not express.
  `enabled` is `false` unless the file says `true`, and nothing the daemon does (installing a source, enabling a miner,
  recovering after a crash, a re-install) ever writes it.
  The daemon rewrites only the `[[triggers]]` section, in place, with the machinery that already rewrites `[[miners]]`
  and under one lock, so the two never interleave.
  `[miners.trigger]` is removed rather than kept: it never did anything, 1.0 is not reached, and a miner with a second,
  inert idea of a trigger beside the real one would only mislead.
  A configuration file that still has one is refused when it is loaded, naming the key, and the fix is to delete it.
- **A trigger only decides *when*, and asks the way a person does.**
  Every signal ends in one call, the request for a run that `memcastle miner run` makes (`request_miner_run`), so
  a trigger can cause nothing a person could not and takes the same checks.
  A manual firing (`memcastle trigger fire`) is that call too, which is what "manual uses the same path as
  event-driven" means in code.
  A trigger does no acquisition, normalisation, deduplication or extraction, and no payload (a delivery's body, a
  changed path) is passed on: the source is asked to discover from its cursor and finds what the signal was about.
  This is what lets the pipeline's idempotence make a spurious or repeated signal cost a pass and never a duplicate.
- **A source declares capability, and the host provides the rest.**
  A manifest may list `[triggers.watch]` and `[triggers.webhook]` with a description.
  It is not a permission (so it is outside the consent digest), adds no WIT function and leaves `format` at 1, the same
  way `[options]` did in ADR-042.
  A timetable and a poll only mean "mine again", so every source has them; a webhook and a watch need the source to say
  what a delivery or a change means, so they must be declared.
  A source that declares nothing is complete.
  Enabling a trigger the source does not support is refused.
- **A webhook has its own listener, which is closed unless asked for.**
  Deliveries go to `POST /hooks/{trigger}` on a socket of their own, never the main router: the daemon's bearer token
  authenticates clients, a webhook's sender can only be given a secret for its own trigger, and invariant 6 (every
  route of the main router is behind the token) stays true with no exception added to `is_public`.
  The listener is off unless `[webhook] enable = true`, binds loopback unless `allow_remote` *and* authentication are on,
  must not share a port, is bound only while a webhook trigger is enabled, and is bounded in body size and concurrency.
  Each trigger names a secret by reference (`env` or `file`, never in the file), read at each delivery so rotation needs
  no restart; HMAC-SHA-256 of the body or the secret itself is compared in constant time, and every refusal is the
  same empty `401`.
  The body is hashed and dropped.
  The daemon never opens a port to the outside, configures a tunnel or changes a firewall: enabling a webhook
  is refused until the listener, the secret and the miner are in place, and the documentation lists the rest (TLS, a proxy
  or tunnel, a firewall rule) as the operator's.
- **The palace remembers progress, never intent.**
  `trigger_state` holds when a trigger last fired, when a timetable is next due, counters and the last failure;
  `trigger_delivery` holds the id of each accepted delivery that carried one, with the job it became.
  Neither can enable anything.
  A timetable's next due time is stored, so a restart neither forgets the wait nor fires early, an overdue slot fires once
  and not once per missed slot, and enabling never fires on the spot.
  A delivery is recorded before it is queued and linked to its job after, and one found without a job at startup is
  queued if its trigger is still enabled and dropped if not.
  An id is remembered for seven days.
- **Bursts are coalesced where the work is queued, not guessed at.**
  A request that finds a run of the same source with the same settings still queued joins it; a run already running does
  not count, because it may have passed the change.
  At most one run is running and one waiting per miner however many signals arrive, without a job-level idempotency
  key the pipeline does not need.
  The listener limits concurrent deliveries (`429` beyond), a watcher debounces, a poll backs off while failing, and the
  supervisor retries a watcher or a listener that failed with a growing wait.
  A failure is stored and shown, never stops the daemon or another trigger, and `unavailable` is computed on every look.
- **Defining, enabling and firing are administrative.**
  They are REST and CLI only; MCP has `memcastle_trigger_list` and `memcastle_trigger_get`, read-only, so an agent
  can say what runs unattended and cannot change it.
  The dashboard shows triggers read-only.
  Changes are published as `trigger` events carrying a name and a fixed word.

## Alternatives rejected

- **Keep `[miners.trigger]` and make it act.**
  Smallest change, but one trigger per miner cannot be enabled independently of another, and a file written when it was
  inert would start running on upgrade.
- **Keep `[miners.trigger]` as a deprecated, ignored table.**
  It would keep an old file loading, at the price of a second meaning of "trigger" in every page and answer.
- **A webhook route on the main router, behind the token.**
  Simplest, and invariant 6 would hold, but GitHub and most services cannot send a custom bearer token and sign with their
  own scheme, so it would serve nobody, and an exception in `is_public` would have been the first on a route that
  writes.
- **Webhook payloads carried to the source.**
  It would let a source act on exactly what changed, at the price of a trigger contract in the WIT, a payload
  that is attacker-influenced input to sandboxed code, and a path that bypasses discovery's idempotence.
  A source that reads a service on request, plus a trigger, covers the same cases with no new contract.
- **A scheduler in each integration.**
  What #26 and #125 planned.
  Two timers with two ideas of "once a day" and no daemon-side state; a trigger is one, configured once, visible in one
  place, and an integration remains free to keep its own.
- **A cron expression parser.**
  A dependency and a grammar for what `every` and `at` cover.
  It can be added as a further type without changing anything here.
- **Remembering the enabled state in the palace.**
  It would let a restart or a recovery reach a state the user never chose, and give two homes to one fact.

## Consequences

- The daemon starts nothing on its own: no task, port or watcher exists until the user enables a trigger, which is
  enforced by `tests/in_process/triggers.rs` and by the trigger module knowing nothing but what its host hands it
  (`tests/trigger_isolation.rs`).
- `notify` is a new dependency, for the watcher; `hmac` is the one already in the lockfile.
- Vendor integrations (Todoist, GitHub, Slack) are sources plus a trigger the user chooses, and are not in this layer.
- A filesystem watcher sees only the daemon's own machine and the operating system's limit on watches;
  both are reported as a failing trigger, and a poll is the fallback.
- Schemes that sign a timestamp too (Slack's) are not supported; a proxy that verifies them can sit in front.
- The integration contract's "the daemon has no scheduler" is amended: it has none unless the user configures one, and a
  client's request is still a request.
- Several daemons on one remote palace each run their own triggers from their own file, so two that watch the same
  source will each ask; the join-a-waiting-run rule is per daemon, and the pipeline's idempotence is what keeps it safe.
