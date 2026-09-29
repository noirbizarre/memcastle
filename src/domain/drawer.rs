//! The atomic stored memory unit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{DrawerId, JobId, RoomId};

/// Where a drawer's content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Mined from a file on disk.
    File,
    /// Written directly through an MCP tool call or the HTTP API.
    Manual,
    /// Reserved for future ingest modes (conversation transcripts, etc).
    #[serde(other)]
    Other,
}

/// Provenance of a drawer's content: what produced it, and from where.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    /// What kind of ingest produced this drawer.
    pub kind: SourceKind,
    /// A file path, URI, or other locator, when applicable.
    pub uri: Option<String>,
    /// The agent that made the write, when known — e.g. a checkpoint item's
    /// or diary entry's `agent_identity`.
    pub agent: Option<String>,
}

/// Bookkeeping for *why* a drawer exists, distinct from *what* it contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// Free-text identifier of who/what asked for this write.
    pub requested_by: String,
    /// The job that produced this drawer, if any (manual writes have none).
    pub job_id: Option<JobId>,
}

/// One verbatim chunk of content, filed under a room.
///
/// `content` is never mutated after creation — a re-mine creates a new
/// drawer (or, once dedup lands, is rejected) rather than overwriting one,
/// so provenance and history stay honest. `embedding` and the temporal
/// `valid_from`/`valid_to` pair are populated by later phases (semantic
/// search, supersession) and are `None`/equal-to-`created_at` for every
/// drawer this bootstrap writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawer {
    /// Unique identifier.
    pub id: DrawerId,
    /// The room this drawer is filed under.
    pub room: RoomId,
    /// The verbatim, original content.
    pub content: String,
    /// SHA-256 of `content`, for cheap exact-duplicate detection.
    pub content_hash: String,
    /// Where this content came from.
    pub source: Source,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// A dense embedding vector, once semantic search populates it.
    pub embedding: Option<Vec<f32>>,
    /// Why this drawer exists.
    pub provenance: Provenance,
    /// The start of this drawer's validity window.
    pub valid_from: DateTime<Utc>,
    /// The end of this drawer's validity window, if it has been superseded.
    pub valid_to: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last-updated timestamp (metadata only — `content` itself is immutable).
    pub updated_at: DateTime<Utc>,
}
