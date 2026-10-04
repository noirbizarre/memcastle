//! Entity-resolution repository methods: candidate lookup, resolve-or-create, aliases and review links.
//!
//! The store gathers the entities a name might be and applies `domain::resolve`'s verdict; it holds no matching
//! policy of its own (docs/adr/025). Nothing here merges two existing entities or deletes one: resolution only
//! decides which *existing* entity a new sighting belongs to, and records the sighting's own spelling.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{
    Entity, EntityCandidate, EntityId, Observation, Resolution, ResolutionRule, entity_key,
    require_label, resolve,
};
use crate::error::{Error, Result};

use super::SurrealStore;
use super::entities::ENTITY_COLUMNS;

/// Which side of a `possibly_same_as` edge the other entity is on, from the queried entity's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PossibleSide {
    /// The queried entity was seen later and resembles this older one.
    Older,
    /// This entity was seen later and resembles the queried one.
    Newer,
}

/// An entity another one resembles but was not equated with, awaiting a decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PossibleEntity {
    /// The other entity.
    pub entity: Entity,
    /// Whether it is older or newer than the queried one.
    pub side: PossibleSide,
    /// How alike their names are, in `(0, 1)`.
    pub similarity: f32,
    /// When the resemblance was recorded.
    pub created_at: DateTime<Utc>,
}

/// How many characters shorter or longer than the observed key a candidate's key may be and still be a typo away.
const KEY_LENGTH_BAND: usize = 2;

impl SurrealStore {
    /// The entity `name` of `kind` refers to: an existing one it converges on, or a new one.
    ///
    /// Returns the entity and the [`Observation`] to record on the mention: the name as spelled and the rule that
    /// tied it to the entity (`Exact` for a name that is, or becomes, the entity's own).
    ///
    /// * A spelling variant of a known entity (case, punctuation, a recorded alias, and with `fuzzy` a unique
    ///   one-character typo) converges on it, and the observed spelling is added to its aliases.
    /// * Anything else becomes a new entity. If it resembles existing ones without being equatable to them, a
    ///   `possibly_same_as` edge records each resemblance for later review (with `fuzzy`).
    ///
    /// Replay-safe: resolving the same sighting again finds the entity the first run settled on.
    pub async fn resolve_or_create_entity(
        &self,
        name: &str,
        kind: &str,
        fuzzy: bool,
    ) -> Result<(Entity, Observation)> {
        let kind = require_label("kind", kind)?;
        let key = entity_key(name);
        let key_len = key.chars().count();
        let candidates = self
            .entity_candidates(
                &kind,
                &key,
                key_len.saturating_sub(KEY_LENGTH_BAND),
                key_len + KEY_LENGTH_BAND,
            )
            .await?;
        let known: Vec<EntityCandidate> = candidates
            .iter()
            .map(|e| EntityCandidate {
                id: e.id,
                name: e.name.clone(),
                kind: e.kind.clone(),
                aliases: e.aliases.clone(),
            })
            .collect();

        match resolve(name, &kind, &known, fuzzy) {
            Resolution::Same {
                entity,
                rule,
                confidence,
            } => {
                let mut found = candidates
                    .into_iter()
                    .find(|e| e.id == entity)
                    .expect("resolution only names candidates it was given");
                if found.name != name && !found.aliases.iter().any(|a| a == name) {
                    self.add_alias(found.id, name).await?;
                    found.aliases.push(name.to_string());
                }
                Ok((
                    found,
                    Observation {
                        name: name.to_string(),
                        rule,
                        confidence,
                    },
                ))
            }
            Resolution::Distinct { possible } => {
                let entity = self
                    .get_or_create_entity(name, &kind, serde_json::json!({}))
                    .await?;
                for resemblance in possible {
                    self.link_possibly_same(entity.id, resemblance.entity, resemblance.similarity)
                        .await?;
                }
                Ok((
                    entity,
                    Observation {
                        name: name.to_string(),
                        rule: ResolutionRule::Exact,
                        confidence: 1.0,
                    },
                ))
            }
        }
    }

