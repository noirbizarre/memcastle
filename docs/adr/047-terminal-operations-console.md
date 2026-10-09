# ADR-047: Terminal operations console shares daemon events and controls

## Status

Accepted.

## Context

The web dashboard already reads identifier-only change notices from `GET /api/events` and fetches authoritative state from
REST.
Operators also need a full-screen terminal view of mining jobs, readiness and maintenance without moving business logic
into a CLI process.
Unlike cooperative cancellation, a force-cancel may stop a local worker before it checkpoints; an interrupted request
must remain durable across a daemon restart, and a different daemon's lease must never be overwritten.

## Decision

`memcastle tui` uses ratatui and crossterm for rendering and input, and `client::DaemonClient` for every daemon call.
Its Rust SSE reader uses the web dashboard's existing authenticated endpoint and wire format, never a parallel bus.
Events prompt rereads; reconnect, `resync`, a manual refresh and periodic reconciliation cover gaps in that process-local
stream.

The REST-only `POST /api/jobs/{id}/force-cancel` applies to a locally owned running mining job.
The scheduler first persists `cancel_requested`, then aborts and joins its task.
It applies `JobEvent::Cancel` through `Job::apply` and checks running status and lease owner atomically before answering.
A crash between those steps is recovered as a cancellation, and subsequent fenced writes from the old run are refused.
Blocking work outside the task may continue briefly and previously filed data remains filed; force-cancel does not claim
process isolation or instant termination.
The ordinary pause and cancel routes remain cooperative, preserving checkpoint-friendly behavior.

## Consequences

The terminal and browser share one domain and event model, while each keeps its own presentation and stream reader.
No new storage, credential handling, MCP control or event replay is needed.
An owner on a different daemon cannot be force-cancelled through this daemon; the caller receives a rejection and can ask
for graceful cancellation instead.
