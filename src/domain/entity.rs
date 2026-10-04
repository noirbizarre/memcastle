//! Temporal knowledge-graph types.
//!
//! Schema-ready and, as of `store::entities`, schema-wired: the SurrealDB
//! tables and graph edge these map to are defined in
//! `database/schema/palace.surql`, and `store::entities` reads and writes
//! them. Two things populate them: a checkpoint item's optional `fact`
//! mutation (`checkpoint::apply_fact_mutation`), whose labels stay free text,
//! and the extraction job (`extract::job`), which reads mined drawers and
//! writes only labels from the closed vocabulary below ([`EntityKind`],
//! [`Predicate`]), each edge carrying the [`FactProvenance`] that says which
//! drawer it was derived from. Extraction adds graph data; it never rewrites
//! the drawers it read.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{DrawerId, EntityId, JobId, Origin, RelationshipId, ResolutionRule};

/// A named thing the palace has opinions about (a person, a project, a term).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier.
    pub id: EntityId,
    /// Canonical display name.
    pub name: String,
    /// Free-text classification (`"person"`, `"project"`, ...).
    pub kind: String,
    /// Arbitrary additional attributes.
    pub properties: Value,
    /// Other spellings this entity was observed under or given, never including `name`. Recorded when entity
    /// resolution converges a variant on it, so the source's own spelling is not lost (docs/adr/025).
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// A directed, bi-temporal edge between two entities.
///
/// `valid_to: None` means "still true." Superseding a fact means closing the
/// old edge (`valid_to = now`) and opening a new one — never mutating a
/// still-open edge in place — so an `as_of` query before the boundary keeps
/// seeing the original claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    /// Unique identifier — needed so `supersede`/`invalidate` can target one
    /// specific edge rather than "the edge between these two entities" (an
    /// entity pair can have more than one predicate, and the same predicate
    /// more than once across time).
    pub id: RelationshipId,
    /// The subject of the relationship.
    pub from: EntityId,
    /// The object of the relationship.
    pub to: EntityId,
    /// The relationship's label. Free text for facts a person or agent
    /// asserts (normalised by [`normalize_label`]); a [`Predicate`] for facts
    /// the extraction job derives, so a model's free-form phrasing cannot
    /// fragment the graph (MemPalace's `kg_normalize` lesson).
    pub predicate: String,
    /// Confidence in `[0, 1]`.
    pub confidence: f32,
    /// When this fact became true.
    pub valid_from: DateTime<Utc>,
    /// When this fact stopped being true, if it has been superseded.
    pub valid_to: Option<DateTime<Utc>>,
    /// What the fact was derived from. `None` for a fact somebody asserted
    /// directly (a checkpoint), `Some` for one the extraction job derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FactProvenance>,
}

/// Where an extracted fact (or an entity mention) came from: the evidence,
/// the run that read it and the extractor that judged it.
///
/// Stored on `mentions` and `relates_to` edges. The drawer is the evidence and
/// stays canonical: this is a pointer to it, never a copy of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactProvenance {
    /// The drawer whose content the fact was read from.
    pub drawer: DrawerId,
    /// Where that drawer came from in its mining source, when it has an origin
    /// (copied so the fact can be traced without a second lookup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// The extraction job that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// The extractor that produced it (`heuristic`, `command`, `http`, ...).
    pub extractor: String,
    /// When it was extracted.
    pub extracted_at: DateTime<Utc>,
}

/// One `mentions` edge, read from the entity's side: a drawer that talks about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    /// The drawer that mentions the entity.
    pub drawer: DrawerId,
    /// When the link was made.
    pub created_at: DateTime<Utc>,
    /// Where an extracted link came from; `None` for one a person made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<FactProvenance>,
    /// How the drawer spelled the entity and why that spelling was taken to be it; `None` for a link made before
    /// entity resolution existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
}

/// One sighting of an entity in a drawer: the name exactly as the source wrote it and how it was resolved.
///
/// Stored on the `mentions` edge so that resolving two spellings to one entity loses neither: the drawer says what
/// it said, and this says which entity that was taken to mean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// The name as the source spelled it.
    pub name: String,
    /// Why that name was taken to be this entity.
    pub rule: ResolutionRule,
    /// How sure, in `(0, 1]`.
    pub confidence: f32,
}

/// The closed set of entity kinds the extraction job may write.
///
/// Anything an extractor says that is not one of these becomes
/// [`EntityKind::Other`], so "Person", "human" and "employee" cannot become
/// three kinds of the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    /// A human being.
    Person,
    /// A company, team or other group.
    Organization,
    /// A project, product or repository.
    Project,
    /// A tool, library, service or technology.
    Tool,
    /// A physical or virtual place.
    Place,
    /// An abstract idea or term.
    Concept,
    /// Anything that fits none of the above.
    Other,
}

impl EntityKind {
    /// Every kind, in a stable order (the vocabulary handed to an extractor).
    pub const ALL: [Self; 7] = [
        Self::Person,
        Self::Organization,
        Self::Project,
        Self::Tool,
        Self::Place,
        Self::Concept,
        Self::Other,
    ];

    /// The stored label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Organization => "organization",
            Self::Project => "project",
            Self::Tool => "tool",
            Self::Place => "place",
            Self::Concept => "concept",
            Self::Other => "other",
        }
    }

    /// Read a label an extractor produced; anything outside the vocabulary is
    /// [`Self::Other`] rather than a new kind.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let label = normalize_label(raw).unwrap_or_default();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == label)
            .unwrap_or(Self::Other)
    }
}