    /// Record `alias` as another spelling of entity `id`, returning the entity as it now stands, or `None` for an
    /// unknown id. A no-op for the entity's own name or an alias it already has.
    ///
    /// This is the lever for settling an ambiguous resemblance by hand: once a spelling is an alias, every later
    /// sighting of it converges on the entity.
    pub async fn add_entity_alias(&self, id: EntityId, alias: &str) -> Result<Option<Entity>> {
        let Some(entity) = self.get_entity(id).await? else {
            return Ok(None);
        };
        let alias = alias.trim();
        if alias.is_empty() {
            return Err(Error::EmptyLabel {
                field: "alias".to_string(),
            });
        }
        if entity.name != alias && !entity.aliases.iter().any(|a| a == alias) {
            self.add_alias(id, alias).await?;
        }
        self.get_entity(id).await
    }

    /// The entities `name`'s key might belong to: the same key, a recorded alias with it, or a key within the
    /// length band (where a typo can live). Narrowed to `kind` unless the observation is a vague `other`.
    async fn entity_candidates(
        &self,
        kind: &str,
        key: &str,
        min_len: usize,
        max_len: usize,
    ) -> Result<Vec<Entity>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {ENTITY_COLUMNS} FROM entity \
                 WHERE ($kind = 'other' OR kind = $kind) \
                   AND (key = $key OR $key IN alias_keys \
                        OR (key != NONE AND string::len(key) >= $min_len AND string::len(key) <= $max_len) \
                        OR array::len(alias_keys ?? []) > 0) \
                 ORDER BY id ASC"
            ))
            .bind(("kind", kind.to_string()))
            .bind(("key", key.to_string()))
            .bind(("min_len", min_len as u64))
            .bind(("max_len", max_len as u64))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// Append `alias` to an entity's aliases and its key to the alias keys, once.
    async fn add_alias(&self, id: EntityId, alias: &str) -> Result<()> {
        super::retrying_on_conflict(|| async {
            super::checked(
                self.db
                    .query(
                        "UPDATE type::record('entity', $id) SET \
                         aliases = array::union(aliases ?? [], [$alias]), \
                         alias_keys = array::union(alias_keys ?? [], [$alias_key])",
                    )
                    .bind(("id", id.to_string()))
                    .bind(("alias", alias.to_string()))
                    .bind(("alias_key", entity_key(alias)))
                    .await?,
            )
            .map(|_| ())
        })
        .await
    }

    /// Record that `newer` resembles `older` without being equated with it. Idempotent. Returns whether this call
    /// created the edge.
    pub async fn link_possibly_same(
        &self,
        newer: EntityId,
        older: EntityId,
        similarity: f32,
    ) -> Result<bool> {
        if newer == older {
            return Ok(false);
        }
        let mut response = self
            .db
            .query(
                "LET $existing = (SELECT VALUE id FROM possibly_same_as \
                    WHERE in = type::record('entity', $newer) AND out = type::record('entity', $older)); \
                 IF array::len($existing) = 0 { \
                    RELATE (type::record('entity', $newer))->possibly_same_as->(type::record('entity', $older)) \
                      SET similarity = $similarity, created_at = <datetime>$now; \
                    RETURN true; \
                 } ELSE { RETURN false; };",
            )
            .bind(("newer", newer.to_string()))
            .bind(("older", older.to_string()))
            .bind(("similarity", similarity))
            .bind(("now", super::stored(Utc::now())))
            .await?
            .check()?;
        let created: Option<bool> = response.take(response.num_statements() - 1)?;
        Ok(created.unwrap_or(false))
    }

    /// The entities `entity` resembles or is resembled by, most alike first.
    pub async fn list_possible_entities(&self, entity: EntityId) -> Result<Vec<PossibleEntity>> {
        #[derive(Deserialize)]
        struct Row {
            other: EntityId,
            similarity: f32,
            created_at: DateTime<Utc>,
        }
        let mut response = self
            .db
            .query(
                "SELECT record::id(out) AS other, similarity, <string>created_at AS created_at \
                   FROM possibly_same_as WHERE in = type::record('entity', $entity); \
                 SELECT record::id(in) AS other, similarity, <string>created_at AS created_at \
                   FROM possibly_same_as WHERE out = type::record('entity', $entity);",
            )
            .bind(("entity", entity.to_string()))
            .await?;
        let older: Vec<Row> = super::take_rows(&mut response, 0)?;
        let newer: Vec<Row> = super::take_rows(&mut response, 1)?;
        let mut possible = Vec::new();
        for (side, row) in older
            .into_iter()
            .map(|r| (PossibleSide::Older, r))
            .chain(newer.into_iter().map(|r| (PossibleSide::Newer, r)))
        {
            // An entity is never deleted by this code, but a dangling edge must not fail the whole list.
            if let Some(other) = self.get_entity(row.other).await? {
                possible.push(PossibleEntity {
                    entity: other,
                    side,
                    similarity: row.similarity,
                    created_at: row.created_at,
                });
            }
        }
        possible.sort_by(|a, b| {
            b.similarity
                .total_cmp(&a.similarity)
                .then_with(|| a.entity.id.to_string().cmp(&b.entity.id.to_string()))
        });
        Ok(possible)
    }

    /// Fill `key`, `aliases` and `alias_keys` on entities stored before resolution existed. Returns how many were
    /// filled.
    ///
    /// Only for `crate::migrate`. Idempotent: an entity with a key no longer matches. Never merges two entities
    /// that share a key: that call is not made retroactively, and new sightings converge on one of them.
    pub(crate) async fn backfill_entity_keys(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Row {
            id: String,
            name: String,
        }
        let mut response = self
            .db
            .query("SELECT record::id(id) AS id, name FROM entity WHERE !key")
            .await?;
        let rows: Vec<Row> = super::take_rows(&mut response, 0)?;
        let mut filled = 0;
        for row in rows {
            super::retrying_on_conflict(|| async {
                super::checked(
                    self.db
                        .query(
                            "UPDATE type::record('entity', $id) SET key = $key, \
                             aliases = aliases ?? [], alias_keys = alias_keys ?? []",
                        )
                        .bind(("id", row.id.clone()))
                        .bind(("key", entity_key(&row.name)))
                        .await?,
                )
                .map(|_| ())
            })
            .await?;
            filled += 1;
        }
        Ok(filled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    #[tokio::test]
    async fn a_casing_variant_converges_and_the_variant_is_kept_as_an_alias() {
        let store = store().await;
        let (ada, _) = store
            .resolve_or_create_entity("Ada Lovelace", "person", true)
            .await
            .unwrap();
        let (again, observation) = store
            .resolve_or_create_entity("ADA LOVELACE", "person", true)
            .await
            .unwrap();
        assert_eq!(again.id, ada.id);
        assert_eq!(observation.rule, ResolutionRule::Normalized);
        assert_eq!(observation.name, "ADA LOVELACE");
        let stored = store.get_entity(ada.id).await.unwrap().unwrap();
        assert_eq!(
            stored.name, "Ada Lovelace",
            "the canonical name is not rewritten"
        );
        assert_eq!(stored.aliases, vec!["ADA LOVELACE".to_string()]);
        assert_eq!(store.list_entities(None, None, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn resolving_the_same_sighting_twice_changes_nothing() {
        let store = store().await;
        store
            .resolve_or_create_entity("SurrealDB", "tool", true)
            .await
            .unwrap();
        store
            .resolve_or_create_entity("surrealdb", "tool", true)
            .await
            .unwrap();
        store
            .resolve_or_create_entity("surrealdb", "tool", true)
            .await
            .unwrap();
        let entities = store.list_entities(None, None, 10).await.unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].aliases, vec!["surrealdb".to_string()]);
    }

    #[tokio::test]
    async fn a_unique_typo_converges_but_an_ambiguous_one_stays_distinct_with_candidates() {
        let store = store().await;
        let (surreal, _) = store
            .resolve_or_create_entity("SurrealDB", "tool", true)
            .await
            .unwrap();
        let (typo, observation) = store
            .resolve_or_create_entity("SurealDB", "tool", true)
            .await
            .unwrap();
        assert_eq!(typo.id, surreal.id);
        assert_eq!(observation.rule, ResolutionRule::Typo);

        // Two people two edits apart, and a name one edit from each: nothing says which, so it stays its own entity.
        let (first, _) = store
            .resolve_or_create_entity("Katarina", "person", true)
            .await
            .unwrap();
        let (second, _) = store
            .resolve_or_create_entity("Katerino", "person", true)
            .await
            .unwrap();
        assert_ne!(first.id, second.id, "two edits apart is not a typo");
        let (ambiguous, _) = store
            .resolve_or_create_entity("Katerina", "person", true)
            .await
            .unwrap();
        assert_ne!(ambiguous.id, first.id);
        assert_ne!(ambiguous.id, second.id);
        let possible = store.list_possible_entities(ambiguous.id).await.unwrap();
        assert_eq!(possible.len(), 2, "both resemblances are kept for review");
        assert!(possible.iter().all(|p| p.side == PossibleSide::Older));
        let from_the_other_end = store.list_possible_entities(first.id).await.unwrap();
        assert!(
            from_the_other_end
                .iter()
                .any(|p| p.entity.id == ambiguous.id && p.side == PossibleSide::Newer)
        );
    }

    #[tokio::test]
    async fn fuzzy_matching_off_keeps_a_typo_as_its_own_entity() {
        let store = store().await;
        let (surreal, _) = store
            .resolve_or_create_entity("SurrealDB", "tool", false)
            .await
            .unwrap();
        let (typo, _) = store
            .resolve_or_create_entity("SurealDB", "tool", false)
            .await
            .unwrap();
        assert_ne!(typo.id, surreal.id);
        assert!(
            store
                .list_possible_entities(typo.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_alias_added_by_hand_settles_later_sightings() {
        let store = store().await;
        let (castle, _) = store
            .resolve_or_create_entity("MemCastle", "project", true)
            .await
            .unwrap();
        store
            .add_entity_alias(castle.id, "the castle")
            .await
            .unwrap();
        let (found, observation) = store
            .resolve_or_create_entity("The Castle", "project", true)
            .await
            .unwrap();
        assert_eq!(found.id, castle.id);
        assert_eq!(observation.rule, ResolutionRule::Alias);
        assert!(
            store
                .add_entity_alias(EntityId::new(), "x")
                .await
                .unwrap()
                .is_none()
        );
        assert!(store.add_entity_alias(castle.id, "  ").await.is_err());
    }

    #[tokio::test]
    async fn the_entity_backfill_fills_keys_once_and_never_merges() {
        let store = store().await;
        store
            .execute_for_tests(
                "CREATE type::record('entity', '00000000-0000-4000-8000-000000000001') \
                   SET name = 'Ada', kind = 'person', properties = {}; \
                 CREATE type::record('entity', '00000000-0000-4000-8000-000000000002') \
                   SET name = 'ADA', kind = 'person', properties = {};",
            )
            .await;
        assert_eq!(store.backfill_entity_keys().await.unwrap(), 2);
        assert_eq!(store.backfill_entity_keys().await.unwrap(), 0);
        assert_eq!(store.list_entities(None, None, 10).await.unwrap().len(), 2);
        // Older duplicates are left as they are, and a new sighting settles on one of them deterministically.
        let (first, _) = store
            .resolve_or_create_entity("ada", "person", true)
            .await
            .unwrap();
        let (second, _) = store
            .resolve_or_create_entity("aDa", "person", true)
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
    }
}
