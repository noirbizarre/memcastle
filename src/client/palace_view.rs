//! Human renderings of a single wing, room or drawer, and of what a delete
//! would remove.
//!
//! Like [`super::table`] these are only used when the output is pretty (a terminal without `--json`);
//! anything else gets JSON.

use crate::app::{EntityLink, Superseded, WingDetail};
use crate::domain::{Deleted, Drawer, DrawerHistory, RoomSummary, WingSummary};
use crate::term::Painter;

use super::table::render_rooms;

/// `Label: value`, the label dimmed so the value is what the eye lands on.
pub(super) fn field(painter: Painter, label: &str, value: impl std::fmt::Display) -> String {
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
    lines.push(field(painter, "Valid", validity(drawer)));
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

/// The validity of a drawer as `from -> to`, with `now` for an open end, so a
/// version's place in the chain reads at a glance.
fn validity(drawer: &Drawer) -> String {
    let end = drawer
        .valid_to
        .map_or_else(|| "now".to_string(), |end| end.to_rfc3339());
    format!("{} -> {end}", drawer.valid_from.to_rfc3339())
}

/// How a piece of knowledge evolved: one block per version, oldest first, each
/// with its validity period, provenance and content. The recorded time is
/// shown beside the validity because the two differ whenever something was
/// learned after the fact.
#[must_use]
pub fn render_history(history: &DrawerHistory, painter: Painter) -> String {
    let total = history.versions.len();
    let blocks: Vec<String> = history
        .versions
        .iter()
        .enumerate()
        .map(|(index, version)| {
            let current = version.valid_to.is_none();
            let state = if current {
                painter.ok("current")
            } else {
                painter.dim("superseded")
            };
            let mut lines = vec![format!(
                "{} {} {state}",
                painter.dim(&format!("Version {}/{total}", index + 1)),
                version.id
            )];
            lines.push(field(painter, "Valid", validity(version)));
            lines.push(field(painter, "Recorded", version.created_at.to_rfc3339()));
            if let Some(agent) = &version.source.agent {
                lines.push(field(painter, "Agent", agent));
            }
            if let Some(uri) = &version.source.uri {
                lines.push(field(painter, "Source", uri));
            }
            lines.push(String::new());
            lines.push(version.content.clone());
            lines.join("\n")
        })
        .collect();
    blocks.join("\n\n")
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

/// One line confirming a note: its stable id and where it was filed, or that the same note was already there.
#[must_use]
pub fn render_note(
    drawer: &Drawer,
    created: bool,
    wing: &str,
    room: &str,
    painter: Painter,
) -> String {
    if created {
        format!(
            "{} note {} in {wing}/{room}",
            painter.ok("Saved"),
            drawer.id
        )
    } else {
        format!(
            "{} note {} in {wing}/{room}, it was already captured",
            painter.dim("Found"),
            drawer.id
        )
    }
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

/// One line confirming a diary entry: its id, so it can be found again.
#[must_use]
pub fn render_diary_written(drawer: &Drawer, painter: Painter) -> String {
    format!("{} diary entry {}", painter.ok("Saved"), drawer.id)
}

/// One or two lines saying which drawer stopped being current and what, if anything, took its place.
#[must_use]
pub fn render_superseded(outcome: &Superseded, painter: Painter) -> String {
    match &outcome.replacement {
        Some(replacement) => format!(
            "{} drawer {}\n{} drawer {}",
            painter.ok("Superseded"),
            outcome.superseded.id,
            painter.dim("Replaced by"),
            replacement.id,
        ),
        // No replacement: the drawer is simply no longer current, which is not a delete.
        None => format!(
            "{} drawer {} (its history is kept)",
            painter.ok("Invalidated"),
            outcome.superseded.id
        ),
    }
}

/// One line saying that a drawer now mentions an entity, or already did.
#[must_use]
pub fn render_entity_link(link: &EntityLink, painter: Painter) -> String {
    let entity = format!("{} {}", link.entity.kind, link.entity.name);
    if link.created {
        format!(
            "{} drawer {} to {entity}",
            painter.ok("Linked"),
            link.drawer
        )
    } else {
        format!(
            "{} drawer {} was already linked to {entity}",
            painter.dim("Found"),
            link.drawer
        )
    }
}

/// One line saying whether revoking found a token to revoke.
#[must_use]
pub fn render_revoked(revoked: bool, painter: Painter) -> String {
    if revoked {
        format!("{} the generated token", painter.ok("Revoked"))
    } else {
        // Not an error: the end state the user asked for already holds.
        format!("{} no generated token to revoke", painter.dim("Found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DrawerId, Provenance, RoomId, Source, SourceKind};

    fn drawer() -> Drawer {
        Drawer::new(
            DrawerId::new(),
            RoomId::new(),
            "body".to_string(),
            Source {
                kind: SourceKind::Note,
                uri: None,
                agent: None,
                origin: None,
            },
            Vec::new(),
            Provenance {
                requested_by: "cli".to_string(),
                job_id: None,
            },
        )
    }

    #[test]
    fn a_saved_diary_entry_is_confirmed_by_its_id() {
        let entry = drawer();
        assert_eq!(
            render_diary_written(&entry, Painter::PLAIN),
            format!("Saved diary entry {}", entry.id)
        );
    }

    #[test]
    fn a_supersession_names_the_replacement_and_an_invalidation_says_nothing_was_deleted() {
        let (old, new) = (drawer(), drawer());
        let replaced = Superseded {
            superseded: old.clone(),
            replacement: Some(new.clone()),
        };
        let text = render_superseded(&replaced, Painter::PLAIN);
        assert!(
            text.contains(&old.id.to_string()) && text.contains(&new.id.to_string()),
            "{text}"
        );

        let invalidated = Superseded {
            superseded: old,
            replacement: None,
        };
        let text = render_superseded(&invalidated, Painter::PLAIN);
        assert!(text.starts_with("Invalidated drawer"), "{text}");
        assert!(text.contains("history is kept"), "{text}");
    }

    #[test]
    fn revoking_with_no_token_is_reported_as_already_done_not_as_a_revocation() {
        assert_eq!(
            render_revoked(true, Painter::PLAIN),
            "Revoked the generated token"
        );
        assert!(render_revoked(false, Painter::PLAIN).contains("no generated token"));
    }
}
