//! Durable, replay-safe decisions about graph assertions.

use crate::domain::{
    DrawerId, FactLifecycle, FactLink, FactLinkKind, FactLinkOrigin, FactState, Relationship,
    RelationshipId, infer_fact_link,
};
use crate::error::{Error, Result};
use chrono::{DateTime, Utc};

use super::SurrealStore;

const COLUMNS: &str =
    "from_id AS from, to_id AS to, kind, origin, reason, <string>at AS at, evidence";

impl SurrealStore {
    /// Persist one decision under a deterministic ID. Never move its recorded time on replay.
    pub async fn record_fact_link(&self, link: &FactLink) -> Result<()> {
        let id = link_id(link);
        let result = super::retrying_on_conflict(|| async {
            self.db.query(
            "IF !record::exists(type::record('fact_link', $id)) { \
               CREATE type::record('fact_link', $id) SET from_id = $from, to_id = $to, \
                 kind = $kind, origin = $origin, reason = $reason, at = <datetime>$at, evidence = $evidence; \
             };"
        )
        .bind(("id", id.clone()))
        .bind(("from", link.from.to_string()))
        .bind(("to", link.to.to_string()))
        .bind(("kind", link.kind.as_str()))
        .bind(("origin", match link.origin { FactLinkOrigin::Inferred => "inferred", FactLinkOrigin::Explicit => "explicit" }))
        .bind(("reason", link.reason.clone()))
        .bind(("at", super::stored(link.at)))
        .bind(("evidence", link.evidence.map(|id| id.to_string())))
        .await?.check()?;
            Ok(())
        }).await;
        if let Err(error) = result {
            // A competing extractor may have written this deterministic link after our existence check.
            let mut response = self.db.query(
                "SELECT VALUE record::id(id) FROM fact_link WHERE id = type::record('fact_link', $id)"
            ).bind(("id", id)).await?;
            let existing: Vec<String> = super::take_rows(&mut response, 0)?;
            if existing.is_empty() {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Compare this resolved, extracted edge with assertions on the same subject and predicate.
    /// Comparison is independent of extraction provider and safe to redo after a lost marker.
    pub async fn reconcile_extracted_fact(&self, fact: &Relationship) -> Result<()> {
        if fact.valid_to.is_some() {
            return Ok(());
        }
        let mut response = self
            .db
            .query(format!(
                "SELECT {} FROM relates_to WHERE in = type::record('entity', $subject) \
              AND predicate = $predicate AND id != type::record('relates_to', $id)",
                super::entities::RELATIONSHIP_COLUMNS,
            ))
            .bind(("subject", fact.from.to_string()))
            .bind(("predicate", fact.predicate.clone()))
            .bind(("id", fact.id.to_string()))
            .await?;
        let others: Vec<Relationship> = super::take_rows(&mut response, 0)?;
        for other in others {
            // One drawer repeating a claim is one piece of evidence, not independent confirmation.
            if fact.provenance.as_ref().map(|p| p.drawer)
                == other.provenance.as_ref().map(|p| p.drawer)
            {
                continue;
            }
            let Some(kind) = infer_fact_link(fact, &other) else {
                continue;
            };
            let link = FactLink {
                from: fact.id,
                to: other.id,
                kind,
                origin: FactLinkOrigin::Inferred,
                reason: match kind {
                    FactLinkKind::Confirms => "v1: same resolved subject, predicate and object",
                    _ => "v1: different objects for a single-valued predicate",
                }
                .to_string(),
                at: Utc::now(),
                evidence: fact.provenance.as_ref().map(|p| p.drawer),
            };
            self.record_fact_link(&link).await?;
        }
        Ok(())
    }

    /// A replacement drawer explicitly identifies its predecessor; match resolved claims within that lineage.
    /// The old assertion is already closed at the drawer boundary, but its evidence is not discarded.
    pub async fn link_revised_extraction(
        &self,
        fact: &Relationship,
        predecessor: DrawerId,
    ) -> Result<()> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {} FROM relates_to WHERE provenance.drawer = $drawer \
             AND in = type::record('entity', $subject) AND predicate = $predicate",
                super::entities::RELATIONSHIP_COLUMNS,
            ))
            .bind(("drawer", predecessor.to_string()))
            .bind(("subject", fact.from.to_string()))
            .bind(("predicate", fact.predicate.clone()))
            .await?;
        let previous: Vec<Relationship> = super::take_rows(&mut response, 0)?;
        for old in previous {
            self.record_fact_link(&FactLink {
                from: fact.id,
                to: old.id,
                kind: FactLinkKind::Supersedes,
                origin: FactLinkOrigin::Inferred,
                reason: "v1: successor drawer replaced this source's earlier assertion".into(),
                at: Utc::now(),
                evidence: fact.provenance.as_ref().map(|p| p.drawer),
            })
            .await?;
        }
        Ok(())
    }

