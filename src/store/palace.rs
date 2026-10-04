//! Managing the hierarchy itself: lookups by id, counts, listings and
//! cascading deletes.
//!
//! The foreign keys are plain strings with no `ON DELETE` (see `wings`'s
//! module doc), so nothing here is cascaded by the database: a delete removes
//! the children explicitly, in one transaction, because a half-finished cascade
//! would leave exactly the orphans `audit` exists to report.

use std::collections::HashMap;

use serde::Deserialize;

use crate::domain::{
    Deleted, Drawer, DrawerId, DrawerSummary, Room, RoomId, RoomSummary, Wing, WingId, WingSummary,
};
use crate::error::Result;

use super::SurrealStore;
use super::drawers::DRAWER_COLUMNS;

/// How much of a drawer's content a listing previews.
const PREVIEW_CHARS: usize = 120;

/// One row of a `GROUP BY room` count.
#[derive(Deserialize)]
struct RoomCount {
    room: String,
    count: u64,
}

impl SurrealStore {
    /// The wing with this id, or `None`.
    pub async fn get_wing_by_id(&self, id: WingId) -> Result<Option<Wing>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, palace, name, description, <string>created_at AS created_at \
                 FROM wing WHERE id = type::record('wing', $id) LIMIT 1",
            )
            .bind(("id", id.to_string()))
            .await?;
        let mut wings: Vec<Wing> = super::take_rows(&mut response, 0)?;
        Ok(wings.pop())
    }

    /// The room with this id, or `None`.
    pub async fn get_room_by_id(&self, id: RoomId) -> Result<Option<Room>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, wing, name, description, <string>created_at AS created_at \
                 FROM room WHERE id = type::record('room', $id) LIMIT 1",
            )
            .bind(("id", id.to_string()))
            .await?;
        let mut rooms: Vec<Room> = super::take_rows(&mut response, 0)?;
        Ok(rooms.pop())
    }

    /// The drawer with this id, or `None`.
    pub async fn get_drawer(&self, id: DrawerId) -> Result<Option<Drawer>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {DRAWER_COLUMNS} FROM drawer WHERE id = type::record('drawer', $id) LIMIT 1"
            ))
            .bind(("id", id.to_string()))
            .await?;
        let mut drawers: Vec<Drawer> = super::take_rows(&mut response, 0)?;
        Ok(drawers.pop())
    }

    /// The drawer named `name` in `room`, or `None`.
    pub async fn get_drawer_by_name(&self, room: RoomId, name: &str) -> Result<Option<Drawer>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {DRAWER_COLUMNS} FROM drawer WHERE room = $room AND name = $name LIMIT 1"
            ))
            .bind(("room", room.to_string()))
            .bind(("name", name.to_string()))
            .await?;
        let mut drawers: Vec<Drawer> = super::take_rows(&mut response, 0)?;
        Ok(drawers.pop())
    }

    /// Every room in the palace, across wings, ordered by name.
    async fn list_all_rooms(&self) -> Result<Vec<Room>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, wing, name, description, <string>created_at AS created_at \
                 FROM room ORDER BY name",
            )
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// How many drawers each room holds, for the given rooms or (with `None`)
    /// every room. A room with no drawers has no entry.
    async fn drawer_counts_by_room(
        &self,
        rooms: Option<Vec<String>>,
    ) -> Result<HashMap<String, u64>> {
        let mut response = self
            .db
            .query(
                // Grouped in the database so the drawers themselves (and their
                // content) never cross the wire just to be counted.
                "SELECT room, count() AS count FROM drawer \
                 WHERE $rooms = NULL OR room IN $rooms GROUP BY room",
            )
            // `bindable`: a bare `None` would bind as `NONE`, never `= NULL`.
            .bind(("rooms", super::bindable(&rooms)?))
            .await?;
        let rows: Vec<RoomCount> = super::take_rows(&mut response, 0)?;
        Ok(rows.into_iter().map(|row| (row.room, row.count)).collect())
    }

    /// How many drawers `room` holds.
    pub async fn count_room_drawers(&self, room: RoomId) -> Result<u64> {
        let counts = self
            .drawer_counts_by_room(Some(vec![room.to_string()]))
            .await?;
        Ok(counts.get(&room.to_string()).copied().unwrap_or(0))
    }

    /// Every wing with its room and drawer counts, ordered by name.
    pub async fn list_wing_summaries(&self) -> Result<Vec<WingSummary>> {
        let wings = self.list_wings().await?;
        let rooms = self.list_all_rooms().await?;
        let drawers = self.drawer_counts_by_room(None).await?;
        Ok(wings
            .into_iter()
            .map(|wing| summarise_wing(wing, &rooms, &drawers))
            .collect())
    }

    /// One wing with its room and drawer counts.
    pub async fn wing_summary(&self, wing: Wing) -> Result<WingSummary> {
        let rooms = self.list_rooms(wing.id).await?;
        let ids = rooms.iter().map(|room| room.id.to_string()).collect();
        let drawers = self.drawer_counts_by_room(Some(ids)).await?;
        Ok(summarise_wing(wing, &rooms, &drawers))
    }

    /// The rooms of `wing` with their drawer counts, ordered by name.
    pub async fn list_room_summaries(&self, wing: &Wing) -> Result<Vec<RoomSummary>> {
        let rooms = self.list_rooms(wing.id).await?;
        let ids = rooms.iter().map(|room| room.id.to_string()).collect();
        let drawers = self.drawer_counts_by_room(Some(ids)).await?;
        Ok(rooms
            .into_iter()
            .map(|room| {
                let count = drawers.get(&room.id.to_string()).copied().unwrap_or(0);
                RoomSummary {
                    room,
                    wing_name: wing.name.clone(),
                    drawers: count,
                }
            })
            .collect())
    }

    /// One room with its drawer count.
    pub async fn room_summary(&self, wing: &Wing, room: Room) -> Result<RoomSummary> {
        let drawers = self.count_room_drawers(room.id).await?;
        Ok(RoomSummary {
            room,
            wing_name: wing.name.clone(),
            drawers,
        })
    }

    /// The newest `limit` drawers of `room` as listing rows: no embedding and
    /// only a preview of the content.
    pub async fn list_drawer_summaries(
        &self,
        room: RoomId,
        limit: u32,
    ) -> Result<Vec<DrawerSummary>> {
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, room, name, string::len(content) AS chars, \
                        string::slice(content, 0, $preview) AS preview, source, \
                        <string>created_at AS created_at \
                 FROM drawer WHERE room = $room ORDER BY created_at DESC LIMIT $limit",
            )
            .bind(("room", room.to_string()))
            .bind(("preview", PREVIEW_CHARS as u64))
            .bind(("limit", limit))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// Delete one room and every drawer in it, returning what was removed.
    ///
    /// The counts are read just before the delete, so a drawer written in
    /// between is removed but not counted: the delete is atomic, the report is
    /// informational.
    pub async fn delete_room(&self, room: RoomId) -> Result<Deleted> {
        let drawers = self.count_room_drawers(room).await?;
        super::retrying_on_conflict(|| async {
            let response = self
                .db
                .query(
                    // One transaction: a failure between the two statements
                    // would otherwise leave a room's drawers orphaned.
                    "BEGIN TRANSACTION; \
                     DELETE drawer WHERE room = $room; \
                     DELETE type::record('room', $room); \
                     COMMIT TRANSACTION;",
                )
                .bind(("room", room.to_string()))
                .await?;
            // `.await` alone only reports transport failures. `checked`, not
            // `.check()`: a conflicted transaction's first error is
            // `NotExecuted`, which would hide the conflict from the retry.
            super::checked(response)?;
            Ok(())
        })
        .await?;
        Ok(Deleted {
            wings: 0,
            rooms: 1,
            drawers,
        })
    }

    /// Delete a wing, its rooms and their drawers, returning what was removed.
    /// Same atomicity and counting as [`Self::delete_room`].
    pub async fn delete_wing(&self, wing: WingId) -> Result<Deleted> {
        let rooms = self.list_rooms(wing).await?;
        let ids: Vec<String> = rooms.iter().map(|room| room.id.to_string()).collect();
        let drawers: u64 = self.drawer_counts_by_room(Some(ids)).await?.values().sum();
        super::retrying_on_conflict(|| async {
            let response = self
                .db
                .query(
                    // Children first, in one transaction, so the hierarchy is
                    // never observable with a parent gone and children left.
                    "BEGIN TRANSACTION; \
                     DELETE drawer WHERE room IN \
                         (SELECT VALUE record::id(id) FROM room WHERE wing = $wing); \
                     DELETE room WHERE wing = $wing; \
                     DELETE type::record('wing', $wing); \
                     COMMIT TRANSACTION;",
                )
                .bind(("wing", wing.to_string()))
                .await?;
            // `checked`: see `delete_room`.
            super::checked(response)?;
            Ok(())
        })
        .await?;
        Ok(Deleted {
            wings: 1,
            rooms: rooms.len() as u64,
            drawers,
        })
    }
}

