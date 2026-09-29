//! Palace/wing/room repository methods.
//!
//! Every foreign key here (`wing.palace`, `room.wing`) is stored as a plain
//! string, not a SurrealDB record link (`record<table>`). That gives up
//! nothing we use in this bootstrap (no graph traversal across them yet) and
//! means the only place a native `RecordId` value is ever handled is the
//! table's own built-in `id` column, immediately unwrapped via
//! `record::id()` — see the module-level comment in `store::mod`.

use chrono::Utc;

use crate::domain::{Palace, PalaceId, Room, RoomId, Wing, WingId};
use crate::error::Result;

use super::SurrealStore;

/// The projection every palace read shares.
const PALACE_COLUMNS: &str = "record::id(id) AS id, name, <string>created_at AS created_at";

impl SurrealStore {
    /// The palace singleton, or `None` on a brand-new store — the read-only
    /// counterpart of [`Self::get_or_create_palace`], for callers (status,
    /// diary reads) that must not write just to look.
    pub async fn get_palace(&self) -> Result<Option<Palace>> {
        let mut response = self
            .db
            .query(format!("SELECT {PALACE_COLUMNS} FROM palace LIMIT 1"))
            .await?;
        let mut palaces: Vec<Palace> = super::take_rows(&mut response, 0)?;
        Ok(palaces.pop())
    }

    /// The wing named `name`, or `None` — never creates it. See
    /// [`Self::get_or_create_wing`] for the writing form.
    pub async fn get_wing(&self, name: &str) -> Result<Option<Wing>> {
        // No palace yet means no wing can exist; asking is not a reason to
        // create one.
        let Some(palace) = self.get_palace().await? else {
            return Ok(None);
        };
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, palace, name, description, <string>created_at AS created_at \
                 FROM wing WHERE palace = $palace AND name = $name LIMIT 1",
            )
            .bind(("palace", palace.id.to_string()))
            .bind(("name", name.to_string()))
            .await?;
        let mut wings: Vec<Wing> = super::take_rows(&mut response, 0)?;
        Ok(wings.pop())
    }

    /// The room named `name` within `wing`, or `None` — never creates it.
    /// See [`Self::get_or_create_room`] for the writing form.
    pub async fn get_room(&self, wing: WingId, name: &str) -> Result<Option<Room>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, wing, name, description, <string>created_at AS created_at \
                 FROM room WHERE wing = $wing AND name = $name LIMIT 1",
            )
            .bind(("wing", wing.to_string()))
            .bind(("name", name.to_string()))
            .await?;
        let mut rooms: Vec<Room> = super::take_rows(&mut response, 0)?;
        Ok(rooms.pop())
    }

    /// Get the palace singleton, creating it with `default_name` if this is
    /// a brand-new store.
    ///
    /// One daemon serves exactly one palace, and the SurrealDB namespace +
    /// database selection already provides that isolation — this row exists
    /// for display/status purposes (name, creation date), not as a join key
    /// anything else in this bootstrap depends on.
    pub async fn get_or_create_palace(&self, default_name: &str) -> Result<Palace> {
        if let Some(palace) = self.get_palace().await? {
            return Ok(palace);
        }

        let palace = Palace {
            id: PalaceId::new(),
            name: default_name.to_string(),
            created_at: Utc::now(),
        };
        // `.check()` after every write in this module: `.await` alone only
        // reports transport failures, not a rejected statement — see
        // `store::mod`'s module doc.
        self.db
            .query("CREATE type::record('palace', $id) SET name = $name, created_at = <datetime>$created_at")
            .bind(("id", palace.id.to_string()))
            .bind(("name", palace.name.clone()))
            .bind(("created_at", palace.created_at.to_rfc3339()))
            .await?
            .check()?;
        Ok(palace)
    }

    /// Find the wing named `name`, or create it under the palace singleton.
    pub async fn get_or_create_wing(&self, name: &str, description: Option<&str>) -> Result<Wing> {
        let palace = self
            .get_or_create_palace(crate::domain::DEFAULT_PALACE_NAME)
            .await?;

        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, palace, name, description, <string>created_at AS created_at \
                 FROM wing WHERE palace = $palace AND name = $name LIMIT 1",
            )
            .bind(("palace", palace.id.to_string()))
            .bind(("name", name.to_string()))
            .await?;
        let existing: Vec<Wing> = super::take_rows(&mut response, 0)?;
        if let Some(wing) = existing.into_iter().next() {
            return Ok(wing);
        }

        let wing = Wing {
            id: WingId::new(),
            palace: palace.id,
            name: name.to_string(),
            description: description.map(str::to_string),
            created_at: Utc::now(),
        };
        self.db
            .query(
                "CREATE type::record('wing', $id) SET \
                 palace = $palace, name = $name, description = $description, created_at = <datetime>$created_at",
            )
            .bind(("id", wing.id.to_string()))
            .bind(("palace", wing.palace.to_string()))
            .bind(("name", wing.name.clone()))
            .bind(("description", wing.description.clone()))
            .bind(("created_at", wing.created_at.to_rfc3339()))
            .await?
            .check()?;
        Ok(wing)
    }

    /// Find the room named `name` within `wing`, or create it.
    pub async fn get_or_create_room(
        &self,
        wing: WingId,
        name: &str,
        description: Option<&str>,
    ) -> Result<Room> {
        if let Some(room) = self.get_room(wing, name).await? {
            return Ok(room);
        }

        let room = Room {
            id: RoomId::new(),
            wing,
            name: name.to_string(),
            description: description.map(str::to_string),
            created_at: Utc::now(),
        };
        self.db
            .query(
                "CREATE type::record('room', $id) SET \
                 wing = $wing, name = $name, description = $description, created_at = <datetime>$created_at",
            )
            .bind(("id", room.id.to_string()))
            .bind(("wing", room.wing.to_string()))
            .bind(("name", room.name.clone()))
            .bind(("description", room.description.clone()))
            .bind(("created_at", room.created_at.to_rfc3339()))
            .await?
            .check()?;
        Ok(room)
    }

    /// List every wing in the palace.
    pub async fn list_wings(&self) -> Result<Vec<Wing>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, palace, name, description, <string>created_at AS created_at \
                 FROM wing ORDER BY name",
            )
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// List every room in `wing`.
    pub async fn list_rooms(&self, wing: WingId) -> Result<Vec<Room>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, wing, name, description, <string>created_at AS created_at \
                 FROM room WHERE wing = $wing ORDER BY name",
            )
            .bind(("wing", wing.to_string()))
            .await?;
        super::take_rows(&mut response, 0)
    }
}