    /// Retrieve every recorded link touching any of these assertions in one query.
    pub async fn fact_links(&self, ids: &[RelationshipId]) -> Result<Vec<FactLink>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let mut response = self.db.query(format!(
            "SELECT {COLUMNS} FROM fact_link WHERE from_id IN $ids OR to_id IN $ids ORDER BY at ASC, id ASC"
        )).bind(("ids", ids)).await?;
        super::take_rows(&mut response, 0)
    }

    /// Decorate a batch of assertions using durable events, without an N+1 graph read.
    pub async fn attach_lifecycle(
        &self,
        facts: &mut [Relationship],
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.attach_lifecycle_with_preferences(
            facts,
            at,
            &crate::domain::SourcePreferences::default(),
        )
        .await
    }

    /// Read-side conflict hint: preserve every assertion while explaining which evidence is currently strongest.
    pub async fn attach_lifecycle_with_preferences(
        &self,
        facts: &mut [Relationship],
        at: DateTime<Utc>,
        preferences: &crate::domain::SourcePreferences,
    ) -> Result<()> {
        let links = self
            .fact_links(&facts.iter().map(|f| f.id).collect::<Vec<_>>())
            .await?;
        let peers: Vec<String> = links
            .iter()
            .flat_map(|link| [link.from.to_string(), link.to.to_string()])
            .collect();
        let mut response = self
            .db
            .query(format!(
                "SELECT {} FROM relates_to WHERE record::id(id) IN $ids",
                super::entities::RELATIONSHIP_COLUMNS,
            ))
            .bind(("ids", peers))
            .await?;
        let candidates: Vec<Relationship> = super::take_rows(&mut response, 0)?;
        for fact in facts {
            let relevant: Vec<_> = links
                .iter()
                // Inferred comparisons describe the facts' validity, even if extraction discovered them later;
                // explicit corrections are choices made at their recorded instant, never retroactive.
                .filter(|link| {
                    (link.origin == FactLinkOrigin::Inferred || link.at <= at)
                        && (link.from == fact.id || link.to == fact.id)
                })
                .cloned()
                .collect();
            let state = if fact.valid_from > at {
                FactState::Historical
            } else if fact.valid_to.is_some_and(|end| end <= at) {
                if relevant
                    .iter()
                    .any(|l| l.to == fact.id && l.kind == FactLinkKind::Supersedes)
                {
                    FactState::Superseded
                } else if relevant
                    .iter()
                    .any(|l| l.from == fact.id && l.kind == FactLinkKind::Invalidates)
                {
                    FactState::Invalidated
                } else {
                    FactState::Historical
                }
            } else if relevant.iter().any(|link| {
                link.kind == FactLinkKind::Contradicts
                    && candidates.iter().any(|other| {
                        other.id != fact.id
                            && (other.id == link.from || other.id == link.to)
                            && other.valid_from <= at
                            && other.valid_to.is_none_or(|end| end > at)
                    })
            }) {
                FactState::Conflicting
            } else {
                FactState::Current
            };
            let preferred = (state == FactState::Conflicting)
                .then(|| {
                    candidates
                        .iter()
                        .filter(|peer| {
                            peer.valid_from <= at
                                && peer.valid_to.is_none_or(|end| end > at)
                                && (peer.id == fact.id
                                    || relevant.iter().any(|link| {
                                        link.kind == FactLinkKind::Contradicts
                                            && (link.from == peer.id || link.to == peer.id)
                                    }))
                        })
                        .max_by(|a, b| {
                            let explicit = |edge: &Relationship| {
                                edge.assertion.is_some() || edge.provenance.is_none()
                            };
                            let quality = |edge: &Relationship| {
                                if preferences.sources.is_empty()
                                    && preferences.default == Default::default()
                                {
                                    return edge.confidence;
                                }
                                let origin =
                                    edge.provenance.as_ref().and_then(|p| p.origin.as_ref());
                                let occurred_at = origin
                                    .and_then(|origin| origin.occurred_at)
                                    .unwrap_or(edge.valid_from);
                                let age = (at - occurred_at).num_days().max(0) as f32;
                                let freshness = 0.12 / (1.0 + age / 30.0);
                                edge.confidence
                                    + freshness
                                    + preferences.resolve(origin).level.adjustment() * 0.5
                            };
                            explicit(a)
                                .cmp(&explicit(b))
                                .then_with(|| quality(a).total_cmp(&quality(b)))
                                .then_with(|| a.valid_from.cmp(&b.valid_from))
                                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
                        })
                        .map(|edge| edge.id)
                })
                .flatten();
            fact.lifecycle = Some(FactLifecycle {
                state,
                links: relevant,
                preferred,
                basis: preferred.map(|id| {
                    if preferences.sources.is_empty() && preferences.default == Default::default() {
                        return "explicit assertion, then confidence, validity start, fact ID (ranking only; conflict remains unresolved)".to_string();
                    }
                    let matched = candidates.iter().find(|candidate| candidate.id == id).map(|candidate| {
                        preferences.resolve(candidate.provenance.as_ref().and_then(|p| p.origin.as_ref()))
                    });
                    format!("explicit assertion, then confidence + bounded freshness + source preference ({matched:?}), validity start, fact ID (ranking only; conflict remains unresolved)")
                }),
            });
        }
        Ok(())
    }

    /// The assertion and its linked evidence, including closed assertions, at an optional point in time.
    pub async fn fact_history(
        &self,
        id: RelationshipId,
        at: Option<DateTime<Utc>>,
    ) -> Result<Vec<Relationship>> {
        self.fact_history_with_preferences(id, at, &crate::domain::SourcePreferences::default())
            .await
    }

    /// Fetch a fact's history and explain conflicts using the served palace's effective policy.
    pub async fn fact_history_with_preferences(
        &self,
        id: RelationshipId,
        at: Option<DateTime<Utc>>,
        preferences: &crate::domain::SourcePreferences,
    ) -> Result<Vec<Relationship>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {} FROM relates_to WHERE id = type::record('relates_to', $id)",
                super::entities::RELATIONSHIP_COLUMNS
            ))
            .bind(("id", id.to_string()))
            .await?;
        let facts: Vec<Relationship> = super::take_rows(&mut response, 0)?;
        if facts.is_empty() {
            return Err(Error::RelationshipNotFound { id: id.to_string() });
        }
        let mut ids = vec![id];
        // Walk explicit supersession and supporting decisions, bounded and cycle-safe.
        for _ in 0..100 {
            let mut added = false;
            for link in self.fact_links(&ids).await? {
                for candidate in [link.from, link.to] {
                    if !ids.contains(&candidate) {
                        ids.push(candidate);
                        added = true;
                    }
                }
            }
            if !added {
                break;
            }
        }
        let id_strings: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let mut response = self.db.query(format!(
            "SELECT {} FROM relates_to WHERE record::id(id) IN $ids ORDER BY valid_from ASC, id ASC",
            super::entities::RELATIONSHIP_COLUMNS
        )).bind(("ids", id_strings)).await?;
        let mut facts: Vec<Relationship> = super::take_rows(&mut response, 0)?;
        if let Some(at) = at {
            facts.retain(|fact| fact.valid_from <= at);
        }
        self.attach_lifecycle_with_preferences(
            &mut facts,
            at.unwrap_or_else(Utc::now),
            preferences,
        )
        .await?;
        Ok(facts)
    }
}