/// A wing with the counts of the `rooms` it owns, given each room's drawer
/// count in `drawers`.
fn summarise_wing(wing: Wing, rooms: &[Room], drawers: &HashMap<String, u64>) -> WingSummary {
    let owned = rooms.iter().filter(|room| room.wing == wing.id);
    let (room_count, drawer_count) = owned.fold((0, 0), |(rooms, total), room| {
        (
            rooms + 1,
            total + drawers.get(&room.id.to_string()).copied().unwrap_or(0),
        )
    });
    WingSummary {
        wing,
        rooms: room_count,
        drawers: drawer_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Provenance, Source, SourceKind};

    fn drawer(room: RoomId, content: &str, name: Option<&str>) -> Drawer {
        Drawer::new(
            DrawerId::new(),
            room,
            content.to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        )
        .with_name(name.map(str::to_string))
    }

    async fn seeded() -> (SurrealStore, Wing, Room, Room) {
        let store = SurrealStore::connect_memory_for_tests().await;
        let wing = store.get_or_create_wing("work", None).await.unwrap();
        let a = store.get_or_create_room(wing.id, "a", None).await.unwrap();
        let b = store.get_or_create_room(wing.id, "b", None).await.unwrap();
        store
            .create_drawer(&drawer(a.id, "one", None))
            .await
            .unwrap();
        store
            .create_drawer(&drawer(a.id, "two", None))
            .await
            .unwrap();
        store
            .create_drawer(&drawer(b.id, "three", Some("t")))
            .await
            .unwrap();
        (store, wing, a, b)
    }

    #[tokio::test]
    async fn any_number_of_unnamed_drawers_can_share_a_room() {
        // The unique (room, name) index must treat "no name" as no constraint,
        // or the second unnamed drawer in a room would be rejected.
        let (store, _, a, _) = seeded().await;
        assert_eq!(store.count_room_drawers(a.id).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_drawer_name_is_unique_within_its_room_but_reusable_in_another() {
        let (store, _, a, b) = seeded().await;
        let clash = store.create_drawer(&drawer(b.id, "other", Some("t"))).await;
        assert!(
            clash.is_err(),
            "a second `t` in the same room must be refused"
        );
        store
            .create_drawer(&drawer(a.id, "other", Some("t")))
            .await
            .expect("the same name in another room is fine");
    }

    #[tokio::test]
    async fn a_drawer_is_found_by_name_and_by_id() {
        let (store, _, _, b) = seeded().await;
        let by_name = store.get_drawer_by_name(b.id, "t").await.unwrap().unwrap();
        assert_eq!(by_name.content, "three");
        let by_id = store.get_drawer(by_name.id).await.unwrap().unwrap();
        assert_eq!(by_id.name.as_deref(), Some("t"));
        assert!(
            store
                .get_drawer_by_name(b.id, "nope")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn wing_summaries_count_rooms_and_the_drawers_in_them() {
        let (store, _, _, _) = seeded().await;
        let other = store.get_or_create_wing("empty", None).await.unwrap();
        let summaries = store.list_wing_summaries().await.unwrap();
        let work = summaries.iter().find(|s| s.wing.name == "work").unwrap();
        assert_eq!((work.rooms, work.drawers), (2, 3));
        let empty = summaries.iter().find(|s| s.wing.id == other.id).unwrap();
        assert_eq!((empty.rooms, empty.drawers), (0, 0));
    }

    #[tokio::test]
    async fn a_drawer_listing_previews_content_instead_of_carrying_it() {
        let (store, _, a, _) = seeded().await;
        let long = "x".repeat(500);
        store
            .create_drawer(&drawer(a.id, &long, None))
            .await
            .unwrap();
        let rows = store.list_drawer_summaries(a.id, 10).await.unwrap();
        let big = rows.iter().find(|row| row.chars == 500).unwrap();
        assert_eq!(big.preview.chars().count(), PREVIEW_CHARS);
    }

    #[tokio::test]
    async fn deleting_a_room_removes_its_drawers_and_nothing_else() {
        let (store, wing, a, b) = seeded().await;
        let deleted = store.delete_room(a.id).await.unwrap();
        assert_eq!(
            deleted,
            Deleted {
                wings: 0,
                rooms: 1,
                drawers: 2
            }
        );
        assert!(store.get_room_by_id(a.id).await.unwrap().is_none());
        assert_eq!(store.count_room_drawers(b.id).await.unwrap(), 1);
        assert!(store.get_wing_by_id(wing.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn deleting_a_wing_leaves_no_orphan_rooms_or_drawers() {
        let (store, wing, _, _) = seeded().await;
        let keeper = store.get_or_create_wing("keep", None).await.unwrap();
        let room = store
            .get_or_create_room(keeper.id, "r", None)
            .await
            .unwrap();
        store
            .create_drawer(&drawer(room.id, "mine", None))
            .await
            .unwrap();

        let deleted = store.delete_wing(wing.id).await.unwrap();

        assert_eq!(
            deleted,
            Deleted {
                wings: 1,
                rooms: 2,
                drawers: 3
            }
        );
        assert!(store.get_wing_by_id(wing.id).await.unwrap().is_none());
        assert_eq!(
            store.count_drawers().await.unwrap(),
            1,
            "only the other wing's drawer remains"
        );
        assert_eq!(store.list_all_rooms().await.unwrap().len(), 1);
    }
}
