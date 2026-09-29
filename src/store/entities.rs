//! Knowledge-graph repository methods: entities and their relationships.
//!
//! The first place in this codebase that issues `RELATE`/graph-edge
//! SurrealQL — everywhere else (`wings`, `drawers`, `jobs`) stores foreign
//! keys as plain strings (see `wings`'s module doc). `relates_to` is a real
//! SurrealDB graph edge table (`TYPE RELATION IN entity OUT entity`), so
//! creating/closing/traversing it goes through `RELATE`/`UPDATE`/a graph
//! traversal expression instead of `CREATE`/plain `SELECT ... WHERE`.
//!
//! **Querying "is this fact still current":** `drawer.valid_to` (the only
//! prior art for an optional temporal field) is write-only — nothing ever
//! filters on it, so there's no established pattern here to follow. Rather
//! than lean on the NULL-vs-NONE distinction `store::mod`'s own regression
//! test warns is a 3.x driver gotcha for *bound query parameters*, current
//! vs. expired is decided with SurrealQL truthiness (`!valid_to`), which
//! reads as "still valid" whether an unset `valid_to` ends up stored as
//! `NONE` (absent) or `NULL` — the ambiguity this codebase's other comments
//! flag doesn't matter for a truthiness check the way it does for `= NULL`.

use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{
    Entity, EntityId, NewRelationship, Relationship, RelationshipId, normalize_label,
};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The column list every entity read projects. See `drawers::DRAWER_COLUMNS`
/// for why `id` goes through `record::id()`.
const ENTITY_COLUMNS: &str = "record::id(id) AS id, name, kind, properties";

/// The column list every relationship read projects. `in`/`out` are
/// `relates_to`'s built-in graph-edge fields (every `RELATE`d table has
/// them) — aliased to `from`/`to` to match [`Relationship`]'s field names.
const RELATIONSHIP_COLUMNS: &str = "record::id(id) AS id, record::id(in) AS from, \
     record::id(out) AS to, predicate, confidence, \
     <string>valid_from AS valid_from, valid_to";

/// Reject an empty (post-[`normalize_label`]) label with a diagnostic that
/// names the field, instead of silently storing `""`.
fn required_label(field: &'static str, raw: &str) -> Result<String> {
    normalize_label(raw).ok_or(Error::EmptyLabel {
        field: field.to_string(),
    })
}

impl SurrealStore {
    /// Find the entity named `name` of kind `kind`, or create it.
    ///
    /// `kind` is normalized (trimmed, lowercased) before either half of the
    /// get-or-create runs, so `"Person"` and `"person"` resolve to the same
    /// entity — see [`normalize_label`].
    pub async fn get_or_create_entity(
        &self,
        name: &str,
        kind: &str,
        properties: Value,
    ) -> Result<Entity> {
        let kind = required_label("kind", kind)?;

        let mut response = self
            .db
            .query(format!(
                "SELECT {ENTITY_COLUMNS} FROM entity WHERE name = $name AND kind = $kind LIMIT 1"
            ))
            .bind(("name", name.to_string()))
            .bind(("kind", kind.clone()))
            .await?;
        let existing: Vec<Entity> = super::take_rows(&mut response, 0)?;
        if let Some(entity) = existing.into_iter().next() {
            return Ok(entity);
        }

        let entity = Entity {
            id: EntityId::new(),
            name: name.to_string(),
            kind,
            properties,
        };
        self.db
            .query(
                "CREATE type::record('entity', $id) SET \
                 name = $name, kind = $kind, properties = $properties",
            )
            .bind(("id", entity.id.to_string()))
            .bind(("name", entity.name.clone()))
            .bind(("kind", entity.kind.clone()))
            // `Value`'s object variant serializes via `serialize_map`, which
            // the driver's binder handles natively -- unlike `Source`/
            // `Provenance` in `drawers.rs`, no `bindable()` wrapping needed.
            .bind(("properties", entity.properties.clone()))
            .await?
            .check()?;
        Ok(entity)
    }

    /// Create a new, currently-valid relationship (`valid_to: None`) between
    /// two entities.
    pub async fn create_relationship(
        &self,
        from: EntityId,
        to: EntityId,
        predicate: &str,
        confidence: f32,
    ) -> Result<Relationship> {
        self.create_relationship_with_id(RelationshipId::new(), from, to, predicate, confidence)
            .await
    }

    /// Like [`Self::create_relationship`], but under a caller-chosen id and
    /// a no-op if that id already exists — the replay-safe form for a job
    /// handler that derives the id from (job, item index). See
    /// [`Self::create_drawer_once`] for why a replay must not duplicate.
    ///
    /// Returns the relationship as it now stands: the one just written, or
    /// the one an earlier attempt already wrote.
    pub async fn create_relationship_with_id(
        &self,
        id: RelationshipId,
        from: EntityId,
        to: EntityId,
        predicate: &str,
        confidence: f32,
    ) -> Result<Relationship> {
        let predicate = required_label("predicate", predicate)?;
        let relationship = Relationship {
            id,
            from,
            to,
            predicate,
            confidence,
            valid_from: Utc::now(),
            valid_to: None,
        };
        if self.relationship_exists(id).await? {
            return Ok(relationship);
        }
        self.relate(&relationship).await?;
        Ok(relationship)
    }

