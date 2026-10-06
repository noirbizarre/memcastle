# ADR-041: The daemon pushes change notices over server-sent events, and the dashboard reads again when one arrives

## Status

Accepted.
Amends [ADR-035](035-web-dashboard.md) (its "read on demand, neither poll nor push" decision: pages still read on demand,
and now also when the daemon says something changed).
Builds on [ADR-006](006-job-leases.md) (several daemons may share a remote palace),
[ADR-007](007-memory-mode-gate-follows-data-access.md) (what a mode may read) and
[ADR-014](014-optional-token-authentication.md) (every route but health is guarded, and a token is a header).

## Context

Issue #215.
The dashboard of ADR-035 reads when a page opens, when the Refresh button is pressed and after the user's own actions.
That needed nothing the API did not already offer, but a job's progress or a new memory only shows when the user asks.
ADR-035 left a push to a later decision, because it needs an event source in the daemon, a route and its tests.

The forces:

- **A client must be able to learn that something changed** without a timer, which costs a request per page per interval
  whether or not anything changed.
- **The authentication layer must not grow a second path.**
  A browser `WebSocket` and `EventSource` cannot send an `Authorization` header, and a token in a URL lands in logs,
  history and `Referer` (ADR-035 rejected that).
- **Nothing may see what a read would refuse.**
  Memory modes are checked per request (ADR-002, ADR-007), so a push must not carry content the mode would not return.
- **No interface may reach `store` or `jobs`** (invariant 1), and the writers are in `app` and in the job handlers.
- **A stream must not keep the daemon from stopping**, and one slow client must not slow a writer.
- **A palace may be shared** by several daemons (ADR-006), and one daemon cannot hear another's writes in memory.

## Decision

- **The notices are facts, never content.**
  An event has a kind (`job`, `drawer`, `wing`, `room`, `entity`, or `resync`), an action (`created`, `updated`,
  `deleted`) and identifiers: a job's id, kind and status, or a drawer's, wing's, room's or entity's id.
  A burst writer (a mining run, an extraction sweep) says "drawers changed" once per document or pass, without an id.
  No title, text, job input, path or progress line is ever in one.
  A client reads the change through the ordinary routes, which apply the memory mode, so the stream can show nothing a
  read would not.
- **An in-process bus, in a pure module.**
  `events::EventBus` wraps a bounded `tokio::sync::broadcast` channel.
  `server::run` creates one and gives it to the scheduler (job transitions, and every handler's progress through
  `JobContext::checkpoint`, the one place all of them save it) and to `AppServices` (the writes the services make);
  handlers publish their own writes through `JobContext::events`.
  The module reaches no store, no jobs and no network, so `app`, `jobs` and the handlers all depend on it and not on
  each other.
  An event is published after the change is saved, so a client that reads on it finds what it was told about.
  Publishing never fails and never waits.
- **`GET /api/events` is server-sent events on the main listener.**
  It is an ordinary guarded route, so the authentication layer is unchanged (invariant 6): the token is an
  `Authorization` header and never in the address.
  The first frame is `open`; then one frame per event, named by its kind, with the event's JSON as data;
  a comment every 15 seconds keeps a quiet stream from timing out in a proxy.
  The route calls one `AppServices` method, so the interface knows no `store` or `jobs`.
- **Modes: a `disabled` session is refused, a `read_only` one gets the same stream.**
  Subscribing is a read (`403` with the usual diagnostic in `disabled`, never an empty stream),
  and a read-only session may read.
  Every subscriber receives every event, whatever its mode, which is why events carry no content.
- **A slow client is told to read again, not queued.**
  The channel is bounded.
  A connection that falls behind it receives one `resync` frame and then continues, which means "you missed events:
  read everything you show".
  A queue per connection would let one stalled client grow the daemon's memory without limit.
- **The stream ends when the daemon shuts down.**
  Each connection selects on the shutdown token, on the client leaving and on the bus closing,
  so graceful shutdown never waits for an open stream.
- **The dashboard reads the stream with `fetch`, not `EventSource`.**
  `EventSource` cannot send the token or the memory mode.
  `MemCastleClient.events` sends the headers every request carries and parses the frames (`web/src/api/sse.ts`).
  A refusal is handled like any other (a `401` ends the session) and is never retried; a break or silence is retried
  with a growing pause up to 30 seconds.
- **One stream per signed-in dashboard, listened to by the pages that need it.**
  The shell opens it (it is only mounted while signed in), reopens it when the memory mode changes, and closes it when
  the shell goes away.
  `useLoad(load, { on: [kinds] })` re-reads quietly (no spinner) after a burst of events of those kinds has settled for
  250 ms, never has two reads in flight, and re-reads once whenever the stream opens, which covers what happened before
  it was open and while it was down.
- **The Refresh button stays, and the stream is an addition.**
  Without a stream (refused, not yet open, a proxy that buffers it) every page works as ADR-035 describes.
  The shell says whether updates are `Live`, `Connecting` or `Manual`.
- **A daemon only hears its own writes.**
  On a remote palace shared by several daemons, another daemon's changes do not reach this one's bus,
  and the Refresh button is how they show.
- **There is no MCP counterpart.**
  An agent has the tools it needs, and a pushed notice is for a person looking at a page.

## Alternatives rejected

- **A WebSocket.**
  A browser cannot send a header on one, which would force an in-band sign-in like the database endpoint's (ADR-015)
  and a second authentication path to keep correct.
  Nothing here needs the client to talk back.
- **`EventSource` with the token in the query string.**
  The token would be logged and kept in history, which ADR-035 and invariant 11 rule out.
- **SurrealDB live queries.**
  They would also see another daemon's writes on a shared palace, but they tie the stream to the store (the interfaces
  may not reach it), put a subscription on every write path, and would emit the store's records, which are content.
  They are the way to close the shared-palace gap if it matters; a bus in front of them would not need to change.
- **Polling on a timer.**
  A request per page per interval, whether or not anything changed, is what ADR-035 declined.
- **Carrying the changed record in the event.**
  It would save a read, and it would put content on a stream that is checked once, at connect, instead of on every read
  the mode gates.
- **A channel per connection with its own queue.**
  One slow client could grow the daemon's memory without limit; `resync` is bounded and always correct.
- **Publishing from `store`.**
  It would give the lowest layer a dependency on the highest and would announce writes that are not a change anyone sees
  (a lease renewed, a cursor saved).
- **Filtering the stream by mode on the server.**
  There is nothing to filter: every event is an identifier, and `disabled` is refused whole.

## Consequences

- `tests/in_process/events.rs` holds delivery, the identifiers-only rule, the mode gate, authentication and an open
  stream during shutdown; `tests/in_process/auth.rs` lists the route among the guarded ones;
  `web/test` holds the parser, the stream's headers and reconnection, and a page that updates with no timer;
  `web/test/daemon` runs the dashboard's client against a real daemon.
- A job that checkpoints often publishes often, and a mining run publishes one drawer notice per document.
  The channel is bounded, the stream says `resync` when a client falls behind, and the dashboard waits for a burst to
  settle, so the cost is a read per burst, not per event.
- Every writer added later has to publish, or its change shows only on refresh.
  `AppServices::announce` and `JobContext::events` are the two places to do it, and the events are the kinds above.
- The dashboard holds one more long-lived connection to the daemon, and a proxy in front of it has to let a response
  stream (`X-Accel-Buffering: no` for nginx; the daemon sends `Cache-Control: no-cache`).
- On a shared remote palace the dashboard may be behind another daemon's writes until Refresh is pressed.
