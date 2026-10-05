//! Managing the palace hierarchy: listing, showing, creating and deleting
//! wings, rooms and drawers.
//!
//! Wings and rooms are addressed by name or by UUID, a drawer by name or UUID
//! within its room (see [`crate::domain::PalacePath`]). Names win over ids, and
//! names that look like UUIDs are refused at creation, so the two can never
//! collide.
//!
//! Gating follows ADR-007: names and counts are palace content, so listing and
//! showing are **reads**, and creating and deleting are **writes**. There is no
//! MCP tool for any of it: deleting a wing is a human decision.

use serde::{Deserialize, Serialize};

use crate::domain::{
    Deleted, Drawer, DrawerId, DrawerSummary, JobKind, JobStatus, MemoryMode, NameKind, Provenance,
    Room, RoomId, RoomSummary, Source, SourceKind, Wing, WingId, WingSummary, validate_name,
};
use crate::error::{Error, Result};

use super::{AppServices, MAX_READ_LIMIT};

/// How many drawers a listing returns when the caller does not say.
pub const DEFAULT_LIST_LIMIT: u32 = 50;

/// The result of a `create`, which is idempotent for wings and rooms:
/// `created` says whether this call made the record or found it already there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Created<T> {
    /// `true` if this call created the record, `false` if it already existed.
    pub created: bool,
    /// The record, as it stands now.
    #[serde(flatten)]
    pub item: T,
}

/// A wing with its rooms, for `wing show`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WingDetail {
    /// The wing and its totals.
    pub wing: WingSummary,
    /// Its rooms, by name.
    pub rooms: Vec<RoomSummary>,
}

/// The corrected content a superseded drawer is replaced by.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawerReplacement {
    /// The new content. Required and non-blank: a replacement is a new memory.
    pub content: String,
    /// Tags for the replacement; the superseded drawer's own when absent.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// What linking a drawer to an entity did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityLink {
    /// The drawer that mentions the entity.
    pub drawer: DrawerId,
    /// The entity, created if it did not exist.
    pub entity: crate::domain::Entity,
    /// Whether this call made the link (`false`: it was already there).
    pub created: bool,
}

/// What superseding a drawer did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Superseded {
    /// The drawer whose validity now ends, content untouched.
    pub superseded: Drawer,
    /// The drawer that took its place, absent for a plain invalidation.
    pub replacement: Option<Drawer>,
}

impl AppServices {
    /// End a drawer's validity now and, when `replacement` is given, open a
    /// corrected drawer from the same instant.
    ///
    /// Drawer content is immutable, so a correction is a new drawer plus a
    /// closed old one: nothing is rewritten, a point-in-time search still sees
    /// what was believed then, and the default (current) search sees only the
    /// replacement. The replacement is filed in the same room, inherits the
    /// old drawer's name (the old one gives it up; it stays addressable by id)
    /// and, unless `replacement.tags` says otherwise, its tags. Without a
    /// replacement the drawer is simply no longer current.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits writes,
    /// [`Error::DrawerNotFound`], [`Error::DrawerSuperseded`] if it already
    /// ended, [`Error::InvalidInput`] for blank replacement content, or a
    /// store error.
    pub async fn supersede_drawer(
        &self,
        id: DrawerId,
        replacement: Option<DrawerReplacement>,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Superseded> {
        Self::require_write(mode, "drawer_supersede")?;
        if let Some(replacement) = &replacement
            && replacement.content.trim().is_empty()
        {
            return Err(Error::invalid_input(
                "content",
                "must not be empty; omit it to invalidate the drawer without a replacement",
            ));
        }
        let old = self
            .store
            .get_drawer(id)
            .await?
            .ok_or_else(|| Error::DrawerNotFound {
                room: "-".to_string(),
                drawer: id.to_string(),
            })?;
        let superseded = || Error::DrawerSuperseded {
            drawer: id.to_string(),
        };
        if old.valid_to.is_some() {
            return Err(superseded());
        }

        let at = chrono::Utc::now();
        let new = replacement.map(|replacement| {
            let mut drawer = Drawer::new(
                DrawerId::new(),
                old.room,
                replacement.content,
                Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: old.source.agent.clone(),
                    origin: None,
                },
                replacement.tags.unwrap_or_else(|| old.tags.clone()),
                Provenance {
                    requested_by: requested_by.to_string(),
                    job_id: None,
                },
            )
            .with_name(old.name.clone());
            // The instant the old drawer closes: its end is exclusive, so the
            // two are never both valid and never both absent.
            drawer.valid_from = at;
            drawer
        });
        if !self.store.supersede_drawer(id, new.as_ref(), at).await? {
            return Err(superseded());
        }
        if new.is_some() {
            self.scheduler.ensure_embedding_sweep().await;
        }
        let closed = self
            .store
            .get_drawer(id)
            .await?
            .ok_or_else(|| Error::DrawerNotFound {
                room: "-".to_string(),
                drawer: id.to_string(),
            })?;
        Ok(Superseded {
            superseded: closed,
            replacement: new,
        })
    }

