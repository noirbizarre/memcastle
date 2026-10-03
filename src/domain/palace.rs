//! The palace hierarchy: `Palace -> Wing -> Room`.
//!
//! A drawer's actual content lives in [`super::Drawer`]; these three types
//! are purely the navigational taxonomy it is filed under.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{PalaceId, RoomId, WingId};

/// The name a palace has before anyone has named it: what the store creates
/// it under, and what a read-only `status` reports until a write has created
/// the record — one constant so the name cannot change when the first write
/// happens.
pub const DEFAULT_PALACE_NAME: &str = "default";

/// The root of one memory store. One daemon serves exactly one palace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Palace {
    /// Unique identifier.
    pub id: PalaceId,
    /// Human-readable name.
    pub name: String,
    /// When this palace was first created.
    pub created_at: DateTime<Utc>,
}

/// A top-level namespace — typically one project or one conversation source.
///
/// MemPalace's own history is the reason this stays a thin bucket rather
/// than a rigid schema: an early ingest mode that didn't key wings by real
/// source identity ended up dumping everything into one mega-wing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wing {
    /// Unique identifier.
    pub id: WingId,
    /// The palace this wing belongs to.
    pub palace: PalaceId,
    /// Human-readable name.
    pub name: String,
    /// Optional free-text description.
    pub description: Option<String>,
    /// When this wing was created.
    pub created_at: DateTime<Utc>,
}

/// A topical sub-bucket within a [`Wing`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    /// Unique identifier.
    pub id: RoomId,
    /// The wing this room belongs to.
    pub wing: WingId,
    /// Human-readable name.
    pub name: String,
    /// Optional free-text description.
    pub description: Option<String>,
    /// When this room was created.
    pub created_at: DateTime<Utc>,
}

/// A [`Wing`] with what it holds, for listings and the delete summary.
///
/// The counts are computed when asked for, never stored: a stored count would
/// need updating by every writer (mining, checkpoint, diary, repair) and would
/// drift the first time one forgot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WingSummary {
    /// The wing itself.
    #[serde(flatten)]
    pub wing: Wing,
    /// How many rooms it has.
    pub rooms: u64,
    /// How many drawers are filed in those rooms.
    pub drawers: u64,
}

/// A [`Room`] with what it holds, and the name of the wing it is in so a
/// listing across wings can say where each room lives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSummary {
    /// The room itself.
    #[serde(flatten)]
    pub room: Room,
    /// The name of the wing the room belongs to.
    pub wing_name: String,
    /// How many drawers are filed in it.
    pub drawers: u64,
}

/// A drawer as a listing shows it: enough to recognise and address it,
/// without the content (which can be large) or the embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawerSummary {
    /// Unique identifier.
    pub id: super::DrawerId,
    /// The room it is filed under.
    pub room: RoomId,
    /// Its name within the room, if it has one.
    #[serde(default)]
    pub name: Option<String>,
    /// How many characters of content it holds.
    pub chars: u64,
    /// The start of the content, for recognising the drawer.
    pub preview: String,
    /// Where the content came from.
    pub source: super::Source,
    /// When it was written.
    pub created_at: DateTime<Utc>,
}

/// What a delete removed, counted from the palace as it was just before.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deleted {
    /// Wings removed (0 or 1).
    pub wings: u64,
    /// Rooms removed, including those of a deleted wing.
    pub rooms: u64,
    /// Drawers removed, including those of a deleted wing or room.
    pub drawers: u64,
}
