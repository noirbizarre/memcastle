//! The palace domain model.
//!
//! Pure types only: no `surrealdb`, no `tokio`, no I/O. `store` maps these
//! to/from SurrealDB records; `app` is the only layer that *decides* to
//! create palace content, though `store` rebuilds these types from rows and
//! the job handlers update a running `Job`'s progress and result. Keeping this module free of infrastructure
//! dependencies is what lets `store`'s backend change without a domain
//! rewrite.

pub mod auth;
mod checkpoint;
mod drawer;
mod entity;
mod extraction;
mod ids;
mod job;
mod memory_mode;
mod palace;
mod path;
mod search;
mod secret;
mod source;

pub use checkpoint::{CheckpointDestination, CheckpointItem, CheckpointPayload, FactMutation};
pub use drawer::{Drawer, Origin, Provenance, Source, SourceKind, content_hash, sha256_hex};
pub use entity::{
    Entity, EntityKind, FactProvenance, Mention, NewRelationship, Predicate, Relationship,
    normalize_label, require_label,
};
pub use extraction::{ExtractedEntity, ExtractedGraph, ExtractedRelation, Limits, MAX_NAME_CHARS};
pub use ids::{DrawerId, EntityId, JobId, PalaceId, RelationshipId, RoomId, SourceId, WingId};
pub(crate) use job::default_dry_run;
pub use job::{
    InvalidPriority, Job, JobEvent, JobKind, JobProgress, JobStatus, MiningSource, Priority,
    TransitionError,
};
pub use memory_mode::MemoryMode;
pub use search::{
    EMBEDDING_DIMENSION, RankingMode, SearchFilter, SearchHit, SearchQuery, Signals, Temporal,
};
pub use secret::Secret;
pub use source::{
    Candidate, CanonicalDocument, ChunkRef, CredentialRef, Cursor, RawDocument, Segment,
    SourceCapabilities, SourceDocumentRecord, SourceRecord, SourceRef,
};

/// The channels a write can come through, recorded as `Job::requested_by` and
/// `provenance.requested_by`. Named once so a spelling drift between the
/// client that sends one and the daemon that records it cannot split "cli"
/// from "CLI" in the palace's provenance.
pub mod channel {
    /// The `memcastle` command line.
    pub const CLI: &str = "cli";
    /// A direct REST caller that did not name itself.
    pub const HTTP: &str = "http";
    /// An MCP tool call.
    pub const MCP: &str = "mcp";
}
pub use palace::{
    DEFAULT_PALACE_NAME, Deleted, DrawerSummary, Palace, Room, RoomSummary, Wing, WingSummary,
};
pub use path::{NameKind, PalacePath, validate_name};
