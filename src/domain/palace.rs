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
