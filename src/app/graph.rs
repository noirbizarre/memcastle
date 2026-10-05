//! Reading the knowledge graph: entities, the relationships between them and the drawers that mention them.
//!
//! Every answer carries its provenance and temporal validity (docs/adr/024), so a client can tell a fact somebody
//! asserted from one an extractor derived, and see what it was derived from. The graph is derived data beside the
//! drawers: reading it is a **read** under ADR-007, and there is no write here (extraction is a job, a link is
//! `drawer_link`).

use crate::domain::{Entity, EntityId, MemoryMode, Mention, Relationship};
use crate::error::{Error, Result};
use crate::store::PossibleEntity;

use super::{AppServices, effective_limit};

/// How many entities a listing returns when the caller does not say.
pub const DEFAULT_ENTITY_LIMIT: u32 = 50;

impl AppServices {
    /// Entities, optionally narrowed to a kind and to names containing `name`, by name.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, or a store error.
    pub async fn list_entities(
        &self,
        name: Option<&str>,
        kind: Option<&str>,
        limit: Option<u32>,
        mode: MemoryMode,
    ) -> Result<Vec<Entity>> {
        Self::require_read(mode, "entity_list")?;
        let limit = effective_limit(limit, DEFAULT_ENTITY_LIMIT);
        let name = name.map(str::trim).filter(|name| !name.is_empty());
        self.store.list_entities(name, kind, limit).await
    }

    /// Every relationship touching an entity, in either direction, with provenance and validity.
    /// `include_expired` adds the history of superseded and retracted facts.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, [`Error::EntityNotFound`], or a store error.
    pub async fn entity_relationships(
        &self,
        entity: EntityId,
        include_expired: bool,
        mode: MemoryMode,
    ) -> Result<Vec<Relationship>> {
        Self::require_read(mode, "entity_relationships")?;
        self.require_entity(entity).await?;
        self.store.list_relationships(entity, include_expired).await
    }

    /// Every drawer that mentions an entity, with the provenance of the link.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, [`Error::EntityNotFound`], or a store error.
    pub async fn entity_mentions(
        &self,
        entity: EntityId,
        mode: MemoryMode,
    ) -> Result<Vec<Mention>> {
        Self::require_read(mode, "entity_mentions")?;
        self.require_entity(entity).await?;
        self.store.list_entity_mentions(entity).await
    }

    /// The entities `entity` resembles without having been equated with it, or that resemble it: names
    /// entity resolution could not settle (docs/adr/025). Nothing here is merged; each is awaiting a decision.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, [`Error::EntityNotFound`], or a store error.
    pub async fn entity_candidates(
        &self,
        entity: EntityId,
        mode: MemoryMode,
    ) -> Result<Vec<PossibleEntity>> {
        Self::require_read(mode, "entity_candidates")?;
        self.require_entity(entity).await?;
        self.store.list_possible_entities(entity).await
    }

    /// Record `alias` as another spelling of `entity`, so every later sighting of it converges there. The way to
    /// settle one of [`Self::entity_candidates`] by hand. Idempotent. A write, so refused unless `mode` permits.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`], [`Error::EntityNotFound`], [`Error::EmptyLabel`] for a blank alias, or a store
    /// error.
    pub async fn add_entity_alias(
        &self,
        entity: EntityId,
        alias: &str,
        mode: MemoryMode,
    ) -> Result<Entity> {
        Self::require_write(mode, "entity_alias")?;
        self.store
            .add_entity_alias(entity, alias)
            .await?
            .ok_or_else(|| Error::EntityNotFound {
                id: entity.to_string(),
            })
    }

    async fn require_entity(&self, entity: EntityId) -> Result<()> {
        match self.store.get_entity(entity).await? {
            Some(_) => Ok(()),
            None => Err(Error::EntityNotFound {
                id: entity.to_string(),
            }),
        }
    }
}
