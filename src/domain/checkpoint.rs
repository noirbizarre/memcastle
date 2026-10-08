//! The checkpoint payload: an already-classified batch of memory writes,
//! ready for durable, resumable persistence.
//!
//! **Naming collision, read this first:** this module's [`CheckpointPayload`]
//! (wrapped in [`super::JobKind::Checkpoint`]) and [`super::Job::checkpoint`]
//! (the scheduler's generic, handler-defined resume state) share the word
//! "checkpoint" but are unrelated concepts. A running checkpoint *job*
//! still uses `Job::checkpoint` to record its own resume position (see
//! `crate::checkpoint::run`), exactly like every other job kind — the two
//! never nest inside one another.
//!
//! **What this does *not* do:** decide what's worth remembering. MemCastle
//! has no LLM client and does not classify conversation content into these
//! buckets — the calling integration does that with its own model, using
//! its own view of the conversation. This type only describes the result of
//! that classification, for MemCastle to persist correctly and resumably.

use serde::{Deserialize, Serialize};

use super::{EntityId, RelationshipId, Source};

/// Which bucket a checkpoint item's content belongs to, as classified by
/// the calling integration — not decided by MemCastle (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointDestination {
    /// A durable preference the agent should keep honoring across sessions.
    Preference,
    /// A decision or fact scoped to the current project.
    Project,
    /// A diary-style entry: a record of what happened, not a standing fact.
    Diary,
    /// Anything that doesn't fit a more specific bucket.
    General,
}

impl CheckpointDestination {
    /// The wing an item of this destination files under when it doesn't
    /// supply its own (`CheckpointItem::wing`) — one fixed bucket per
    /// destination, the simplest routing that satisfies "resolve
    /// destination -> wing/room" for V1. Per-project wing routing is what
    /// `CheckpointItem::wing` is for; this is only the fallback.
    #[must_use]
    pub fn default_wing(self) -> &'static str {
        match self {
            Self::Preference => "preferences",
            Self::Project => "projects",
            Self::Diary => "diary",
            Self::General => "general",
        }
    }

    /// The room an item of this destination files under, within whichever
    /// wing it resolves to. Unlike the wing, this is never overridden —
    /// this issue's payload shape has no need for finer room-level control
    /// yet, and a fixed room name per destination keeps every bucket's
    /// entries in one predictable place regardless of which wing they
    /// landed in.
    #[must_use]
    pub fn room_name(self) -> &'static str {
        match self {
            Self::Diary => "diary",
            Self::Preference | Self::Project | Self::General => "entries",
        }
    }
}

/// A knowledge-graph edge mutation attached to a checkpoint item, applied
/// in addition to (not instead of) writing the item's drawer — see
/// `crate::checkpoint::run`'s doc comment for why both happen.
///
/// `subject`/`object`/`from`/`to`/`relationship_id` are [`EntityId`]s and
/// [`RelationshipId`]s the caller has already resolved (created or looked
/// up via its own means) — resolving human-readable names to entities is
/// the caller's responsibility, not this job's; see `store::entities` for
/// the underlying operations these variants dispatch to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FactMutation {
    /// Open a new, currently-valid edge between two entities. Maps onto
    /// [`crate::store::SurrealStore::create_relationship`].
    Add {
        /// The subject of the relationship.
        subject: EntityId,
        /// The relationship's label.
        predicate: String,
        /// The object of the relationship.
        object: EntityId,
        /// Confidence in `[0, 1]`.
        confidence: f32,
    },
    /// Close an existing edge and open a replacement — the fact changed,
    /// it wasn't simply retracted. Field shape mirrors
    /// [`super::NewRelationship`] exactly (plus the id of the edge being
    /// replaced), since it maps directly onto
    /// [`crate::store::SurrealStore::supersede_relationship`].
    Supersede {
        /// The edge being closed.
        relationship_id: RelationshipId,
        /// The subject of the replacement relationship.
        from: EntityId,
        /// The object of the replacement relationship.
        to: EntityId,
        /// The replacement relationship's label.
        predicate: String,
        /// Confidence in `[0, 1]`.
        confidence: f32,
        /// Why this explicit replacement takes precedence over an inferred conflict.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Close an existing edge without opening a replacement — the fact is
    /// retracted. Maps onto
    /// [`crate::store::SurrealStore::invalidate_relationship`].
    Invalidate {
        /// The edge being closed.
        relationship_id: RelationshipId,
        /// Why the assertion was retracted.
        #[serde(default)]
        reason: Option<String>,
    },
    /// Add an auditable explicit comparison without closing either assertion.
    Link {
        /// Assertion making the comparison.
        relationship_id: RelationshipId,
        /// Assertion it is compared with.
        other_id: RelationshipId,
        /// Either confirms, contradicts, or refines (never supersedes: use the atomic operation).
        kind: super::FactLinkKind,
        /// The caller's explanation.
        reason: String,
    },
}

/// One unit of checkpoint work: a drawer to write, optionally alongside a
/// knowledge-graph mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointItem {
    /// Which bucket this item belongs to.
    pub destination: CheckpointDestination,
    /// Overrides `destination`'s default wing (see
    /// [`CheckpointDestination::default_wing`]) when set — e.g. filing a
    /// `Project` item under a specific project's wing rather than one
    /// shared `"projects"` bucket. `None` (including when the field is
    /// omitted from JSON entirely) keeps the fixed-bucket routing.
    #[serde(default)]
    pub wing: Option<String>,
    /// An optional name for the resulting drawer, unique within its room, so
    /// it can be addressed as `wing/room/name`. A name already held by a
    /// drawer with other content fails the item (the caller asked for that
    /// name); replaying the same item is a no-op. See
    /// [`super::validate_name`] for what a name may be.
    #[serde(default)]
    pub name: Option<String>,
    /// The drawer's content.
    pub content: String,
    /// Free-form labels, stored on the resulting drawer verbatim.
    pub tags: Vec<String>,
    /// Reused as-is for the resulting drawer's `Source` — in particular,
    /// `source.agent` is how a `Diary`-destination item carries the
    /// identity it should be scoped to (see `domain::drawer::Source`'s doc
    /// comment and issue #13's design, which this anticipates without
    /// depending on it).
    pub source: Source,
    /// A knowledge-graph mutation to apply alongside the drawer write, if
    /// any.
    #[serde(default)]
    pub fact: Option<FactMutation>,
}

/// A batch of checkpoint items to persist as one durable, resumable job —
/// see the module doc for what this is (and is not) responsible for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointPayload {
    /// The items to process, in order. Resumption (`crate::checkpoint::run`)
    /// tracks a plain index into this list, so this order must stay stable
    /// for the lifetime of one job.
    pub items: Vec<CheckpointItem>,
}
