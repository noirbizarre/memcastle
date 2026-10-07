//! Change notifications: "something changed", never what it holds.
//!
//! The scheduler and the writers publish an [`Event`] to the daemon's [`EventBus`] after a change is saved, and
//! `GET /api/events` relays them to clients, so a dashboard can re-read instead of polling.
//! An event carries a kind, an action and identifiers only (a job's id, kind and status; a drawer's, wing's, room's or
//! entity's id): no title, text, job input or progress message.
//! A client re-reads through the normal routes, which apply the memory mode, so the stream can never show what a
//! read would refuse (see `docs/adr/041-server-sent-events-for-dashboard-updates.md`).
//!
//! The bus lives in this process only.
//! Another daemon sharing a remote palace publishes to its own bus, which is why a client keeps a manual refresh.
//!
//! This module is pure: it reaches no store, no jobs and no network, so `app`, `jobs` and the handlers can all depend
//! on it without depending on each other.

use std::fmt::Display;

use serde::Serialize;
use tokio::sync::broadcast;

/// How many events a slow subscriber may fall behind by before it is told to re-read everything.
///
/// Bounded on purpose: an unbounded queue per connection lets one stalled client grow the daemon's memory without
/// limit, and a mining run can publish thousands of drawer events a minute.
pub const CAPACITY: usize = 256;

/// What an event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A job's status or progress changed.
    Job,
    /// A drawer was written, superseded, changed or deleted.
    Drawer,
    /// A wing was created or deleted.
    Wing,
    /// A room was created or deleted.
    Room,
    /// An entity, its aliases or its links changed.
    Entity,
    /// A trigger fired, failed or changed (docs/adr/043).
    Trigger,
    /// Events were missed: re-read everything.
    Resync,
}

impl EventKind {
    /// The name this kind goes by on the wire: the SSE event name and the `kind` field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Job => "job",
            Self::Drawer => "drawer",
            Self::Wing => "wing",
            Self::Room => "room",
            Self::Entity => "entity",
            Self::Trigger => "trigger",
            Self::Resync => "resync",
        }
    }
}

/// What happened to the thing an event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// It now exists.
    Created,
    /// It changed (a job's status or progress, a drawer superseded).
    Updated,
    /// It is gone.
    Deleted,
}

/// One "something changed" notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    /// What the event is about.
    pub kind: EventKind,
    /// What happened to it.
    pub action: Action,
    /// The identifier of the thing, when the event is about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// For a job: its kind (`mine`, `audit`, ...), a fixed word that never carries parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_kind: Option<String>,
    /// For a job: its status (`running`, `completed`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl Event {
    fn about(kind: EventKind, action: Action, id: impl Display) -> Self {
        Self {
            kind,
            action,
            id: Some(id.to_string()),
            job_kind: None,
            status: None,
        }
    }

    /// A job was queued, changed status or made progress.
    #[must_use]
    pub fn job(action: Action, id: impl Display, job_kind: &str, status: impl Display) -> Self {
        Self {
            job_kind: Some(job_kind.to_owned()),
            status: Some(status.to_string()),
            ..Self::about(EventKind::Job, action, id)
        }
    }

    /// A drawer changed.
    #[must_use]
    pub fn drawer(action: Action, id: impl Display) -> Self {
        Self::about(EventKind::Drawer, action, id)
    }

    /// Drawers changed, without saying which: for a writer that stores many at once (a mining run publishes one per
    /// document, not one per chunk).
    #[must_use]
    pub fn drawers_changed() -> Self {
        Self {
            kind: EventKind::Drawer,
            action: Action::Updated,
            id: None,
            job_kind: None,
            status: None,
        }
    }

    /// A wing changed.
    #[must_use]
    pub fn wing(action: Action, id: impl Display) -> Self {
        Self::about(EventKind::Wing, action, id)
    }

    /// A room changed.
    #[must_use]
    pub fn room(action: Action, id: impl Display) -> Self {
        Self::about(EventKind::Room, action, id)
    }

    /// An entity, an alias or a link changed.
    #[must_use]
    pub fn entity(action: Action, id: impl Display) -> Self {
        Self::about(EventKind::Entity, action, id)
    }

    /// Entities or relationships changed, without saying which: for a sweep that writes many at once.
    #[must_use]
    pub fn entity_graph_changed() -> Self {
        Self {
            kind: EventKind::Entity,
            action: Action::Updated,
            id: None,
            job_kind: None,
            status: None,
        }
    }

    /// A trigger fired, failed, was defined or removed. `status` is a fixed word (`queued`, `coalesced`, `failed`,
    /// `changed`), never a payload, a path or an error message: a client re-reads the trigger for those.
    #[must_use]
    pub fn trigger(action: Action, name: impl Display, status: &str) -> Self {
        Self {
            status: Some(status.to_owned()),
            ..Self::about(EventKind::Trigger, action, name)
        }
    }

    /// The subscriber missed events and must re-read everything.
    #[must_use]
    pub fn resync() -> Self {
        Self {
            kind: EventKind::Resync,
            action: Action::Updated,
            id: None,
            job_kind: None,
            status: None,
        }
    }
}

