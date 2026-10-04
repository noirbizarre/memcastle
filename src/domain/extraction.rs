//! What an extractor says about a piece of text, and the rules that make it safe to store.
//!
//! Pure types: no I/O, no provider. An extractor (heuristic, a command, an LLM over HTTP) returns an
//! [`ExtractedGraph`]; [`ExtractedGraph::normalise`] is the one place where its answer is held to the closed
//! vocabulary and to sane bounds, so a provider can stay dumb and a misbehaving model cannot write garbage into the
//! graph. Extraction is derived data: nothing here refers to rewriting a drawer.

use serde::{Deserialize, Serialize};

use super::{EntityKind, Predicate};

/// One entity an extractor found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedEntity {
    /// The entity's name as it appears in the text.
    pub name: String,
    /// Its kind, already inside the closed vocabulary.
    pub kind: EntityKind,
}

/// One relationship an extractor found between two of its entities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedRelation {
    /// The subject's name (matches an [`ExtractedEntity::name`]).
    pub subject: String,
    /// The label, inside the closed vocabulary.
    pub predicate: Predicate,
    /// The object's name (matches an [`ExtractedEntity::name`]).
    pub object: String,
    /// How sure the extractor is, in `[0, 1]`.
    pub confidence: f32,
}

/// Everything an extractor found in one text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtractedGraph {
    /// The entities found.
    pub entities: Vec<ExtractedEntity>,
    /// The relationships found between them.
    pub relations: Vec<ExtractedRelation>,
}

/// The bounds [`ExtractedGraph::normalise`] enforces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// The most entities kept from one text.
    pub max_entities: usize,
    /// The most relationships kept from one text.
    pub max_relations: usize,
    /// Relationships below this confidence are dropped.
    pub min_confidence: f32,
}

/// The longest entity name kept, in characters: a "name" longer than this is a sentence the extractor misread.
pub const MAX_NAME_CHARS: usize = 120;