pub(super) fn link_id(link: &FactLink) -> String {
    let (a, b) = if matches!(
        link.kind,
        FactLinkKind::Confirms | FactLinkKind::Contradicts
    ) && link.from.to_string() > link.to.to_string()
    {
        (link.to, link.from)
    } else {
        (link.from, link.to)
    };
    let origin = match link.origin {
        FactLinkOrigin::Inferred => "inferred".to_string(),
        FactLinkOrigin::Explicit => format!(
            "explicit:{}",
            link.evidence
                .map_or_else(|| "legacy".to_string(), |id| id.to_string())
        ),
    };
    RelationshipId::derive(
        a.0,
        &format!("fact-link:{}:{origin}:{b}", link.kind.as_str()),
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DrawerId, FactProvenance, NewRelationship, Predicate};
    use serde_json::json;

    #[tokio::test]
    async fn source_preference_shapes_a_conflict_without_resolving_or_hiding_either_fact() {
        use crate::domain::{
            ConnectorPreference, Origin, PreferenceLevel, SourceId, SourcePreferences,
        };

        let store = SurrealStore::connect_memory_for_tests().await;
        let subject = store
            .get_or_create_entity("Ada", "person", json!({}))
            .await
            .unwrap()
            .id;
        let paris = store
            .get_or_create_entity("Paris", "place", json!({}))
            .await
            .unwrap()
            .id;
        let london = store
            .get_or_create_entity("London", "place", json!({}))
            .await
            .unwrap()
            .id;
        let mut facts = Vec::new();
        for (source, to) in [("ordinary", paris), ("favored", london)] {
            let fact = store
                .create_relationship_with(
                    RelationshipId::new(),
                    NewRelationship {
                        from: subject,
                        to,
                        predicate: "located_in".into(),
                        confidence: 0.8,
                    },
                    Utc::now(),
                    Some(FactProvenance {
                        drawer: DrawerId::new(),
                        origin: Some(Origin {
                            source_id: SourceId::new(),
                            source: source.into(),
                            document: "d".into(),
                            chunk: 0,
                            revision: "r".into(),
                            metadata: None,
                            occurred_at: Some(Utc::now()),
                        }),
                        job_id: None,
                        extractor: "test".into(),
                        extracted_at: Utc::now(),
                    }),
                )
                .await
                .unwrap();
            store.reconcile_extracted_fact(&fact).await.unwrap();
            facts.push(fact);
        }
        let mut policy = SourcePreferences::default();
        policy.sources.insert(
            "favored".into(),
            ConnectorPreference {
                level: Some(PreferenceLevel::High),
                ..Default::default()
            },
        );
        store
            .attach_lifecycle_with_preferences(&mut facts, Utc::now(), &policy)
            .await
            .unwrap();
        assert!(
            facts
                .iter()
                .all(|fact| fact.lifecycle.as_ref().unwrap().state == FactState::Conflicting)
        );
        assert!(
            facts
                .iter()
                .all(|fact| fact.lifecycle.as_ref().unwrap().preferred == Some(facts[1].id))
        );
        assert!(
            facts[0]
                .lifecycle
                .as_ref()
                .unwrap()
                .basis
                .as_ref()
                .unwrap()
                .contains("source preference")
        );

        let explicit = store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from: subject,
                    to: paris,
                    predicate: "located_in".into(),
                    confidence: 0.1,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        store.reconcile_extracted_fact(&explicit).await.unwrap();
        let explicit_id = explicit.id;
        facts.push(explicit);
        store
            .attach_lifecycle_with_preferences(&mut facts, Utc::now(), &policy)
            .await
            .unwrap();
        assert_eq!(
            facts[1].lifecycle.as_ref().unwrap().preferred,
            Some(explicit_id),
            "an explicit assertion must outrank even favored extracted evidence"
        );
    }

    #[tokio::test]
    async fn independent_evidence_confirms_and_disagreement_remains_visible() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let ada = store
            .get_or_create_entity("Ada", "person", json!({}))
            .await
            .unwrap()
            .id;
        let a = store
            .get_or_create_entity("Paris", "place", json!({}))
            .await
            .unwrap()
            .id;
        let b = store
            .get_or_create_entity("London", "place", json!({}))
            .await
            .unwrap()
            .id;
        let mut facts = Vec::new();
        for to in [a, a, b] {
            let fact = store
                .create_relationship_with(
                    RelationshipId::new(),
                    NewRelationship {
                        from: ada,
                        to,
                        predicate: Predicate::LocatedIn.as_str().into(),
                        confidence: 0.8,
                    },
                    Utc::now(),
                    Some(FactProvenance {
                        drawer: DrawerId::new(),
                        origin: None,
                        job_id: None,
                        extractor: "test".into(),
                        extracted_at: Utc::now(),
                    }),
                )
                .await
                .unwrap();
            store.reconcile_extracted_fact(&fact).await.unwrap();
            facts.push(fact);
        }
        let current = store.list_relationships(ada, false).await.unwrap();
        assert_eq!(current.len(), 3);
        assert!(
            current
                .iter()
                .all(|f| f.lifecycle.as_ref().unwrap().state == FactState::Conflicting)
        );
        let history = store.fact_history(facts[0].id, None).await.unwrap();
        assert_eq!(history.len(), 3);
        assert!(
            history
                .iter()
                .flat_map(|f| &f.lifecycle.as_ref().unwrap().links)
                .any(|link| link.kind == FactLinkKind::Confirms)
        );
        assert!(
            history
                .iter()
                .flat_map(|f| &f.lifecycle.as_ref().unwrap().links)
                .any(|link| link.kind == FactLinkKind::Contradicts)
        );
        let before = store.fact_links(&[facts[2].id]).await.unwrap();
        store.reconcile_extracted_fact(&facts[2]).await.unwrap();
        let after = store.fact_links(&[facts[2].id]).await.unwrap();
        assert_eq!(
            serde_json::to_value(before).unwrap(),
            serde_json::to_value(after).unwrap(),
            "a lost extraction marker must not move or duplicate recorded decisions"
        );
    }

    #[tokio::test]
    async fn correction_is_durable_and_replay_keeps_the_original_boundary() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let subject = store
            .get_or_create_entity("Ada", "person", json!({}))
            .await
            .unwrap()
            .id;
        let object = store
            .get_or_create_entity("Paris", "place", json!({}))
            .await
            .unwrap()
            .id;
        let new = NewRelationship {
            from: subject,
            to: object,
            predicate: "located_in".into(),
            confidence: 1.0,
        };
        let old = store
            .create_relationship(RelationshipId::new(), new.clone(), Utc::now())
            .await
            .unwrap();
        let boundary = Utc::now();
        let replacement = RelationshipId::new();
        store
            .supersede_relationship(old.id, replacement, new.clone(), boundary)
            .await
            .unwrap();
        store
            .supersede_relationship(old.id, replacement, new, Utc::now())
            .await
            .unwrap();
        let history = store.fact_history(old.id, None).await.unwrap();
        assert_eq!(history.len(), 2);
        let previous = history.iter().find(|f| f.id == old.id).unwrap();
        assert_eq!(previous.valid_to, Some(boundary));
        assert_eq!(
            previous.lifecycle.as_ref().unwrap().state,
            FactState::Superseded
        );
        assert_eq!(previous.lifecycle.as_ref().unwrap().links.len(), 1);
        let at_boundary = store.fact_history(old.id, Some(boundary)).await.unwrap();
        assert_eq!(
            at_boundary
                .iter()
                .filter(|f| f.valid_from <= boundary && f.valid_to.is_none_or(|end| end > boundary))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn ending_one_disputed_assertion_restores_current_state_without_erasing_the_conflict() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let ada = store
            .get_or_create_entity("Ada", "person", json!({}))
            .await
            .unwrap()
            .id;
        let paris = store
            .get_or_create_entity("Paris", "place", json!({}))
            .await
            .unwrap()
            .id;
        let london = store
            .get_or_create_entity("London", "place", json!({}))
            .await
            .unwrap()
            .id;
        let before = Utc::now() - chrono::Duration::minutes(2);
        let make = |to| NewRelationship {
            from: ada,
            to,
            predicate: "located_in".into(),
            confidence: 0.8,
        };
        let first = store
            .create_relationship(RelationshipId::new(), make(paris), before)
            .await
            .unwrap();
        let second = store
            .create_relationship_with(
                RelationshipId::new(),
                make(london),
                before,
                Some(FactProvenance {
                    drawer: DrawerId::new(),
                    origin: None,
                    job_id: None,
                    extractor: "test".into(),
                    extracted_at: Utc::now(),
                }),
            )
            .await
            .unwrap();
        store.reconcile_extracted_fact(&second).await.unwrap();
        let during = Utc::now();
        let disputed = store.get_relationship(first.id).await.unwrap().unwrap();
        assert_eq!(
            disputed.lifecycle.as_ref().unwrap().preferred,
            Some(first.id),
            "direct assertion outranks inferred evidence for display even when it did not arrive last"
        );
        store
            .invalidate_relationship(second.id, during)
            .await
            .unwrap();
        let current = store.get_relationship(first.id).await.unwrap().unwrap();
        assert_eq!(
            current.lifecycle.as_ref().unwrap().state,
            FactState::Current
        );
        let old = store
            .fact_history(first.id, Some(during - chrono::Duration::nanoseconds(1)))
            .await
            .unwrap();
        assert!(
            old.iter()
                .all(|f| f.lifecycle.as_ref().unwrap().state == FactState::Conflicting)
        );
        let history = store.fact_history(second.id, None).await.unwrap();
        assert_eq!(
            history
                .iter()
                .find(|f| f.id == second.id)
                .unwrap()
                .lifecycle
                .as_ref()
                .unwrap()
                .state,
            FactState::Invalidated
        );
    }
}
