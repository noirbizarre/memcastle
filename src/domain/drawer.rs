//! The atomic stored memory unit.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{DrawerId, JobId, RoomId};

/// Where a drawer's content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Mined from a file on disk.
    File,
    /// Written directly through an MCP tool call or the HTTP API.
    Manual,
    /// Mined from a conversation transcript (an agent's session history).
    Transcript,
    /// Captured as a note: a thought written down by a person (`memcastle note`), kept verbatim.
    Note,
    /// Reserved for future ingest modes (chat messages, issues, pages, ...).
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
    /// **Who wrote it**: the agent identity behind the write — a checkpoint
    /// item's or a diary entry's `agent_identity` — or `None` when no agent
    /// is involved (mining a directory). One meaning for every writer; the
    /// channel a write arrived through is [`Provenance::requested_by`]'s job,
    /// not this field's.
    pub agent: Option<String>,
    /// Which mining source, document and chunk this drawer was cut from.
    /// `None` for anything not mined through a source (manual writes, checkpoint items) and for drawers mined
    /// before the unified source model existed; absent from the stored object in both cases, so
    /// rows and wire shapes written earlier are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// Where inside a mining source a drawer came from: enough to find the source document again, and to tell
/// which version of it the drawer was cut from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The mined place the document was acquired from.
    pub source_id: super::SourceId,
    /// The adapter that acquired it (`directory`, `pi`, ...).
    pub source: String,
    /// The document's identity within the source (a relative path, a session file, a message id).
    pub document: String,
    /// Zero-based position of this chunk within the document.
    pub chunk: u32,
    /// The document revision this chunk was cut from, so a later revision can be told apart.
    pub revision: String,
    /// Immutable document metadata at the time this chunk was filed; old revisions must not inherit new criteria.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Source event time, if known; separate from when this drawer became valid in the palace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
}

impl Source {
    /// A source with no mining origin: the shape every writer that is not a mining source uses.
    #[must_use]
    pub fn new(kind: SourceKind, uri: Option<String>, agent: Option<String>) -> Self {
        Self {
            kind,
            uri,
            agent,
            origin: None,
        }
    }
}

/// Bookkeeping for *why* a drawer exists, distinct from *what* it contains.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// **Through which channel the write was asked for**: `"cli"`, `"http"`
    /// or `"mcp"` (whatever the submitting interface says; the HTTP API
    /// accepts a caller-chosen string). One meaning for every writer — diary
    /// entries, mining and checkpoint all record the channel here and the
    /// agent identity in [`Source::agent`], so "who asked" can be queried
    /// uniformly. `"unknown"` marks a drawer written before this rule, whose
    /// channel was never recorded (see the `diary-provenance` migration).
    pub requested_by: String,
    /// The job that produced this drawer, if any (manual writes have none).
    pub job_id: Option<JobId>,
}

