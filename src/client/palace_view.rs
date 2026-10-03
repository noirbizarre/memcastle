//! Human renderings of a single wing, room or drawer, and of what a delete
//! would remove.
//!
//! Like [`super::table`] these are only used on a terminal; a pipe gets JSON.

use crate::app::WingDetail;
use crate::domain::{Deleted, Drawer, RoomSummary, WingSummary};
use crate::term::Painter;

use super::table::render_rooms;

/// `Label: value`, the label dimmed so the value is what the eye lands on.
fn field(painter: Painter, label: &str, value: impl std::fmt::Display) -> String {
    format!("{} {value}", painter.dim(&format!("{label}:")))
}

/// What deleting `wing` would remove, one field per line. Shown before the
/// confirmation, so the question is asked with the stakes in view.
#[must_use]
pub fn wing_stakes(wing: &WingSummary, painter: Painter) -> String {
    [
        field(painter, "Wing", &wing.wing.name),
        field(painter, "Rooms", wing.rooms),
        field(painter, "Drawers", wing.drawers),
    ]
    .join("\n")
}

/// What deleting `room` would remove, as [`wing_stakes`].
#[must_use]
pub fn room_stakes(room: &RoomSummary, painter: Painter) -> String {
    [
        field(
            painter,
            "Room",
            format!("{}/{}", room.wing_name, room.room.name),
        ),
        field(painter, "Drawers", room.drawers),
    ]
    .join("\n")
}

/// A wing with its totals and its rooms.
#[must_use]
pub fn render_wing(detail: &WingDetail, painter: Painter, width: Option<u16>) -> String {
    let wing = &detail.wing;
    let mut lines = vec![
        field(painter, "Wing", &wing.wing.name),
        field(painter, "Rooms", wing.rooms),
        field(painter, "Drawers", wing.drawers),
    ];
    if let Some(description) = &wing.wing.description {
        lines.push(field(painter, "Description", description));
    }
    lines.push(field(painter, "Created", wing.wing.created_at.to_rfc3339()));
    if !detail.rooms.is_empty() {
        lines.push(String::new());
        lines.push(render_rooms(&detail.rooms, painter, width));
    }
    lines.join("\n")
}

/// A room with its totals.
#[must_use]
pub fn render_room(room: &RoomSummary, painter: Painter) -> String {
    let mut lines = vec![
        field(
            painter,
            "Room",
            format!("{}/{}", room.wing_name, room.room.name),
        ),
        field(painter, "Drawers", room.drawers),
    ];
    if let Some(description) = &room.room.description {
        lines.push(field(painter, "Description", description));
    }
    lines.push(field(painter, "Created", room.room.created_at.to_rfc3339()));
    lines.join("\n")
}

/// A drawer's metadata, then its content verbatim. The content comes last and
/// untouched so it can be read (or copied) exactly as stored.
#[must_use]
pub fn render_drawer(drawer: &Drawer, painter: Painter) -> String {
    let mut lines = vec![field(painter, "Drawer", drawer.id)];
    if let Some(name) = &drawer.name {
        lines.push(field(painter, "Name", name));
    }
    lines.push(field(painter, "Created", drawer.created_at.to_rfc3339()));
    if let Some(agent) = &drawer.source.agent {
        lines.push(field(painter, "Agent", agent));
    }
    if let Some(uri) = &drawer.source.uri {
        lines.push(field(painter, "Source", uri));
    }
    if !drawer.tags.is_empty() {
        lines.push(field(painter, "Tags", drawer.tags.join(", ")));
    }
    lines.push(String::new());
    lines.push(drawer.content.clone());
    lines.join("\n")
}

/// One line saying what a delete removed.
#[must_use]
pub fn render_deleted(what: &str, deleted: &Deleted, painter: Painter) -> String {
    let mut parts = Vec::new();
    if deleted.rooms > 0 && deleted.wings > 0 {
        parts.push(plural(deleted.rooms, "room"));
    }
    if deleted.drawers > 0 && (deleted.wings > 0 || deleted.rooms > 0) {
        parts.push(plural(deleted.drawers, "drawer"));
    }
    let tail = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    };
    format!("{} {what}{tail}", painter.ok("Deleted"))
}

fn plural(count: u64, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// What deleting the drawer addressed as `path` would remove: which one it is
/// and how much it holds, since an id or a name alone says little.
#[must_use]
pub fn drawer_stakes(path: &str, drawer: &Drawer, painter: Painter) -> String {
    let mut lines = vec![
        field(painter, "Drawer", path),
        field(painter, "Id", drawer.id),
        field(painter, "Characters", drawer.content.chars().count()),
    ];
    if let Some(first) = drawer.content.lines().next() {
        let preview: String = first.chars().take(80).collect();
        lines.push(field(painter, "Starts", preview));
    }
    lines.join("\n")
}

/// One line saying whether a create made something or found it already there.
#[must_use]
pub fn render_created(kind: &str, name: &str, created: bool, painter: Painter) -> String {
    if created {
        format!("{} {kind} {name}", painter.ok("Created"))
    } else {
        format!("{} {kind} {name} already exists", painter.dim("Found"))
    }
}
