//! Knowledge-graph repository methods: entities and their relationships.
//!
//! The first place in this codebase that issues `RELATE`/graph-edge
//! SurrealQL — everywhere else (`wings`, `drawers`, `jobs`) stores foreign
//! keys as plain strings (see `wings`'s module doc). `relates_to` is a real
//! SurrealDB graph edge table (`TYPE RELATION IN entity OUT entity`), so
//! creating/closing/traversing it goes through `RELATE`/`UPDATE`/a graph
//! traversal expression instead of `CREATE`/plain `SELECT ... WHERE`.
//!
//! **Querying "is this fact still current":** current vs. expired is decided
//! with SurrealQL truthiness (`!valid_to`), not `= NULL`: it reads as "still
//! valid" whether an unset `valid_to` ends up stored as `NONE` (absent) or
//! `NULL` — the ambiguity this codebase's other comments flag (see
//! `store::mod`'s regression test on bound parameters) doesn't matter for a
//! truthiness check the way it does for `= NULL`. Drawers use the same rule:
//! `store::retrieval`'s scope predicate and `store::graph` both read validity
//! this way, so a drawer and a relationship are "current" by one definition.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{
    Entity, EntityId, FactProvenance, NewRelationship, Relationship, RelationshipId, require_label,
};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The column list every entity read projects. See `drawers::DRAWER_COLUMNS`
/// for why `id` goes through `record::id()`.
///
/// `aliases` is `option<array>` in the schema (an entity from before resolution has none), so it is coalesced to
/// an empty list here rather than in every reader.
pub(super) const ENTITY_COLUMNS: &str =
    "record::id(id) AS id, name, kind, properties, aliases ?? [] AS aliases";

/// The column list every relationship read projects. `in`/`out` are
/// `relates_to`'s built-in graph-edge fields (every `RELATE`d table has
/// them) — aliased to `from`/`to` to match [`Relationship`]'s field names.
const RELATIONSHIP_COLUMNS: &str = "record::id(id) AS id, record::id(in) AS from, \
     record::id(out) AS to, predicate, confidence, \
     <string>valid_from AS valid_from, valid_to, provenance";

