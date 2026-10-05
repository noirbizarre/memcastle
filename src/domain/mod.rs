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
mod fingerprint;
mod ids;
mod job;
mod memory_mode;
mod palace;
mod path;
mod resolution;
mod search;
mod secret;
mod source;
mod source_index;
mod source_package;

pub use checkpoint::{CheckpointDestination, CheckpointItem, CheckpointPayload, FactMutation};
pub use drawer::{
    Drawer, DrawerHistory, Origin, Provenance, Source, SourceKind, content_hash, sha256_hex,
};
pub use entity::{
    Entity, EntityKind, FactProvenance, Mention, NewRelationship, Observation, Predicate,
    Relationship, normalize_label, require_label,
};
pub use extraction::{ExtractedEntity, ExtractedGraph, ExtractedRelation, Limits, MAX_NAME_CHARS};
pub use fingerprint::{
    DuplicateKind, DuplicateSignals, EDIT_DISTANCE_LIMIT, classify, fingerprint, normalize_text,
    similarity, similarity_of_normalized,
};
pub use ids::{DrawerId, EntityId, JobId, PalaceId, RelationshipId, RoomId, SourceId, WingId};
pub(crate) use job::default_dry_run;
pub use job::{
    InvalidPriority, Job, JobEvent, JobKind, JobProgress, JobStatus, MiningSource, Priority,
    TransitionError,
};
pub use memory_mode::MemoryMode;
pub use resolution::{
    EntityCandidate, MIN_TYPO_KEY_CHARS, PossibleMatch, Resolution, ResolutionRule, entity_key,
    resolve,
};
pub use search::{
    EMBEDDING_DIMENSION, RankingMode, SearchFilter, SearchHit, SearchQuery, Signals, Temporal,
};
pub use secret::Secret;
pub use source::{
    Candidate, CanonicalDocument, ChunkRef, CredentialRef, Cursor, RawDocument, Segment,
    SourceCapabilities, SourceDocumentRecord, SourceRecord, SourceRef,
};
pub use source_index::{INDEX_FORMAT, IndexSignature, IndexedSource, IndexedVersion, SourceIndex};
pub use source_package::{
    BuildSection, CONTRACT_VERSION, Compatibility, FilesystemPermissions, MANIFEST_FORMAT,
    MAX_SOURCE_NAME_LEN, ManifestSource, PackageTransitionError, Permissions, ResourceLimits,
    SourceManifest, SourceOrigin, SourcePackageEvent, SourcePackageRecord, SourcePackageState,
    SourceState, TestSection, contract_compatibility, contract_version, is_valid_source_name,
    version_compatibility,
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
