//! Temporal knowledge-graph types.
//!
//! Schema-ready, not schema-wired: the SurrealDB tables and graph edge these
//! map to exist from the first migration (see `store::schema`), but no
//! mining or MCP code populates them yet. Defining the shape now is what
//! lets a later extraction pass land without a storage migration.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::EntityId;

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