impl SurrealStore {
    /// Find the entity named `name` of kind `kind`, or create it.
    ///
    /// `kind` is normalized (trimmed, lowercased) before either half of the
    /// get-or-create runs, so `"Person"` and `"person"` resolve to the same
    /// entity — see [`crate::domain::normalize_label`].
    pub async fn get_or_create_entity(
        &self,
        name: &str,
        kind: &str,
        properties: Value,
    ) -> Result<Entity> {
        let kind = require_label("kind", kind)?;

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
            aliases: Vec::new(),
        };
        self.db
            .query(
                "CREATE type::record('entity', $id) SET \
                 name = $name, kind = $kind, properties = $properties, \
                 key = $key, aliases = [], alias_keys = []",
            )
            .bind(("id", entity.id.to_string()))
            .bind(("name", entity.name.clone()))
            .bind(("kind", entity.kind.clone()))
            // The resolution key, derived from the name here so no entity is ever stored without one.
            .bind(("key", crate::domain::entity_key(&entity.name)))
            // `Value`'s object variant serializes via `serialize_map`, which
            // the driver's binder handles natively -- unlike `Source`/
            // `Provenance` in `drawers.rs`, no `bindable()` wrapping needed.
            .bind(("properties", entity.properties.clone()))
            .await?
            .check()?;
        Ok(entity)
    }

    /// Create a new, currently-valid relationship (`valid_to: None`) under
    /// the caller-chosen `id`, valid from `valid_from`. A no-op if `id`
    /// already exists — the replay-safe form for a job handler that derives
    /// the id from (job, item index). See [`Self::create_drawer_once`] for
    /// why a replay must not duplicate.
    ///
    /// Returns the relationship as it now stands: the one just written, or
    /// the one an earlier attempt already wrote.
    pub async fn create_relationship(
        &self,
        id: RelationshipId,
        new: NewRelationship,
        valid_from: DateTime<Utc>,
    ) -> Result<Relationship> {
        self.create_relationship_with(id, new, valid_from, None)
            .await
    }

    /// [`Self::create_relationship`] for a fact derived from a drawer: the edge
    /// records `provenance` (which drawer, job and extractor it came from), so
    /// it stays traceable and can be closed when its evidence is superseded.
    pub async fn create_relationship_with(
        &self,
        id: RelationshipId,
        new: NewRelationship,
        valid_from: DateTime<Utc>,
        provenance: Option<FactProvenance>,
    ) -> Result<Relationship> {
        let predicate = require_label("predicate", &new.predicate)?;
        let relationship = Relationship {
            id,
            from: new.from,
            to: new.to,
            predicate,
            confidence: new.confidence,
            valid_from,
            valid_to: None,
            provenance,
        };
        if self.relationship_exists(id).await? {
            return Ok(relationship);
        }
        self.relate(&relationship).await?;
        Ok(relationship)
    }

    /// [`Error::RelationshipNotFound`] unless a `relates_to` edge with this id
    /// exists (open or closed).
    async fn require_relationship(&self, id: RelationshipId) -> Result<()> {
        if self.relationship_exists(id).await? {
            Ok(())
        } else {
            Err(Error::RelationshipNotFound { id: id.to_string() })
        }
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

    /// Close `old_id` (`valid_to = at`) and open a replacement edge for
    /// `new` under the caller-chosen `new_id`, in one transaction —
    /// "atomically where practical" (the issue's wording): the first
    /// explicit `BEGIN`/`COMMIT TRANSACTION` in this codebase, because
    /// superseding a fact should never leave the graph with either two
    /// current edges or zero.
    ///
    /// The whole supersede is skipped if `new_id` already exists — an
    /// earlier attempt already did it, and repeating it would move the old
    /// edge's `valid_to` and open a duplicate current edge. See
    /// [`Self::create_drawer_once`].
    pub async fn supersede_relationship(
        &self,
        old_id: RelationshipId,
        new_id: RelationshipId,
        new: NewRelationship,
        at: DateTime<Utc>,
    ) -> Result<Relationship> {
        let predicate = require_label("predicate", &new.predicate)?;
        let replacement = Relationship {
            id: new_id,
            from: new.from,
            to: new.to,
            predicate,
            confidence: new.confidence,
            valid_from: at,
            valid_to: None,
            // A supersession is somebody's assertion, not an extraction.
            provenance: None,
        };
        if self.relationship_exists(new_id).await? {
            return Ok(replacement);
        }
        // Checked, not assumed: `UPDATE` on an id that is not there reports
        // success having changed nothing, which would leave the replacement
        // opened with no old edge closed — two current edges for one fact.
        self.require_relationship(old_id).await?;

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
            .bind(("now", super::stored(replacement.valid_from)))
            .bind(("new_id", replacement.id.to_string()))
            .bind(("from", replacement.from.to_string()))
            .bind(("to", replacement.to.to_string()))
            .bind(("predicate", replacement.predicate.clone()))
            .bind(("confidence", replacement.confidence))
            .await?
            .check()?;
        Ok(replacement)
    }

    /// Close a relationship (`valid_to = at`) without opening a
    /// replacement — the fact is retracted, not superseded by a new one.
    ///
    /// A soft delete, unlike [`Self::delete_drawer`]'s hard one, on purpose:
    /// a retracted fact is still history someone may ask about ("what did we
    /// believe in March?"), whereas a drawer is deleted only when a person
    /// asks (`drawer delete`, or deleting its room or wing).
    pub async fn invalidate_relationship(
        &self,
        id: RelationshipId,
        at: DateTime<Utc>,
    ) -> Result<()> {
        // Checked first: the `UPDATE` below matches nothing for a missing id
        // and still succeeds, so a mistyped id would read as a retraction
        // that happened. An edge that exists but is already closed is fine —
        // that is a replayed invalidate, and `WHERE !valid_to` keeps its
        // original close time.
        self.require_relationship(id).await?;
        self.db
            // `WHERE !valid_to`: only close an edge that is still open, so
            // repeating an invalidate (a replayed job item) keeps the
            // original close time instead of silently moving it forward.
            .query(
                "UPDATE type::record('relates_to', $id) SET valid_to = $valid_to WHERE !valid_to",
            )
            .bind(("id", id.to_string()))
            .bind(("valid_to", super::stored(at)))
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

    /// The entity with this id, or `None`.
    pub async fn get_entity(&self, id: EntityId) -> Result<Option<Entity>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {ENTITY_COLUMNS} FROM entity WHERE id = type::record('entity', $id)"
            ))
            .bind(("id", id.to_string()))
            .await?;
        let mut rows: Vec<Entity> = super::take_rows(&mut response, 0)?;
        Ok(rows.pop())
    }

    /// The entity called `name` of any kind, preferring a specific kind over
    /// `other`, then the lowest id so the answer is stable. Lets the extraction
    /// job attach a vaguely-typed mention to an entity something better typed
    /// already created, instead of splitting one thing in two.
    pub async fn find_entity_by_name(&self, name: &str) -> Result<Option<Entity>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {ENTITY_COLUMNS} FROM entity WHERE name = $name"
            ))
            .bind(("name", name.to_string()))
            .await?;
        let mut rows: Vec<Entity> = super::take_rows(&mut response, 0)?;
        rows.sort_by(|a, b| {
            (a.kind == "other")
                .cmp(&(b.kind == "other"))
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        });
        Ok(rows.into_iter().next())
    }

    /// Entities, optionally narrowed to a kind and to names containing
    /// `name_contains` (case-insensitive), by name, at most `limit`.
    pub async fn list_entities(
        &self,
        name_contains: Option<&str>,
        kind: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Entity>> {
        let kind = kind.and_then(crate::domain::normalize_label);
        let mut response = self
            .db
            .query(format!(
                "SELECT {ENTITY_COLUMNS} FROM entity \
                 WHERE ($kind = NONE OR kind = $kind) \
                   AND ($needle = NONE OR string::lowercase(name) CONTAINS $needle) \
                 ORDER BY name ASC, id ASC LIMIT $limit"
            ))
            .bind(("kind", kind))
            .bind(("needle", name_contains.map(str::to_lowercase)))
            .bind(("limit", limit))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// Shared write path for a fresh edge: `create_relationship` and
    /// `supersede_relationship`'s replacement half both open a brand-new,
    /// currently-valid `relates_to` record the same way.
    async fn relate(&self, relationship: &Relationship) -> Result<()> {
        self.db
            .query(
                "RELATE (type::record('entity', $from))->relates_to->(type::record('entity', $to)) \
                 SET id = type::record('relates_to', $id), predicate = $predicate, \
                     confidence = $confidence, valid_from = <datetime>$valid_from, valid_to = $valid_to, \
                     provenance = $provenance",
            )
            .bind(("id", relationship.id.to_string()))
            .bind(("from", relationship.from.to_string()))
            .bind(("to", relationship.to.to_string()))
            .bind(("predicate", relationship.predicate.clone()))
            .bind(("confidence", relationship.confidence))
            .bind(("valid_from", super::stored(relationship.valid_from)))
            .bind((
                "valid_to",
                relationship.valid_to.map(super::stored),
            ))
            // `None` binds as `NONE`, which the `option<object>` column wants.
            .bind((
                "provenance",
                relationship
                    .provenance
                    .as_ref()
                    .map(super::bindable)
                    .transpose()?,
            ))
            .await?
            .check()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::error::Error;

    async fn memory_store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    /// A fresh relationship under a random id — what most tests want; the
    /// replay tests choose their own ids.
    async fn open_relationship(
        store: &SurrealStore,
        from: EntityId,
        to: EntityId,
        predicate: &str,
        confidence: f32,
    ) -> Result<Relationship> {
        store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from,
                    to,
                    predicate: predicate.to_string(),
                    confidence,
                },
                Utc::now(),
            )
            .await
    }

    #[tokio::test]
    async fn creating_an_entity_twice_with_the_same_name_and_kind_is_idempotent() {
        let store = memory_store().await;
        let first = store
            .get_or_create_entity("Ada Lovelace", "person", serde_json::json!({}))
            .await
            .expect("create");
        let second = store
            .get_or_create_entity("Ada Lovelace", "person", serde_json::json!({}))
            .await
            .expect("get-or-create");
        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn entity_kind_is_normalized_so_casing_does_not_fragment_the_graph() {
        let store = memory_store().await;
        let first = store
            .get_or_create_entity("Ada Lovelace", "Person", serde_json::json!({}))
            .await
            .expect("create");
        let second = store
            .get_or_create_entity("Ada Lovelace", "person", serde_json::json!({}))
            .await
            .expect("get-or-create despite different casing");
        assert_eq!(first.id, second.id);
        assert_eq!(second.kind, "person");
    }

    #[tokio::test]
    async fn an_empty_kind_or_predicate_is_rejected() {
        let store = memory_store().await;
        let entity_err = store
            .get_or_create_entity("Ada Lovelace", "   ", serde_json::json!({}))
            .await
            .expect_err("blank kind must be rejected");
        assert!(matches!(entity_err, Error::EmptyLabel { .. }));

        let alice = store
            .get_or_create_entity("Alice", "person", serde_json::json!({}))
            .await
            .expect("alice");
        let bob = store
            .get_or_create_entity("Bob", "person", serde_json::json!({}))
            .await
            .expect("bob");
        let relationship_err = open_relationship(&store, alice.id, bob.id, "", 1.0)
            .await
            .expect_err("blank predicate must be rejected");
        assert!(matches!(relationship_err, Error::EmptyLabel { .. }));
    }

    #[tokio::test]
    async fn superseding_a_relationship_closes_the_old_edge_and_leaves_history_queryable() {
        let store = memory_store().await;
        let alice = store
            .get_or_create_entity("Alice", "person", serde_json::json!({}))
            .await
            .expect("alice");
        let acme = store
            .get_or_create_entity("Acme", "organization", serde_json::json!({}))
            .await
            .expect("acme");

        let original = open_relationship(&store, alice.id, acme.id, "employee_of", 0.9)
            .await
            .expect("create relationship");

        let replacement = store
            .supersede_relationship(
                original.id,
                RelationshipId::new(),
                NewRelationship {
                    from: alice.id,
                    to: acme.id,
                    predicate: "former_employee_of".to_string(),
                    confidence: 0.95,
                },
                Utc::now(),
            )
            .await
            .expect("supersede");
        assert_ne!(replacement.id, original.id);

        let current = store
            .list_relationships(alice.id, false)
            .await
            .expect("list current");
        assert_eq!(
            current.len(),
            1,
            "only the replacement should be current, got {current:?}"
        );
        assert_eq!(current[0].id, replacement.id);
        assert_eq!(current[0].predicate, "former_employee_of");

        let history = store
            .list_relationships(alice.id, true)
            .await
            .expect("list including expired");
        assert_eq!(
            history.len(),
            2,
            "both the old and new edge should stay queryable, got {history:?}"
        );
        let old = history
            .iter()
            .find(|r| r.id == original.id)
            .expect("original edge still present");
        assert!(
            old.valid_to.is_some(),
            "the superseded edge must be closed, got {old:?}"
        );
    }

    #[tokio::test]
    async fn invalidating_a_relationship_sets_valid_to_without_creating_a_replacement() {
        let store = memory_store().await;
        let alice = store
            .get_or_create_entity("Alice", "person", serde_json::json!({}))
            .await
            .expect("alice");
        let acme = store
            .get_or_create_entity("Acme", "organization", serde_json::json!({}))
            .await
            .expect("acme");
        let relationship = open_relationship(&store, alice.id, acme.id, "employee_of", 0.9)
            .await
            .expect("create relationship");

        store
            .invalidate_relationship(relationship.id, Utc::now())
            .await
            .expect("invalidate");

        let current = store
            .list_relationships(alice.id, false)
            .await
            .expect("list current");
        assert!(
            current.is_empty(),
            "an invalidated relationship must not read back as current, got {current:?}"
        );

        let history = store
            .list_relationships(alice.id, true)
            .await
            .expect("list including expired");
        assert_eq!(
            history.len(),
            1,
            "invalidating must not create a replacement edge, got {history:?}"
        );
        assert!(history[0].valid_to.is_some());
    }

    async fn two_entities(store: &SurrealStore) -> (EntityId, EntityId) {
        let alice = store
            .get_or_create_entity("Alice", "person", serde_json::json!({}))
            .await
            .unwrap();
        let acme = store
            .get_or_create_entity("Acme", "organization", serde_json::json!({}))
            .await
            .unwrap();
        (alice.id, acme.id)
    }

    fn a_fact(from: EntityId, to: EntityId) -> NewRelationship {
        NewRelationship {
            from,
            to,
            predicate: "employee_of".to_string(),
            confidence: 0.9,
        }
    }

    #[tokio::test]
    async fn invalidating_a_relationship_that_does_not_exist_is_an_error_not_a_silent_no_op() {
        let store = memory_store().await;
        let missing = RelationshipId::new();

        let error = store
            .invalidate_relationship(missing, Utc::now())
            .await
            .expect_err("a mistyped id must not read as a retraction");

        assert!(
            matches!(error, Error::RelationshipNotFound { .. }),
            "{error:?}"
        );
        assert!(
            !store.relationship_exists(missing).await.unwrap(),
            "and the failed call must not have created the record"
        );
    }

    #[tokio::test]
    async fn superseding_a_relationship_that_does_not_exist_is_an_error_and_opens_no_edge() {
        let store = memory_store().await;
        let (alice, acme) = two_entities(&store).await;
        let replacement = RelationshipId::new();

        let error = store
            .supersede_relationship(
                RelationshipId::new(),
                replacement,
                a_fact(alice, acme),
                Utc::now(),
            )
            .await
            .expect_err("there is no old edge to close");

        assert!(
            matches!(error, Error::RelationshipNotFound { .. }),
            "{error:?}"
        );
        assert!(
            !store.relationship_exists(replacement).await.unwrap(),
            "no replacement may be opened when nothing was closed"
        );
    }

    #[tokio::test]
    async fn replaying_an_invalidate_or_a_supersede_is_still_fine() {
        // A resumed checkpoint job repeats the item it crashed in.
        let store = memory_store().await;
        let (alice, acme) = two_entities(&store).await;
        let original = open_relationship(&store, alice, acme, "employee_of", 0.9)
            .await
            .unwrap();
        let replacement = RelationshipId::new();
        store
            .supersede_relationship(original.id, replacement, a_fact(alice, acme), Utc::now())
            .await
            .unwrap();

        // Superseding again: the replacement exists, so it is skipped.
        store
            .supersede_relationship(original.id, replacement, a_fact(alice, acme), Utc::now())
            .await
            .expect("a replayed supersede is a no-op");
        // Invalidating the already-closed original: it exists, so it is fine.
        store
            .invalidate_relationship(original.id, Utc::now())
            .await
            .expect("a replayed invalidate is a no-op");
        assert_eq!(
            store.list_relationships(alice, false).await.unwrap().len(),
            1
        );
    }
}