/// Collapse runs of whitespace into single spaces and trim, so `"Ada  Lovelace "` and `"Ada Lovelace"` are one name.
fn clean_name(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl ExtractedGraph {
    /// Hold an extractor's answer to the rules the graph relies on.
    ///
    /// - Names are whitespace-collapsed; empty and over-long names are dropped.
    /// - Entities are de-duplicated by case-insensitive name (the first one's kind wins).
    /// - A relation survives only if both endpoints are kept entities, they differ, and its confidence is finite
    ///   (then clamped to `[0, 1]`) and at least `min_confidence`.
    /// - Relations are de-duplicated by `(subject, predicate, object)`, keeping the most confident.
    /// - Sizes are capped, most confident relations first.
    ///
    /// The order of what is kept is deterministic, so two runs over the same text store the same thing.
    #[must_use]
    pub fn normalise(self, limits: Limits) -> Self {
        let mut entities: Vec<ExtractedEntity> = Vec::new();
        for entity in self.entities {
            let name = clean_name(&entity.name);
            if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
                continue;
            }
            if entities
                .iter()
                .any(|kept| kept.name.eq_ignore_ascii_case(&name))
            {
                continue;
            }
            entities.push(ExtractedEntity {
                name,
                kind: entity.kind,
            });
        }
        entities.truncate(limits.max_entities);

        // The canonical spelling of a name, so a relation written "ada" binds to the entity "Ada".
        let canonical = |name: &str| -> Option<String> {
            let name = clean_name(name);
            entities
                .iter()
                .find(|kept| kept.name.eq_ignore_ascii_case(&name))
                .map(|kept| kept.name.clone())
        };

        let mut relations: Vec<ExtractedRelation> = Vec::new();
        for relation in self.relations {
            if !relation.confidence.is_finite() {
                continue;
            }
            let confidence = relation.confidence.clamp(0.0, 1.0);
            if confidence < limits.min_confidence {
                continue;
            }
            let (Some(subject), Some(object)) =
                (canonical(&relation.subject), canonical(&relation.object))
            else {
                continue;
            };
            if subject == object {
                continue;
            }
            let existing = relations.iter_mut().find(|kept| {
                kept.subject == subject
                    && kept.object == object
                    && kept.predicate == relation.predicate
            });
            match existing {
                Some(kept) => kept.confidence = kept.confidence.max(confidence),
                None => relations.push(ExtractedRelation {
                    subject,
                    predicate: relation.predicate,
                    object,
                    confidence,
                }),
            }
        }
        // Most confident first, ties by name, so the cap keeps the best and the order never depends on the input's.
        relations.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| a.subject.cmp(&b.subject))
                .then_with(|| a.predicate.as_str().cmp(b.predicate.as_str()))
                .then_with(|| a.object.cmp(&b.object))
        });
        relations.truncate(limits.max_relations);

        Self {
            entities,
            relations,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_entities: 10,
        max_relations: 10,
        min_confidence: 0.0,
    };

    fn entity(name: &str, kind: EntityKind) -> ExtractedEntity {
        ExtractedEntity {
            name: name.into(),
            kind,
        }
    }

    fn relation(
        subject: &str,
        predicate: Predicate,
        object: &str,
        confidence: f32,
    ) -> ExtractedRelation {
        ExtractedRelation {
            subject: subject.into(),
            predicate,
            object: object.into(),
            confidence,
        }
    }

    fn graph(entities: Vec<ExtractedEntity>, relations: Vec<ExtractedRelation>) -> ExtractedGraph {
        ExtractedGraph {
            entities,
            relations,
        }
    }

    #[test]
    fn a_relation_with_an_endpoint_that_is_not_an_entity_is_dropped() {
        let out = graph(
            vec![entity("Ada", EntityKind::Person)],
            vec![relation("Ada", Predicate::WorksOn, "Ghost", 0.9)],
        )
        .normalise(LIMITS);
        assert!(out.relations.is_empty());
    }

    #[test]
    fn a_relation_from_an_entity_to_itself_is_dropped() {
        let out = graph(
            vec![entity("Ada", EntityKind::Person)],
            vec![relation("Ada", Predicate::RelatedTo, "ada", 0.9)],
        )
        .normalise(LIMITS);
        assert!(out.relations.is_empty());
    }

    #[test]
    fn empty_and_overlong_names_are_dropped_and_whitespace_is_collapsed() {
        let out = graph(
            vec![
                entity("   ", EntityKind::Other),
                entity(&"x".repeat(MAX_NAME_CHARS + 1), EntityKind::Other),
                entity("  Ada \n Lovelace ", EntityKind::Person),
            ],
            vec![],
        )
        .normalise(LIMITS);
        assert_eq!(
            out.entities,
            vec![entity("Ada Lovelace", EntityKind::Person)]
        );
    }

    #[test]
    fn entities_differing_only_in_case_are_one_entity_and_the_first_kind_wins() {
        let out = graph(
            vec![
                entity("Rust", EntityKind::Tool),
                entity("rust", EntityKind::Concept),
            ],
            vec![],
        )
        .normalise(LIMITS);
        assert_eq!(out.entities, vec![entity("Rust", EntityKind::Tool)]);
    }

    #[test]
    fn a_non_finite_confidence_is_dropped_and_an_out_of_range_one_is_clamped() {
        let out = graph(
            vec![
                entity("A", EntityKind::Other),
                entity("B", EntityKind::Other),
                entity("C", EntityKind::Other),
            ],
            vec![
                relation("A", Predicate::Uses, "B", f32::NAN),
                relation("A", Predicate::Uses, "C", 7.0),
            ],
        )
        .normalise(LIMITS);
        assert_eq!(out.relations.len(), 1);
        assert!((out.relations[0].confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn relations_below_the_minimum_confidence_are_dropped() {
        let out = graph(
            vec![
                entity("A", EntityKind::Other),
                entity("B", EntityKind::Other),
            ],
            vec![relation("A", Predicate::Uses, "B", 0.3)],
        )
        .normalise(Limits {
            min_confidence: 0.5,
            ..LIMITS
        });
        assert!(out.relations.is_empty());
    }

    #[test]
    fn a_repeated_relation_is_kept_once_at_its_highest_confidence() {
        let out = graph(
            vec![
                entity("A", EntityKind::Other),
                entity("B", EntityKind::Other),
            ],
            vec![
                relation("A", Predicate::Uses, "B", 0.4),
                relation("a", Predicate::Uses, "b", 0.8),
            ],
        )
        .normalise(LIMITS);
        assert_eq!(out.relations.len(), 1);
        assert!((out.relations[0].confidence - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn sizes_are_capped_keeping_the_most_confident_relations() {
        let out = graph(
            vec![
                entity("A", EntityKind::Other),
                entity("B", EntityKind::Other),
                entity("C", EntityKind::Other),
            ],
            vec![
                relation("A", Predicate::Uses, "B", 0.2),
                relation("A", Predicate::Uses, "C", 0.9),
            ],
        )
        .normalise(Limits {
            max_entities: 3,
            max_relations: 1,
            min_confidence: 0.0,
        });
        assert_eq!(out.relations.len(), 1);
        assert_eq!(out.relations[0].object, "C");
    }

    #[test]
    fn normalising_the_same_answer_twice_gives_the_same_graph() {
        let input = graph(
            vec![
                entity("A", EntityKind::Other),
                entity("B", EntityKind::Other),
            ],
            vec![relation("A", Predicate::Uses, "B", 0.5)],
        );
        assert_eq!(input.clone().normalise(LIMITS), input.normalise(LIMITS));
    }
}
