//! Temporal knowledge-graph types.
//!
//! Schema-ready and, as of `store::entities`, schema-wired: the SurrealDB
//! tables and graph edge these map to are defined in
//! `database/schema/palace.surql`, and `store::entities` reads and writes
//! them. The only populator today is a checkpoint item's optional `fact`
//! mutation (`checkpoint::apply_fact_mutation`); mining does not extract
//! entities yet (that's #40's deliberate future work, not an oversight).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{EntityId, RelationshipId};

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
    /// The relationship's label (kept in a closed vocabulary once mining
    /// wires this up — see MemPalace's `kg_normalize` lesson).
    pub predicate: String,
    /// Confidence in `[0, 1]`.
    pub confidence: f32,
    /// When this fact became true.
    pub valid_from: DateTime<Utc>,
    /// When this fact stopped being true, if it has been superseded.
    pub valid_to: Option<DateTime<Utc>>,
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
/// This is the "cheap guard" #11 asks for, not a closed vocabulary — a real
/// fixed set of kinds/predicates is mining's job once #40 wires up
/// extraction (see the `kg_normalize` lesson referenced on
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
    use super::{normalize_label, require_label};

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
}