    /// Record that a drawer mentions an entity (`kind` and `name`), creating
    /// the entity if needed, so graph-aware search can reach this drawer from
    /// others that share it.
    ///
    /// What a person or integration can use to say what a drawer is about (the
    /// extraction job links through the store directly, with provenance).
    /// Idempotent. The drawer is never changed: the link is
    /// derived data beside it. A write, so refused unless `mode` permits.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`], [`Error::DrawerNotFound`],
    /// [`Error::InvalidInput`] for a blank name, [`Error::EmptyLabel`] for a
    /// blank kind, or a store error.
    pub async fn link_drawer_entity(
        &self,
        drawer: DrawerId,
        name: &str,
        kind: &str,
        mode: MemoryMode,
    ) -> Result<EntityLink> {
        Self::require_write(mode, "drawer_link")?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::invalid_input("name", "must not be empty"));
        }
        if !self.store.drawer_exists(drawer).await? {
            return Err(Error::DrawerNotFound {
                room: "-".to_string(),
                drawer: drawer.to_string(),
            });
        }
        // The name converges on an entity it is a variant of (casing,
        // punctuation, an alias, a unique typo); the spelling is kept on the link.
        let (entity, observation) = self
            .store
            .resolve_or_create_entity(name, kind, self.dedup.entity_fuzzy)
            .await?;
        let created = self
            .store
            .link_drawer_entity_observed(drawer, entity.id, None, Some(&observation))
            .await?;
        Ok(EntityLink {
            drawer,
            entity,
            created,
        })
    }

    /// Every wing with its room and drawer counts.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, or a store error.
    pub async fn list_wings(&self, mode: MemoryMode) -> Result<Vec<WingSummary>> {
        Self::require_read(mode, "wing_list")?;
        self.store.list_wing_summaries().await
    }

    /// One wing, by name or id, with its rooms.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::ModeForbidden`] unless `mode`
    /// permits reads, or a store error.
    pub async fn show_wing(&self, wing: &str, mode: MemoryMode) -> Result<WingDetail> {
        Self::require_read(mode, "wing_show")?;
        let wing = self.resolve_wing(wing).await?;
        let rooms = self.store.list_room_summaries(&wing).await?;
        let wing = self.store.wing_summary(wing).await?;
        Ok(WingDetail { wing, rooms })
    }

    /// Create the wing `name`, or return it if it already exists.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] for an unusable name,
    /// [`Error::ModeForbidden`] unless `mode` permits writes, or a store error.
    pub async fn create_wing(
        &self,
        name: &str,
        description: Option<&str>,
        mode: MemoryMode,
    ) -> Result<Created<WingSummary>> {
        Self::require_write(mode, "wing_create")?;
        validate_name(NameKind::Wing, name)?;
        let created = self.store.get_wing(name).await?.is_none();
        let wing = self.store.get_or_create_wing(name, description).await?;
        let item = self.store.wing_summary(wing).await?;
        Ok(Created { created, item })
    }

    /// Delete a wing with every room and drawer in it.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`]; [`Error::PalaceBusy`] while a job that writes
    /// to the palace is queued, running or paused (it could silently
    /// re-create what is removed); [`Error::ModeForbidden`] unless `mode`
    /// permits writes; or a store error.
    pub async fn delete_wing(&self, wing: &str, mode: MemoryMode) -> Result<Deleted> {
        Self::require_write(mode, "wing_delete")?;
        let wing = self.resolve_wing(wing).await?;
        self.ensure_no_palace_writers(format!("wing `{}`", wing.name))
            .await?;
        self.store.delete_wing(wing.id).await
    }

    /// The rooms of one wing with their drawer counts.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::ModeForbidden`] unless `mode`
    /// permits reads, or a store error.
    pub async fn list_rooms(&self, wing: &str, mode: MemoryMode) -> Result<Vec<RoomSummary>> {
        Self::require_read(mode, "room_list")?;
        let wing = self.resolve_wing(wing).await?;
        self.store.list_room_summaries(&wing).await
    }

    /// One room, by name or id, with its drawer count.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::RoomNotFound`],
    /// [`Error::ModeForbidden`] unless `mode` permits reads, or a store error.
    pub async fn show_room(&self, wing: &str, room: &str, mode: MemoryMode) -> Result<RoomSummary> {
        Self::require_read(mode, "room_show")?;
        let wing = self.resolve_wing(wing).await?;
        let room = self.resolve_room(&wing, room).await?;
        self.store.room_summary(&wing, room).await
    }

    /// Create the room `name` in `wing`, or return it if it exists. Like
    /// everything else that files into the palace (mining, checkpoint, diary),
    /// a missing wing is created on the way.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] for an unusable name,
    /// [`Error::ModeForbidden`] unless `mode` permits writes, or a store error.
    pub async fn create_room(
        &self,
        wing: &str,
        name: &str,
        description: Option<&str>,
        mode: MemoryMode,
    ) -> Result<Created<RoomSummary>> {
        Self::require_write(mode, "room_create")?;
        validate_name(NameKind::Room, name)?;
        let wing = self.ensure_wing(wing).await?;
        let created = self.store.get_room(wing.id, name).await?.is_none();
        let room = self
            .store
            .get_or_create_room(wing.id, name, description)
            .await?;
        let item = self.store.room_summary(&wing, room).await?;
        Ok(Created { created, item })
    }

    /// Delete a room with every drawer in it.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::RoomNotFound`],
    /// [`Error::PalaceBusy`] (see [`Self::delete_wing`]),
    /// [`Error::ModeForbidden`] unless `mode` permits writes, or a store error.
    pub async fn delete_room(&self, wing: &str, room: &str, mode: MemoryMode) -> Result<Deleted> {
        Self::require_write(mode, "room_delete")?;
        let wing = self.resolve_wing(wing).await?;
        let room = self.resolve_room(&wing, room).await?;
        self.ensure_no_palace_writers(format!("room `{}/{}`", wing.name, room.name))
            .await?;
        self.store.delete_room(room.id).await
    }

    /// The newest drawers of a room, as listing rows (a preview of the
    /// content, not the content).
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::RoomNotFound`],
    /// [`Error::ModeForbidden`] unless `mode` permits reads, or a store error.
    pub async fn list_drawers(
        &self,
        wing: &str,
        room: &str,
        limit: Option<u32>,
        mode: MemoryMode,
    ) -> Result<Vec<DrawerSummary>> {
        Self::require_read(mode, "drawer_list")?;
        let wing = self.resolve_wing(wing).await?;
        let room = self.resolve_room(&wing, room).await?;
        let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_READ_LIMIT);
        self.store.list_drawer_summaries(room.id, limit).await
    }

    /// One drawer in full, by name or id.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::RoomNotFound`],
    /// [`Error::DrawerNotFound`], [`Error::ModeForbidden`] unless `mode`
    /// permits reads, or a store error.
    pub async fn show_drawer(
        &self,
        wing: &str,
        room: &str,
        drawer: &str,
        mode: MemoryMode,
    ) -> Result<Drawer> {
        Self::require_read(mode, "drawer_show")?;
        let wing = self.resolve_wing(wing).await?;
        let room = self.resolve_room(&wing, room).await?;
        self.resolve_drawer(&wing, &room, drawer).await
    }

    /// Write a drawer into a room, optionally named. A missing wing or room is
    /// created on the way.
    ///
    /// Drawer content is immutable, so naming is the only thing that can
    /// conflict: writing the same name with the same content again is a no-op
    /// (`created: false`), and the same name with other content is
    /// [`Error::DrawerNameTaken`]. An unnamed drawer is always new.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] for an unusable name,
    /// [`Error::InvalidInput`] for empty content, [`Error::DrawerNameTaken`],
    /// [`Error::ModeForbidden`] unless `mode` permits writes, or a store error.
    pub async fn create_drawer(
        &self,
        wing: &str,
        room: &str,
        name: Option<&str>,
        content: String,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Created<Drawer>> {
        Self::require_write(mode, "drawer_create")?;
        if content.trim().is_empty() {
            return Err(Error::invalid_input("content", "must not be empty"));
        }
        if let Some(name) = name {
            validate_name(NameKind::Drawer, name)?;
        }
        let wing = self.ensure_wing(wing).await?;
        let room = self.ensure_room(&wing, room).await?;

        let drawer = Drawer::new(
            DrawerId::new(),
            room.id,
            content,
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            vec![],
            Provenance {
                requested_by: requested_by.to_string(),
                job_id: None,
            },
        )
        .with_name(name.map(str::to_string));

        let Some(name) = name else {
            // An unnamed drawer has no identity beyond its words, so an exact
            // copy already in the room is returned instead of stored again.
            return match crate::dedup::write(
                &self.store,
                &drawer,
                &self.dedup,
                crate::dedup::Rules::MEMORY,
            )
            .await?
            {
                crate::dedup::Outcome::Stored { .. } => {
                    self.scheduler.ensure_embedding_sweep().await;
                    Ok(Created {
                        created: true,
                        item: drawer,
                    })
                }
                crate::dedup::Outcome::Duplicate { existing } => Ok(Created {
                    created: false,
                    item: self.store.get_drawer(existing).await?.ok_or_else(|| {
                        Error::DrawerNotFound {
                            room: format!("{}/{}", wing.name, room.name),
                            drawer: existing.to_string(),
                        }
                    })?,
                }),
            };
        };

        let taken = || Error::DrawerNameTaken {
            room: format!("{}/{}", wing.name, room.name),
            name: name.to_string(),
        };
        if let Some(existing) = self.store.get_drawer_by_name(room.id, name).await? {
            return if existing.content_hash == drawer.content_hash {
                Ok(Created {
                    created: false,
                    item: existing,
                })
            } else {
                Err(taken())
            };
        }
        // A name is an identity of its own, so a named write is always stored;
        // an identical drawer under another name is only linked to.
        let rules = crate::dedup::Rules {
            skip_exact: false,
            ..crate::dedup::Rules::MEMORY
        };
        if let Err(error) = crate::dedup::write(&self.store, &drawer, &self.dedup, rules).await {
            // Two writers racing for one name: the unique index let one win.
            // Report the loser as a name conflict rather than a storage fault.
            return if self
                .store
                .get_drawer_by_name(room.id, name)
                .await?
                .is_some()
            {
                Err(taken())
            } else {
                Err(error)
            };
        }
        self.scheduler.ensure_embedding_sweep().await;
        Ok(Created {
            created: true,
            item: drawer,
        })
    }

    /// Capture a note: a thought written down as it came, filed in `wing`/`room` and kept verbatim (docs/adr/031).
    ///
    /// A note is an unnamed drawer with `source.kind = note`, so it is recalled, searched, embedded, deduplicated and
    /// (when extraction is configured) read for entities like every other memory, and there is no second store.
    /// It is written synchronously, like a diary entry, so the caller gets the drawer's stable id back at once.
    /// `uri` is where it was captured (the working directory, for the CLI); `requested_by` is the channel it came
    /// through. The drawer's `created_at` and `valid_from` are the capture time.
    /// An exact copy already in the room is not stored twice: the existing note is returned with `created = false`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for blank content, [`Error::InvalidPalacePath`] for an unusable wing or room name,
    /// [`Error::ModeForbidden`] unless `mode` permits writes, or a store error.
    pub async fn note_write(
        &self,
        wing: &str,
        room: &str,
        content: String,
        uri: Option<String>,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Created<Drawer>> {
        Self::require_write(mode, "note_write")?;
        // A blank note is never recallable, so storing one would silently lose what the user believed they saved.
        if content.trim().is_empty() {
            return Err(Error::invalid_input("content", "must not be empty"));
        }
        let wing = self.ensure_wing(wing).await?;
        let room = self.ensure_room(&wing, room).await?;

        let drawer = Drawer::new(
            DrawerId::new(),
            room.id,
            content,
            Source::new(SourceKind::Note, uri, None),
            vec![],
            Provenance {
                requested_by: requested_by.to_string(),
                job_id: None,
            },
        );
        match crate::dedup::write(
            &self.store,
            &drawer,
            &self.dedup,
            crate::dedup::Rules::MEMORY,
        )
        .await?
        {
            crate::dedup::Outcome::Stored { .. } => {
                // Derived data, queued after the canonical write succeeded and never able to fail it.
                self.scheduler.ensure_embedding_sweep().await;
                // Notes carry no mining origin, so nothing else would ever ask for them to be read.
                self.scheduler.ensure_extraction_sweep().await;
                Ok(Created {
                    created: true,
                    item: drawer,
                })
            }
            crate::dedup::Outcome::Duplicate { existing } => Ok(Created {
                created: false,
                item: self.store.get_drawer(existing).await?.ok_or_else(|| {
                    Error::DrawerNotFound {
                        room: format!("{}/{}", wing.name, room.name),
                        drawer: existing.to_string(),
                    }
                })?,
            }),
        }
    }

    /// The drawers `drawer` was recorded as a likely duplicate of, or that were recorded as likely duplicates of
    /// it, with the evidence for each (docs/adr/025). Nothing is merged: every drawer here still stands on its own.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, [`Error::DrawerNotFound`], or a store error.
    pub async fn drawer_duplicates(
        &self,
        drawer: DrawerId,
        mode: MemoryMode,
    ) -> Result<Vec<crate::store::SimilarDrawer>> {
        Self::require_read(mode, "drawer_duplicates")?;
        if !self.store.drawer_exists(drawer).await? {
            return Err(Error::DrawerNotFound {
                room: "-".to_string(),
                drawer: drawer.to_string(),
            });
        }
        self.store.list_similar_drawers(drawer).await
    }

    /// How the knowledge `drawer` belongs to evolved: every version of its supersession chain, oldest first,
    /// each verbatim (identity, validity period, provenance and content).
    ///
    /// Any version of the chain gives the whole chain, so an agent can start from a search hit, current or
    /// historical, without reconstructing anything itself (docs/adr/032).
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, [`Error::DrawerNotFound`], or a store error.
    pub async fn drawer_history(
        &self,
        drawer: DrawerId,
        mode: MemoryMode,
    ) -> Result<crate::domain::DrawerHistory> {
        Self::require_read(mode, "drawer_history")?;
        let versions =
            self.store
                .drawer_lineage(drawer)
                .await?
                .ok_or_else(|| Error::DrawerNotFound {
                    room: "-".to_string(),
                    drawer: drawer.to_string(),
                })?;
        Ok(crate::domain::DrawerHistory { drawer, versions })
    }

    /// Delete one drawer, by name or id.
    ///
    /// # Errors
    ///
    /// [`Error::WingNotFound`], [`Error::RoomNotFound`],
    /// [`Error::DrawerNotFound`], [`Error::ModeForbidden`] unless `mode`
    /// permits writes, or a store error.
    pub async fn delete_drawer(
        &self,
        wing: &str,
        room: &str,
        drawer: &str,
        mode: MemoryMode,
    ) -> Result<Deleted> {
        Self::require_write(mode, "drawer_delete")?;
        let wing = self.resolve_wing(wing).await?;
        let room = self.resolve_room(&wing, room).await?;
        let drawer = self.resolve_drawer(&wing, &room, drawer).await?;
        self.store.delete_drawer(drawer.id).await?;
        Ok(Deleted {
            wings: 0,
            rooms: 0,
            drawers: 1,
        })
    }

    /// The wing a name or id refers to. A name wins over an id.
    async fn resolve_wing(&self, wing: &str) -> Result<Wing> {
        if let Some(found) = self.store.get_wing(wing).await? {
            return Ok(found);
        }
        if let Ok(id) = wing.parse::<WingId>()
            && let Some(found) = self.store.get_wing_by_id(id).await?
        {
            return Ok(found);
        }
        Err(Error::WingNotFound {
            wing: wing.to_string(),
        })
    }

    /// The wing a name or id refers to, created by name if it is not there.
    async fn ensure_wing(&self, wing: &str) -> Result<Wing> {
        match self.resolve_wing(wing).await {
            Ok(found) => Ok(found),
            Err(Error::WingNotFound { .. }) => {
                // An unknown id is not a name to create: `validate_name`
                // refuses anything shaped like a UUID.
                if wing.parse::<WingId>().is_ok() {
                    return Err(Error::WingNotFound {
                        wing: wing.to_string(),
                    });
                }
                validate_name(NameKind::Wing, wing)?;
                self.store.get_or_create_wing(wing, None).await
            }
            Err(error) => Err(error),
        }
    }

    /// The room of `wing` a name or id refers to, created when `room` is a usable name nothing has yet.
    async fn ensure_room(&self, wing: &Wing, room: &str) -> Result<Room> {
        match self.resolve_room(wing, room).await {
            Ok(room) => Ok(room),
            Err(Error::RoomNotFound { .. }) => {
                // An unknown id is not a name to create: `validate_name`
                // refuses anything shaped like a UUID.
                if room.parse::<RoomId>().is_ok() {
                    return Err(Error::RoomNotFound {
                        wing: wing.name.clone(),
                        room: room.to_string(),
                    });
                }
                validate_name(NameKind::Room, room)?;
                self.store.get_or_create_room(wing.id, room, None).await
            }
            Err(error) => Err(error),
        }
    }

    /// The room of `wing` a name or id refers to. A name wins over an id, and
    /// an id of a room in another wing is not found here.
    async fn resolve_room(&self, wing: &Wing, room: &str) -> Result<Room> {
        if let Some(found) = self.store.get_room(wing.id, room).await? {
            return Ok(found);
        }
        if let Ok(id) = room.parse::<RoomId>()
            && let Some(found) = self.store.get_room_by_id(id).await?
            && found.wing == wing.id
        {
            return Ok(found);
        }
        Err(Error::RoomNotFound {
            wing: wing.name.clone(),
            room: room.to_string(),
        })
    }

    /// The drawer of `room` a name or id refers to.
    async fn resolve_drawer(&self, wing: &Wing, room: &Room, drawer: &str) -> Result<Drawer> {
        if let Some(found) = self.store.get_drawer_by_name(room.id, drawer).await? {
            return Ok(found);
        }
        if let Ok(id) = drawer.parse::<DrawerId>()
            && let Some(found) = self.store.get_drawer(id).await?
            && found.room == room.id
        {
            return Ok(found);
        }
        Err(Error::DrawerNotFound {
            room: format!("{}/{}", wing.name, room.name),
            drawer: drawer.to_string(),
        })
    }

    /// Refuse while a job that writes to the palace is pending.
    ///
    /// A mining or checkpoint job files into wings and rooms by name,
    /// creating them if absent, so deleting a wing under one would quietly
    /// bring it back (with whatever the job writes next). Which wing a job
    /// targets is not reliably known up front — a checkpoint names wings per
    /// item — so the rule is deliberately coarse: any such job blocks any
    /// wing or room delete. The check is not atomic with the delete; a job
    /// submitted in between is not caught, which the docs say.
    async fn ensure_no_palace_writers(&self, target: String) -> Result<()> {
        let mut count = 0;
        for status in [JobStatus::Queued, JobStatus::Running, JobStatus::Paused] {
            count += self
                .store
                .list_jobs(Some(status))
                .await?
                .iter()
                .filter(|job| writes_to_palace(&job.kind))
                .count();
        }
        if count > 0 {
            return Err(Error::PalaceBusy { target, count });
        }
        Ok(())
    }
}

/// Whether a job of this kind can file drawers into (or remove them from) the
/// palace. A dry-run repair and an audit only report; a demo touches nothing.
fn writes_to_palace(kind: &JobKind) -> bool {
    match kind {
        JobKind::Mine { .. } | JobKind::Checkpoint { .. } => true,
        JobKind::Repair { dry_run, .. } => !dry_run,
        // An embedding sweep only fills a field of drawers that exist; it never
        // creates or removes one, so a wing deleted under it is not resurrected.
        // Extraction only adds graph records beside drawers that exist, for the same reason.
        JobKind::Demo { .. }
        | JobKind::Audit { .. }
        | JobKind::Embed { .. }
        | JobKind::Extract { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Job, JobEvent, Priority};

    const FULL: MemoryMode = MemoryMode::Full;

    async fn app() -> AppServices {
        AppServices::for_tests().await
    }

    async fn drawer(
        app: &AppServices,
        path: (&str, &str),
        name: Option<&str>,
        content: &str,
    ) -> Drawer {
        app.create_drawer(path.0, path.1, name, content.to_string(), "test", FULL)
            .await
            .unwrap()
            .item
    }

    #[tokio::test]
    async fn creating_a_wing_twice_returns_the_same_wing_and_says_it_was_not_new() {
        let app = app().await;
        let first = app.create_wing("work", None, FULL).await.unwrap();
        let second = app.create_wing("work", None, FULL).await.unwrap();
        assert!(first.created && !second.created);
        assert_eq!(first.item.wing.id, second.item.wing.id);
    }

    #[tokio::test]
    async fn a_room_can_be_addressed_by_name_or_by_id_and_a_wing_likewise() {
        let app = app().await;
        let room = app.create_room("work", "x", None, FULL).await.unwrap().item;
        let by_ids = app
            .show_room(&room.room.wing.to_string(), &room.room.id.to_string(), FULL)
            .await
            .unwrap();
        assert_eq!(by_ids.room.id, room.room.id);
        assert_eq!(by_ids.wing_name, "work");
    }

    #[tokio::test]
    async fn a_room_id_from_another_wing_is_not_found_under_this_one() {
        let app = app().await;
        let room = app.create_room("a", "x", None, FULL).await.unwrap().item;
        app.create_wing("b", None, FULL).await.unwrap();
        let result = app.show_room("b", &room.room.id.to_string(), FULL).await;
        assert!(
            matches!(result, Err(Error::RoomNotFound { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn an_unknown_wing_room_or_drawer_is_reported_as_such() {
        let app = app().await;
        assert!(matches!(
            app.show_wing("nope", FULL).await,
            Err(Error::WingNotFound { .. })
        ));
        app.create_wing("work", None, FULL).await.unwrap();
        assert!(matches!(
            app.show_room("work", "nope", FULL).await,
            Err(Error::RoomNotFound { .. })
        ));
        app.create_room("work", "x", None, FULL).await.unwrap();
        assert!(matches!(
            app.show_drawer("work", "x", "nope", FULL).await,
            Err(Error::DrawerNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn a_drawer_is_shown_by_its_name_and_by_its_id() {
        let app = app().await;
        let made = drawer(&app, ("work", "x"), Some("context"), "hello").await;
        let by_name = app.show_drawer("work", "x", "context", FULL).await.unwrap();
        let by_id = app
            .show_drawer("work", "x", &made.id.to_string(), FULL)
            .await
            .unwrap();
        assert_eq!(by_name.id, made.id);
        assert_eq!(by_id.content, "hello");
    }

    #[tokio::test]
    async fn writing_the_same_named_drawer_again_is_a_no_op_but_other_content_conflicts() {
        let app = app().await;
        let first = drawer(&app, ("w", "r"), Some("n"), "same").await;
        let again = app
            .create_drawer("w", "r", Some("n"), "same".into(), "test", FULL)
            .await
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.item.id, first.id);

        let clash = app
            .create_drawer("w", "r", Some("n"), "different".into(), "test", FULL)
            .await;
        assert!(
            matches!(clash, Err(Error::DrawerNameTaken { .. })),
            "{clash:?}"
        );
    }

    #[tokio::test]
    async fn empty_drawer_content_and_unusable_names_are_refused() {
        let app = app().await;
        let empty = app
            .create_drawer("w", "r", None, "  ".into(), "test", FULL)
            .await;
        assert!(matches!(empty, Err(Error::InvalidInput { .. })));
        assert!(matches!(
            app.create_wing("a/b", None, FULL).await,
            Err(Error::InvalidPalacePath { .. })
        ));
        assert!(matches!(
            app.create_room("w", "", None, FULL).await,
            Err(Error::InvalidPalacePath { .. })
        ));
    }

    #[tokio::test]
    async fn creating_a_room_creates_its_missing_wing_but_not_from_an_unknown_id() {
        let app = app().await;
        app.create_room("fresh", "r", None, FULL).await.unwrap();
        assert_eq!(app.list_wings(FULL).await.unwrap().len(), 1);
        let unknown = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            app.create_room(&unknown, "r", None, FULL).await,
            Err(Error::WingNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn an_unusable_name_on_the_way_in_is_a_path_error_and_an_unknown_id_is_not_found() {
        let app = app().await;
        let unknown = uuid::Uuid::new_v4().to_string();
        for (wing, room) in [("w", "a/b"), ("a/b", "r")] {
            let result = app
                .create_drawer(wing, room, None, "x".into(), "test", FULL)
                .await;
            assert!(
                matches!(result, Err(Error::InvalidPalacePath { .. })),
                "{wing}/{room}: {result:?}"
            );
        }
        app.create_wing("w", None, FULL).await.unwrap();
        let result = app
            .create_drawer("w", &unknown, None, "x".into(), "test", FULL)
            .await;
        assert!(
            matches!(result, Err(Error::RoomNotFound { .. })),
            "{result:?}"
        );
        assert!(
            app.list_rooms("w", FULL).await.unwrap().is_empty(),
            "no room was created from an id"
        );
    }

    #[tokio::test]
    async fn a_checkpoint_with_an_unusable_item_name_is_refused_at_submission() {
        use crate::domain::{CheckpointDestination, CheckpointItem, CheckpointPayload};
        let app = app().await;
        let payload = CheckpointPayload {
            items: vec![CheckpointItem {
                destination: CheckpointDestination::General,
                wing: None,
                name: Some("../escape".to_string()),
                content: "x".to_string(),
                tags: vec![],
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: None,
                    origin: None,
                },
                fact: None,
            }],
        };
        let result = app.submit_checkpoint(payload, "test", FULL).await;
        assert!(
            matches!(result, Err(Error::InvalidPalacePath { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn deleting_a_wing_reports_what_it_removed_and_the_wing_is_gone() {
        let app = app().await;
        drawer(&app, ("work", "a"), None, "one").await;
        drawer(&app, ("work", "a"), Some("n"), "two").await;
        drawer(&app, ("work", "b"), None, "three").await;

        let deleted = app.delete_wing("work", FULL).await.unwrap();

        assert_eq!((deleted.wings, deleted.rooms, deleted.drawers), (1, 2, 3));
        assert!(matches!(
            app.show_wing("work", FULL).await,
            Err(Error::WingNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn deleting_a_drawer_leaves_its_siblings() {
        let app = app().await;
        drawer(&app, ("w", "r"), Some("gone"), "a").await;
        drawer(&app, ("w", "r"), Some("kept"), "b").await;
        app.delete_drawer("w", "r", "gone", FULL).await.unwrap();
        let left = app.list_drawers("w", "r", None, FULL).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name.as_deref(), Some("kept"));
    }

    #[tokio::test]
    async fn a_delete_is_refused_while_a_job_that_writes_is_pending_and_allowed_once_it_is_not() {
        let app = app().await;
        drawer(&app, ("work", "r"), None, "x").await;
        let job = Job::new(
            JobKind::Mine {
                source: crate::domain::MiningSource::Directory {
                    path: "/tmp".into(),
                },
                wing: None,
                full: false,
            },
            Priority::Normal,
            "test",
        );
        app.store.save_job(&job).await.unwrap();

        let refused = app.delete_wing("work", FULL).await;
        assert!(
            matches!(refused, Err(Error::PalaceBusy { count: 1, .. })),
            "{refused:?}"
        );
        assert!(
            app.show_wing("work", FULL).await.is_ok(),
            "nothing was deleted"
        );

        let mut done = job;
        done.apply(JobEvent::Cancel).unwrap();
        app.store.save_job(&done).await.unwrap();
        app.delete_wing("work", FULL).await.unwrap();
    }

    #[tokio::test]
    async fn a_read_only_audit_does_not_block_a_delete() {
        let app = app().await;
        drawer(&app, ("work", "r"), None, "x").await;
        let audit = Job::new(JobKind::Audit { scope: None }, Priority::Normal, "test");
        app.store.save_job(&audit).await.unwrap();
        app.delete_wing("work", FULL).await.unwrap();
    }

    #[tokio::test]
    async fn read_only_may_look_but_not_change_and_disabled_may_do_neither() {
        let app = app().await;
        drawer(&app, ("w", "r"), Some("n"), "x").await;

        assert!(app.list_wings(MemoryMode::ReadOnly).await.is_ok());
        assert!(
            app.show_drawer("w", "r", "n", MemoryMode::ReadOnly)
                .await
                .is_ok()
        );
        assert!(matches!(
            app.create_wing("z", None, MemoryMode::ReadOnly).await,
            Err(Error::ModeForbidden { .. })
        ));
        assert!(matches!(
            app.delete_wing("w", MemoryMode::ReadOnly).await,
            Err(Error::ModeForbidden { .. })
        ));
        assert!(matches!(
            app.delete_drawer("w", "r", "n", MemoryMode::ReadOnly).await,
            Err(Error::ModeForbidden { .. })
        ));
        assert!(matches!(
            app.list_wings(MemoryMode::Disabled).await,
            Err(Error::ModeForbidden { .. })
        ));
        assert!(matches!(
            app.list_drawers("w", "r", None, MemoryMode::Disabled).await,
            Err(Error::ModeForbidden { .. })
        ));
    }
}
