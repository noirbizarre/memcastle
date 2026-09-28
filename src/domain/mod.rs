//! The palace domain model.
//!
//! Pure types only: no `surrealdb`, no `tokio`, no I/O. `store` maps these
//! to/from SurrealDB records; `app` is the only layer allowed to construct
//! or mutate them on purpose. Keeping this module free of infrastructure
//! dependencies is what lets `store`'s backend change without a domain
//! rewrite.

mod checkpoint;
mod drawer;
mod entity;
mod ids;
mod job;
mod memory_mode;
mod palace;

pub use checkpoint::{CheckpointDestination, CheckpointItem, CheckpointPayload, FactMutation};
pub use drawer::{Drawer, Provenance, Source, SourceKind};
pub use entity::{Entity, NewRelationship, Relationship, normalize_label};
pub use ids::{DrawerId, EntityId, JobId, PalaceId, RelationshipId, RoomId, WingId};
pub use job::{
    InvalidPriority, Job, JobEvent, JobKind, JobProgress, JobStatus, MiningSource, Priority,
    TransitionError,
};
pub use memory_mode::MemoryMode;
pub use palace::{Palace, Room, Wing};
