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

impl SurrealStore {
    /// Get the palace singleton, creating it with `default_name` if this is
    /// a brand-new store.
    ///
    /// One daemon serves exactly one palace, and the SurrealDB namespace +
    /// database selection already provides that isolation — this row exists
    /// for display/status purposes (name, creation date), not as a join key
    /// anything else in this bootstrap depends on.
    pub async fn get_or_create_palace(&self, default_name: &str) -> Result<Palace> {
        let mut response = self
            .db
            .query("SELECT record::id(id) AS id, name, <string>created_at AS created_at FROM palace LIMIT 1")
            .await?;
        let existing: Vec<Palace> = super::take_rows(&mut response, 0)?;
        if let Some(palace) = existing.into_iter().next() {
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
        let palace = self.get_or_create_palace("default").await?;

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
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, wing, name, description, <string>created_at AS created_at \
                 FROM room WHERE wing = $wing AND name = $name LIMIT 1",
            )
            .bind(("wing", wing.to_string()))
            .bind(("name", name.to_string()))
            .await?;
        let existing: Vec<Room> = super::take_rows(&mut response, 0)?;
        if let Some(room) = existing.into_iter().next() {
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