    /// Whether a `relates_to` edge with this id exists.
    pub async fn relationship_exists(&self, id: RelationshipId) -> Result<bool> {
        #[derive(Deserialize)]
        struct IdRow {
            #[allow(dead_code)]
            id: String,
        }
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id FROM relates_to \
                 WHERE id = type::record('relates_to', $id)",
            )
            .bind(("id", id.to_string()))
            .await?;
        let rows: Vec<IdRow> = super::take_rows(&mut response, 0)?;
        Ok(!rows.is_empty())
    }

    /// Close `old_id` (`valid_to = now`) and open a replacement edge for
    /// `new`, in one transaction — "atomically where practical" (the
    /// issue's wording): the first explicit `BEGIN`/`COMMIT TRANSACTION` in
    /// this codebase, because superseding a fact should never leave the
    /// graph with either two current edges or zero.
    pub async fn supersede_relationship(
        &self,
        old_id: RelationshipId,
        new: NewRelationship,
    ) -> Result<Relationship> {
        self.supersede_relationship_with_id(old_id, RelationshipId::new(), new)
            .await
    }

    /// Like [`Self::supersede_relationship`], but the replacement gets a
    /// caller-chosen id, and the whole supersede is skipped if that id
    /// already exists — an earlier attempt already did it, and repeating it
    /// would move the old edge's `valid_to` and open a duplicate current
    /// edge. See [`Self::create_drawer_once`].
    pub async fn supersede_relationship_with_id(
        &self,
        old_id: RelationshipId,
        new_id: RelationshipId,
        new: NewRelationship,
    ) -> Result<Relationship> {
        let predicate = required_label("predicate", &new.predicate)?;
        let replacement = Relationship {
            id: new_id,
            from: new.from,
            to: new.to,
            predicate,
            confidence: new.confidence,
            valid_from: Utc::now(),
            valid_to: None,
        };
        if self.relationship_exists(new_id).await? {
            return Ok(replacement);
        }

        self.db
            .query(
                "BEGIN TRANSACTION; \
                 UPDATE type::record('relates_to', $old_id) SET valid_to = $now; \
                 RELATE (type::record('entity', $from))->relates_to->(type::record('entity', $to)) \
                     SET id = type::record('relates_to', $new_id), predicate = $predicate, \
                         confidence = $confidence, valid_from = <datetime>$now, valid_to = NONE; \
                 COMMIT TRANSACTION;",
            )
            .bind(("old_id", old_id.to_string()))
            .bind(("now", replacement.valid_from.to_rfc3339()))
            .bind(("new_id", replacement.id.to_string()))
            .bind(("from", replacement.from.to_string()))
            .bind(("to", replacement.to.to_string()))
            .bind(("predicate", replacement.predicate.clone()))
            .bind(("confidence", replacement.confidence))
            .await?
            .check()?;
        Ok(replacement)
    }

    /// Close a relationship (`valid_to = now`) without opening a
    /// replacement — the fact is retracted, not superseded by a new one.
    pub async fn invalidate_relationship(&self, id: RelationshipId) -> Result<()> {
        self.db
            // `WHERE !valid_to`: only close an edge that is still open, so
            // repeating an invalidate (a replayed job item) keeps the
            // original close time instead of silently moving it forward.
            .query(
                "UPDATE type::record('relates_to', $id) SET valid_to = $valid_to WHERE !valid_to",
            )
            .bind(("id", id.to_string()))
            .bind(("valid_to", Utc::now().to_rfc3339()))
            .await?
            .check()?;
        Ok(())
    }

    /// List every relationship touching `entity`, in either direction.
    ///
    /// The required native graph traversal: `<->relates_to` walks both the
    /// `in` and `out` sides of the edge table from `entity` in one query,
    /// with the nested `SELECT` acting as a graph clause (SurrealDB's term
    /// for a `WHERE`/projection scoped to just the edges reached by the
    /// traversal, not the whole `relates_to` table).
    ///
    /// `include_expired: false` (the common case) returns only currently
    /// valid facts; `true` also returns superseded/invalidated history.
    pub async fn list_relationships(
        &self,
        entity: EntityId,
        include_expired: bool,
    ) -> Result<Vec<Relationship>> {
        #[derive(Deserialize)]
        struct EntityEdges {
            edges: Vec<Relationship>,
        }

        let sql = format!(
            "SELECT <->(SELECT {RELATIONSHIP_COLUMNS} FROM relates_to \
             WHERE $include_expired OR !valid_to ORDER BY valid_from DESC) AS edges \
             FROM type::record('entity', $entity)"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("entity", entity.to_string()))
            .bind(("include_expired", include_expired))
            .await?;
        let mut rows: Vec<EntityEdges> = super::take_rows(&mut response, 0)?;
        Ok(rows.pop().map(|r| r.edges).unwrap_or_default())
    }

    /// Shared write path for a fresh edge: `create_relationship` and
    /// `supersede_relationship`'s replacement half both open a brand-new,
    /// currently-valid `relates_to` record the same way.
    async fn relate(&self, relationship: &Relationship) -> Result<()> {
        self.db
            .query(
                "RELATE (type::record('entity', $from))->relates_to->(type::record('entity', $to)) \
                 SET id = type::record('relates_to', $id), predicate = $predicate, \
                     confidence = $confidence, valid_from = <datetime>$valid_from, valid_to = $valid_to",
            )
            .bind(("id", relationship.id.to_string()))
            .bind(("from", relationship.from.to_string()))
            .bind(("to", relationship.to.to_string()))
            .bind(("predicate", relationship.predicate.clone()))
            .bind(("confidence", relationship.confidence))
            .bind(("valid_from", relationship.valid_from.to_rfc3339()))
            .bind((
                "valid_to",
                relationship.valid_to.map(|dt| dt.to_rfc3339()),
            ))
            .await?
            .check()?;
        Ok(())
    }
}
