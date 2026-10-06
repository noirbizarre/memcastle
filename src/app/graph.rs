//! Reading the knowledge graph: entities, the relationships between them and the drawers that mention them.
//!
//! Every answer carries its provenance and temporal validity (docs/adr/024), so a client can tell a fact somebody
//! asserted from one an extractor derived, and see what it was derived from. The graph is derived data beside the
//! drawers: reading it is a **read** under ADR-007, and there is no write here (extraction is a job, a link is
//! `drawer_link`).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::domain::{Entity, EntityId, MemoryMode, Mention, Relationship};
use crate::error::{Error, Result};
use crate::events::{Action, Event};
use crate::store::PossibleEntity;

use super::{AppServices, effective_limit};

/// How many entities a listing returns when the caller does not say.
pub const DEFAULT_ENTITY_LIMIT: u32 = 50;
/// How many entities a graph answer holds when the caller does not say.
pub const DEFAULT_GRAPH_NODES: u32 = 50;
/// The most hops a graph neighbourhood follows, whatever the caller asks for: each hop is a query per entity.
pub const MAX_GRAPH_DEPTH: u32 = 3;

/// The answer to [`AppServices::graph`]: entities and the open facts between them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphView {
    /// The entities, the centre first when there is one.
    pub nodes: Vec<Entity>,
    /// The relationships between them, each with its provenance.
    pub edges: Vec<Relationship>,
    /// Whether the entity cap stopped the answer short of everything in range.
    pub truncated: bool,
}

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
        let entity = self
            .store
            .add_entity_alias(entity, alias)
            .await?
            .ok_or_else(|| Error::EntityNotFound {
                id: entity.to_string(),
            })?;
        self.announce(Event::entity(Action::Updated, entity.id));
        Ok(entity)
    }

    /// A piece of the knowledge graph in one answer, so a client can draw it without one request per entity.
    ///
    /// With `center`, the entities within `depth` hops of it (in either direction, open facts only) and the facts
    /// between them. Without one, the first `limit` entities by name and the facts among them: a starting point to
    /// pick from, not a ranking. Either way the answer is capped at `limit` entities and says when it stopped there.
    /// Read-only, so gated as a **read**.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] unless `mode` permits reads, [`Error::EntityNotFound`] when `center` does not exist,
    /// or a store error.
    pub async fn graph(
        &self,
        center: Option<EntityId>,
        depth: Option<u32>,
        limit: Option<u32>,
        mode: MemoryMode,
    ) -> Result<GraphView> {
        Self::require_read(mode, "graph")?;
        let cap = effective_limit(limit, DEFAULT_GRAPH_NODES) as usize;
        let depth = depth.unwrap_or(1).clamp(1, MAX_GRAPH_DEPTH);

        let mut nodes: Vec<Entity> = Vec::new();
        let mut seen: HashSet<EntityId> = HashSet::new();
        let mut edges: Vec<Relationship> = Vec::new();
        let mut edge_ids = HashSet::new();
        let mut truncated = false;
        let mut frontier: Vec<EntityId> = Vec::new();

        if let Some(center) = center {
            let entity =
                self.store
                    .get_entity(center)
                    .await?
                    .ok_or_else(|| Error::EntityNotFound {
                        id: center.to_string(),
                    })?;
            seen.insert(entity.id);
            frontier.push(entity.id);
            nodes.push(entity);
        } else {
            // Over-asked by one so "there is more" is known without a count query.
            let listed = self.store.list_entities(None, None, cap as u32 + 1).await?;
            truncated = listed.len() > cap;
            for entity in listed.into_iter().take(cap) {
                seen.insert(entity.id);
                frontier.push(entity.id);
                nodes.push(entity);
            }
        }

        let expand = center.is_some();
        for hop in 0..depth {
            let mut next = Vec::new();
            for entity in &frontier {
                for relationship in self.store.list_relationships(*entity, false).await? {
                    let other = if relationship.from == *entity {
                        relationship.to
                    } else {
                        relationship.from
                    };
                    if !seen.contains(&other) {
                        // Only a neighbourhood grows; the overview keeps to the entities it listed.
                        if !expand {
                            continue;
                        }
                        if nodes.len() >= cap {
                            truncated = true;
                            continue;
                        }
                        let Some(entity) = self.store.get_entity(other).await? else {
                            continue;
                        };
                        seen.insert(other);
                        next.push(other);
                        nodes.push(entity);
                    }
                    if edge_ids.insert(relationship.id.to_string()) {
                        edges.push(relationship);
                    }
                }
            }
            // The overview is one hop among what it listed; a neighbourhood stops when nothing new arrived.
            if !expand || next.is_empty() || hop + 1 == depth {
                break;
            }
            frontier = next;
        }

        // An edge whose far end was cut by the cap is dropped: a line to a node that is not drawn is noise.
        edges.retain(|edge| seen.contains(&edge.from) && seen.contains(&edge.to));
        Ok(GraphView {
            nodes,
            edges,
            truncated,
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
