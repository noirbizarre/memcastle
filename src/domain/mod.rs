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
mod credential;
mod drawer;
mod entity;
mod extraction;
mod fingerprint;
mod ids;
mod job;
mod memory_mode;
mod miner;
mod palace;
mod path;
mod preferences;
mod resolution;
mod search;
mod secret;
mod source;
mod source_index;
mod source_package;
mod trigger;

pub use checkpoint::{CheckpointDestination, CheckpointItem, CheckpointPayload, FactMutation};
pub use credential::{AccessTokens, SourceAuth};
pub use drawer::{
    Drawer, DrawerHistory, Origin, Provenance, Source, SourceKind, content_hash, sha256_hex,
};
pub use entity::{
    Entity, EntityKind, FactLifecycle, FactLink, FactLinkKind, FactLinkOrigin, FactProvenance,
    FactState, Mention, NewRelationship, Observation, Predicate, Relationship, infer_fact_link,
    normalize_label, require_label,
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
pub use miner::{
    MAX_MINER_NAME_LEN, MinerDefinition, is_valid_miner_name, option_broadening, validate_miners,
};
pub use resolution::{
    EntityCandidate, MIN_TYPO_KEY_CHARS, PossibleMatch, Resolution, ResolutionRule, entity_key,
    resolve,
};
pub use search::{
    EMBEDDING_DIMENSION, RankingMode, SearchFilter, SearchHit, SearchQuery, Signals, Temporal,
};
pub use secret::Secret;
pub use source::{
    Candidate, CanonicalDocument, ChunkRef, CredentialRef, Cursor, OptionBreadth, OptionKind,
    OptionSpec, Options, RawDocument, Segment, SourceCapabilities, SourceDocumentRecord,
    SourceRecord, SourceRef, is_option_key, parse_since, unknown_option,
};
pub use source_index::{INDEX_FORMAT, IndexSignature, IndexedSource, IndexedVersion, SourceIndex};
pub use source_package::{
    BuildSection, CONTRACT_VERSION, Compatibility, FilesystemPermissions, MANIFEST_FORMAT,
    MAX_SOURCE_NAME_LEN, ManifestOption, ManifestSource, OAuthRequirement, PackageTransitionError,
    Permissions, ResourceLimits, SourceManifest, SourceOrigin, SourcePackageEvent,
    SourcePackageRecord, SourcePackageState, SourceState, TestSection, contract_compatibility,
    contract_version, is_secure_endpoint, is_valid_source_name, version_compatibility,
};
pub use trigger::{
    FireOutcome, MAX_TRIGGER_NAME_LEN, ManifestTrigger, SignatureEncoding, TriggerDefinition,
    TriggerDelivery, TriggerMechanism, TriggerPlan, TriggerSpec, TriggerState, TriggerStatus,
    WebhookAuth, WebhookPlan, is_valid_trigger_name, next_due, parse_duration, supported_triggers,
    validate_triggers,
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
pub use preferences::{
    ConnectorPreference, PreferenceCriterion, PreferenceLevel, PreferenceMatch, SourcePreferences,
};