/// One verbatim chunk of content, filed under a room.
///
/// `content` is never mutated after creation — a re-mine creates a new
/// drawer rather than overwriting one (and an agent write identical to a
/// drawer the room already holds is not stored twice, docs/adr/025), so
/// provenance and history stay honest. A correction is a *supersession*:
/// the old drawer gains a `valid_to` and a replacement opens from that instant.
/// `embedding` is derived data filled in after the fact by the embedding sweep
/// (or by a caller), never part of what the drawer says.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drawer {
    /// Unique identifier.
    pub id: DrawerId,
    /// The room this drawer is filed under.
    pub room: RoomId,
    /// An optional name, unique within the room, so the drawer can be
    /// addressed as `wing/room/name` instead of by UUID. Most drawers have
    /// none. Absent in drawers written before names existed.
    #[serde(default)]
    pub name: Option<String>,
    /// The verbatim, original content.
    pub content: String,
    /// SHA-256 of `content`, for cheap exact-duplicate detection. A *normalised* fingerprint (case, punctuation and
    /// whitespace ignored) is derived from `content` when the drawer is stored and is not part of this type.
    pub content_hash: String,
    /// Where this content came from.
    pub source: Source,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// A dense embedding vector (768 numbers), once the embedding sweep or a caller fills it. Derived data; search results omit it.
    pub embedding: Option<Vec<f32>>,
    /// Why this drawer exists.
    pub provenance: Provenance,
    /// The start of this drawer's validity window.
    pub valid_from: DateTime<Utc>,
    /// The end of this drawer's validity window, if it has been superseded.
    pub valid_to: Option<DateTime<Utc>>,
    /// The drawer this one replaced, when it is a supersession's replacement.
    ///
    /// Set by the store in the transaction that closes the old drawer, never by
    /// a caller, so the two links of a pair cannot disagree. Absent on a first
    /// version and on drawers written before lineage was recorded that the
    /// `supersession-lineage` migration piece could not pair unambiguously.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<DrawerId>,
    /// The drawer that replaced this one. Absent while it is open and when it
    /// was closed without a replacement (an invalidation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<DrawerId>,
    /// Creation timestamp: when MemCastle *recorded* this drawer, which can be
    /// long after the period `valid_from`/`valid_to` describe.
    pub created_at: DateTime<Utc>,
    /// Last-updated timestamp (metadata only — `content` itself is immutable).
    pub updated_at: DateTime<Utc>,
}

/// The successive versions of one piece of knowledge, oldest first.
///
/// Reconstructed from the supersession links (`Drawer::supersedes` and
/// `Drawer::superseded_by`), so an agent asking "how did this evolve?" does not
/// have to rebuild the chain from search hits. Each version is the drawer
/// verbatim, so identity, validity period, provenance and content are all
/// preserved; embeddings are left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawerHistory {
    /// The drawer the history was asked about; one of `versions`.
    pub drawer: DrawerId,
    /// Every version of the knowledge, oldest first. The last is the current
    /// one unless it has a `valid_to` (the knowledge was later invalidated).
    pub versions: Vec<Drawer>,
}

impl Drawer {
    /// A freshly written drawer: its `content_hash` computed from `content`,
    /// and every timestamp set to now, with no embedding and no validity end.
    ///
    /// The single place a drawer is assembled. Diary writes, mining and
    /// checkpoints all used to build the struct by hand, each with its own
    /// hashing and clock reads, so a change to what "a new drawer" means had
    /// to be made in three places and could be missed in one.
    #[must_use]
    pub fn new(
        id: DrawerId,
        room: RoomId,
        content: String,
        source: Source,
        tags: Vec<String>,
        provenance: Provenance,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            room,
            name: None,
            content_hash: content_hash(&content),
            content,
            source,
            tags,
            embedding: None,
            provenance,
            valid_from: now,
            valid_to: None,
            supersedes: None,
            superseded_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// This drawer, named `name` (or left unnamed with `None`). The caller
    /// validates the name with [`super::validate_name`]; a builder rather than
    /// a seventh constructor argument because nearly every writer has none.
    #[must_use]
    pub fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }
}

/// The SHA-256 of `content`, as lowercase hex — a drawer's `content_hash`.
#[must_use]
pub fn content_hash(content: &str) -> String {
    sha256_hex(content.as_bytes())
}

/// SHA-256 of `bytes` as lowercase hex. Encoded by hand: `finalize()` returns
/// a fixed-size byte array that does not implement `LowerHex`, and a
/// dependency for one `format!` is not worth it.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_drawer_hashes_its_content_and_starts_valid_with_matching_timestamps() {
        let drawer = Drawer::new(
            DrawerId::new(),
            RoomId::new(),
            "hello".to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        );

        assert_eq!(
            drawer.content_hash, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            "SHA-256 of `hello`"
        );
        assert_eq!(drawer.created_at, drawer.valid_from);
        assert_eq!(drawer.created_at, drawer.updated_at);
        assert!(drawer.valid_to.is_none() && drawer.embedding.is_none());
    }
}