/// The closed set of predicates the extraction job may write.
///
/// An unknown phrase becomes [`Predicate::RelatedTo`], the deliberately weak
/// fallback, rather than a new label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    /// A person works on a project.
    WorksOn,
    /// A person or team belongs to an organization or team.
    MemberOf,
    /// One thing depends on another.
    DependsOn,
    /// One thing uses another.
    Uses,
    /// One thing owns or maintains another.
    Owns,
    /// One thing is a part of another.
    PartOf,
    /// One thing is located in another.
    LocatedIn,
    /// The fallback: they are connected, and nothing more is claimed.
    RelatedTo,
}

impl Predicate {
    /// Every predicate, in a stable order (the vocabulary handed to an extractor).
    pub const ALL: [Self; 8] = [
        Self::WorksOn,
        Self::MemberOf,
        Self::DependsOn,
        Self::Uses,
        Self::Owns,
        Self::PartOf,
        Self::LocatedIn,
        Self::RelatedTo,
    ];

    /// The stored label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorksOn => "works_on",
            Self::MemberOf => "member_of",
            Self::DependsOn => "depends_on",
            Self::Uses => "uses",
            Self::Owns => "owns",
            Self::PartOf => "part_of",
            Self::LocatedIn => "located_in",
            Self::RelatedTo => "related_to",
        }
    }

    /// Read a label an extractor produced. Spaces and hyphens read as
    /// underscores (`"works on"`, `"depends-on"`); anything outside the
    /// vocabulary is [`Self::RelatedTo`].
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let label = normalize_label(raw)
            .unwrap_or_default()
            .replace([' ', '-'], "_");
        Self::ALL
            .into_iter()
            .find(|predicate| predicate.as_str() == label)
            .unwrap_or(Self::RelatedTo)
    }
}

/// Everything needed to describe a new fact, for
/// [`SurrealStore::supersede_relationship`](crate::store::SurrealStore::supersede_relationship) —
/// no `id` (it is passed alongside, chosen by the caller so a replay reuses it) and no temporal fields (the store always
/// opens a fresh, currently-valid edge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewRelationship {
    /// The subject of the relationship.
    pub from: EntityId,
    /// The object of the relationship.
    pub to: EntityId,
    /// The relationship's label — see [`Relationship::predicate`].
    pub predicate: String,
    /// Confidence in `[0, 1]`.
    pub confidence: f32,
}

/// Trim and lowercase a knowledge-graph label (`Entity::kind`,
/// `Relationship::predicate`) so `"Person"` and `"person"` don't silently
/// become two different graph values.
///
/// This is the "cheap guard" for labels people and agents assert. The closed
/// vocabulary is [`EntityKind`] and [`Predicate`], which only extracted facts
/// are held to (see the `kg_normalize` lesson referenced on
/// [`Relationship::predicate`]). Returns `None` for an empty (post-trim)
/// label, which callers should reject rather than silently store.
#[must_use]
pub fn normalize_label(raw: &str) -> Option<String> {
    let normalized = raw.trim().to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

/// [`normalize_label`], rejecting an empty label with a diagnostic that names
/// the field instead of silently storing `""`. The check lives here, in the
/// domain, because "a label must not be empty" is a rule about labels, not
/// about how the store persists them.
///
/// # Errors
///
/// Returns [`crate::Error::EmptyLabel`] if `raw` is empty after trimming.
pub fn require_label(field: &'static str, raw: &str) -> crate::error::Result<String> {
    normalize_label(raw).ok_or_else(|| crate::Error::EmptyLabel {
        field: field.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{EntityKind, Predicate, normalize_label, require_label};

    #[test]
    fn an_empty_label_is_reported_against_the_field_that_held_it() {
        let error = require_label("predicate", "  ").unwrap_err();
        assert!(matches!(
            error,
            crate::Error::EmptyLabel { field } if field == "predicate"
        ));
    }

    #[test]
    fn differently_cased_labels_normalize_to_the_same_value() {
        assert_eq!(normalize_label("Person"), normalize_label("person"));
        assert_eq!(normalize_label("PERSON").as_deref(), Some("person"));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(normalize_label("  project  ").as_deref(), Some("project"));
    }

    #[test]
    fn an_empty_or_whitespace_only_label_is_rejected() {
        assert_eq!(normalize_label(""), None);
        assert_eq!(normalize_label("   "), None);
    }

    #[test]
    fn an_entity_kind_outside_the_vocabulary_becomes_other() {
        assert_eq!(EntityKind::parse(" Person "), EntityKind::Person);
        assert_eq!(EntityKind::parse("employee"), EntityKind::Other);
        assert_eq!(EntityKind::parse(""), EntityKind::Other);
    }

    #[test]
    fn a_predicate_outside_the_vocabulary_falls_back_to_related_to() {
        assert_eq!(Predicate::parse("works on"), Predicate::WorksOn);
        assert_eq!(Predicate::parse("Depends-On"), Predicate::DependsOn);
        assert_eq!(Predicate::parse("is friends with"), Predicate::RelatedTo);
    }

    #[test]
    fn every_vocabulary_label_parses_back_to_itself() {
        for kind in EntityKind::ALL {
            assert_eq!(EntityKind::parse(kind.as_str()), kind);
        }
        for predicate in Predicate::ALL {
            assert_eq!(Predicate::parse(predicate.as_str()), predicate);
        }
    }
}