/// The daemon's event source. Cheap to clone; every clone publishes to, and subscribes from, the same channel.
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}

impl EventBus {
    /// A bus holding up to [`CAPACITY`] events per subscriber.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(CAPACITY)
    }

    /// A bus holding up to `capacity` events per subscriber (at least one).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// Tell every subscriber about a change that has already been saved.
    ///
    /// Never fails and never waits: a change must not fail because nobody listens (the usual case), and a slow
    /// subscriber must never slow a writer.
    pub fn publish(&self, event: Event) {
        // `send` errors only when there are no receivers, which is not a problem.
        let _ = self.sender.send(event);
    }

    /// Start receiving events published from now on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};

    #[test]
    fn publishing_with_no_subscriber_is_not_an_error() {
        EventBus::new().publish(Event::drawer(Action::Created, "d1"));
    }

    #[test]
    fn every_subscriber_receives_every_event() {
        let bus = EventBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        bus.publish(Event::wing(Action::Created, "w1"));
        assert_eq!(
            first.try_recv().unwrap(),
            Event::wing(Action::Created, "w1")
        );
        assert_eq!(
            second.try_recv().unwrap(),
            Event::wing(Action::Created, "w1")
        );
        assert!(matches!(first.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn a_subscriber_sees_nothing_published_before_it_subscribed() {
        let bus = EventBus::new();
        bus.publish(Event::room(Action::Created, "r1"));
        let mut late = bus.subscribe();
        assert!(matches!(late.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn a_subscriber_that_falls_behind_is_told_it_lagged() {
        let bus = EventBus::with_capacity(2);
        let mut slow = bus.subscribe();
        for n in 0..5 {
            bus.publish(Event::drawer(Action::Created, n));
        }
        assert!(matches!(slow.recv().await, Err(RecvError::Lagged(_))));
    }

    #[test]
    fn a_job_event_serialises_ids_and_words_only() {
        let event = Event::job(Action::Updated, "j1", "mine", "running");
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "kind": "job",
                "action": "updated",
                "id": "j1",
                "job_kind": "mine",
                "status": "running",
            })
        );
    }

    #[test]
    fn a_drawer_event_omits_the_job_fields() {
        assert_eq!(
            serde_json::to_value(Event::drawer(Action::Deleted, "d1")).unwrap(),
            serde_json::json!({ "kind": "drawer", "action": "deleted", "id": "d1" })
        );
    }

    #[test]
    fn a_resync_event_names_no_identifier() {
        assert_eq!(
            serde_json::to_value(Event::resync()).unwrap(),
            serde_json::json!({ "kind": "resync", "action": "updated" })
        );
    }

    #[test]
    fn the_wire_name_of_a_kind_matches_its_serialised_form() {
        for kind in [
            EventKind::Job,
            EventKind::Drawer,
            EventKind::Wing,
            EventKind::Room,
            EventKind::Entity,
            EventKind::Trigger,
            EventKind::Resync,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }
}
