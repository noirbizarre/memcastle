//! The event stream: `GET /api/events`, server-sent events (docs/adr/041).
//!
//! A relay and nothing more: it carries the identifiers `app` publishes ("a job changed", "a drawer was written")
//! and never a title, a text or a job's input, so a client that wants the change re-reads through the routes that
//! apply the memory mode. It sits behind the authentication layer like every route but health, which is why the
//! dashboard reads it with `fetch` and an `Authorization` header: a browser `EventSource` cannot send one, and a
//! token never goes in a URL. There is no MCP counterpart, since an agent has the tools it needs.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;

use crate::events::Event;

use super::{ApiError, ApiState, ModeHeader};

/// How often a comment is sent when nothing else is, so a proxy or a client timeout does not close a quiet stream.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// How many frames may wait for one slow connection before the relay stops reading the bus for it.
///
/// Small on purpose: the bus already bounds what a subscriber may miss, and a connection that cannot keep up is
/// better told to re-read everything (`resync`) than allowed to queue.
const BACKLOG: usize = 64;

/// `GET /api/events`
///
/// The first frame is an `open` event, so a client knows the stream is live before anything has happened.
/// Then one frame per change, named by its kind (`job`, `drawer`, `wing`, `room`, `entity`) with the JSON of the
/// [`Event`] as data. When the connection falls behind the bus, a `resync` frame says that events were missed.
/// The stream ends when the daemon shuts down.
pub(super) async fn events(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    // Refused here, with the same 403 every read gets, so a `disabled` session learns nothing from the stream.
    let mut receiver = state.app.subscribe_events(mode)?;
    let shutdown = state.shutdown.clone();
    let (frames, queue) = tokio::sync::mpsc::channel::<Result<SseEvent, Infallible>>(BACKLOG);

    // A task per connection rather than a hand-written `Stream`: it ends on whichever comes first of a daemon
    // shutdown (so graceful shutdown never waits on an open stream), the client leaving (`closed`), or the bus
    // going away.
    tokio::spawn(async move {
        if frames
            .send(Ok(SseEvent::default().event("open").data("{}")))
            .await
            .is_err()
        {
            return;
        }
        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return,
                () = frames.closed() => return,
                received = receiver.recv() => match received {
                    Ok(event) => event,
                    // The relay fell behind the bus: say so, and let the client re-read what it cares about.
                    Err(RecvError::Lagged(_)) => Event::resync(),
                    Err(RecvError::Closed) => return,
                },
            };
            if frames.send(Ok(frame(&event))).await.is_err() {
                return;
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(queue)).keep_alive(KeepAlive::new().interval(KEEP_ALIVE)))
}

/// One event as an SSE frame: named by its kind, with the event's JSON as data.
fn frame(event: &Event) -> SseEvent {
    // An `Event` is plain strings and enums, so serialising cannot fail; the empty object is only a fallback that
    // keeps the frame well formed.
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    SseEvent::default().event(event.kind.as_str()).data(data)
}
