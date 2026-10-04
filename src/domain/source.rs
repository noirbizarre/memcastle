//! The unified mining source model: what a source *is*, what an adapter hands over, and what MemCastle remembers
//! about both.
//!
//! Pure types, no I/O. The behaviour (adapters, the pipeline, the chunker) lives in `crate::mining`; the rows live in
//! `crate::store`. See `docs/adr/023-unified-source-model-for-mining.md` for why mining is split in three stages:
//!
//! 1. **acquire**: an adapter discovers what is new since a [`Cursor`] and reads it into [`RawDocument`]s, with no
//!    agent and no model involved;
//! 2. **normalize**: the adapter turns a raw document into a [`CanonicalDocument`] (pure, no I/O);
//! 3. **ingest**: the core cuts a canonical document into chunks and files them as drawers, idempotently.
//!
//! Optional semantic processing (entity extraction, summaries) is a separate stage that reads what ingest wrote.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{DrawerId, JobId, SourceId, SourceKind, sha256_hex};

/// Where an adapter stopped, in the adapter's own terms.
///
/// Opaque to the core: it stores it on the source and hands it back, and never looks inside.
/// `null` means "from the beginning". A cursor is an optimisation, never a correctness mechanism: ingest is
/// idempotent, so reading something twice is always safe, and a lost or distrusted cursor costs a re-read, not
/// duplicates.
pub type Cursor = serde_json::Value;

/// A reference to a credential, never the credential itself.
///
/// What an adapter needs to authenticate to an external system is looked up through this at acquisition time and
/// held as a `Secret`; the reference is what is stored on a source, so no secret is ever written to the palace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CredentialRef {
    /// Read from the named environment variable of the daemon.
    Env {
        /// The variable's name.
        name: String,
    },
    /// Read from the first line of a file.
    File {
        /// The file's path.
        path: String,
    },
}

/// The identity of a source: which system, which account on it, and which part of it.
///
/// Two jobs mining "the same place" must agree on the source, because the source is what carries the cursor and what
/// documents belong to; [`SourceRef::id`] is therefore derived from these three fields and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// The adapter's name (`directory`, `pi`, ...).
    pub provider: String,
    /// The account on the provider, when it has accounts (a Slack workspace, a GitHub login). `None` for local
    /// sources.
    pub account: Option<String>,
    /// The part of the provider to read: a directory path, a sessions root, a channel, a repository.
    pub locator: String,
}

impl SourceRef {
    /// The stable identifier of this source.
    #[must_use]
    pub fn id(&self) -> SourceId {
        // NUL cannot appear in any of the three fields, so `("a", "b")` and `("ab", "")` never collide.
        let identity = format!(
            "{}\0{}\0{}",
            self.provider,
            self.account.as_deref().unwrap_or(""),
            self.locator
        );
        SourceId::derive(uuid::Uuid::nil(), &identity)
    }
}

/// What an adapter can do, so the core and the user know what to expect of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
// A manifest may state only what is true of the source; the rest is the conservative `false`.
#[serde(default)]
pub struct SourceCapabilities {
    /// Whether the cursor narrows discovery to what changed (otherwise every run re-reads everything and relies on
    /// revisions to skip what is unchanged).
    pub incremental: bool,
    /// Whether the raw document is kept in the palace next to the drawers cut from it. Off where the origin is
    /// itself durable and local (a file on disk), on where the origin may be rotated away or is expensive to
    /// re-acquire.
    pub retains_raw: bool,
    /// Whether the adapter needs a [`CredentialRef`] to acquire anything.
    pub needs_credentials: bool,
}

/// A document the adapter found, before it is read.
///
/// Discovery is cheap and ordered; reading is not, so the core reads candidates one at a time and can pause between
/// them without having held every body in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The document's identity within its source; stable across runs and across revisions of the document.
    pub external_id: String,
    /// The cursor to store once this candidate (and every one before it) is done.
    pub cursor_after: Cursor,
    /// Whatever the adapter needs to read it back (a path, an API id); opaque to the core.
    pub handle: String,
}

/// One document exactly as the source provided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDocument {
    /// The document's identity within its source.
    pub external_id: String,
    /// A token that changes exactly when the content does (a content hash, an etag, an `updated_at`): an unchanged
    /// revision lets the core skip the document without normalizing it.
    pub revision: String,
    /// The document as acquired. Text, since every source MemCastle mines is.
    pub body: String,
    /// Provider-specific facts that are not content (a path, a session id, a channel).
    pub metadata: serde_json::Value,
    /// When the document was created or last changed at the source, if the source says.
    pub occurred_at: Option<DateTime<Utc>>,
}

impl RawDocument {
    /// The revision of a body that carries no better token of its own: its SHA-256.
    #[must_use]
    pub fn revision_of(body: &str) -> String {
        sha256_hex(body.as_bytes())
    }
}

/// A unit of a canonical document the chunker never splits across chunks unless it alone exceeds the chunk size: a
/// message in a conversation, a file's whole text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The text, already in its final form (including any trailing separator): concatenating every segment is the
    /// document's content.
    pub text: String,
}

/// A raw document in MemCastle's own terms: what the core chunks and files.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalDocument {
    /// A human title (a session's first prompt, a file name), when there is one.
    pub title: Option<String>,
    /// The room the drawers belong in; `None` files them under the adapter's default room.
    pub room: Option<String>,
    /// The name chunk 0 should carry, so the document can be addressed as `wing/room/name`.
    pub name: Option<String>,
    /// What the drawers' `source.kind` says.
    pub kind: SourceKind,
    /// The location a human would open to see the original (a path, a URL), recorded as `source.uri`.
    pub uri: Option<String>,
    /// Tags for every drawer cut from the document.
    pub tags: Vec<String>,
    /// The content, in order.
    pub segments: Vec<Segment>,
}

/// One chunk of a document and the drawer it became.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    /// Zero-based position within the document.
    pub index: u32,
    /// SHA-256 of the chunk's text: what makes "unchanged" decidable without reading the drawer.
    pub hash: String,
    /// The drawer holding the chunk.
    pub drawer: DrawerId,
}

/// What MemCastle remembers about a source between jobs: its identity, where the last run stopped, and how to
/// authenticate (by reference only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRecord {
    /// Derived from [`SourceRecord::reference`].
    pub id: SourceId,
    /// The adapter's name.
    pub provider: String,
    /// The account on the provider, if any.
    pub account: Option<String>,
    /// The part of the provider that is read.
    pub locator: String,
    /// How to authenticate; a reference, never a secret.
    pub credential: Option<CredentialRef>,
    /// Where the last run stopped.
    pub cursor: Cursor,
    /// The job that last advanced the cursor.
    pub last_job: Option<JobId>,
    /// When the cursor last advanced.
    pub last_run_at: Option<DateTime<Utc>>,
    /// First time a job mined this source.
    pub created_at: DateTime<Utc>,
    /// Last change to this record.
    pub updated_at: DateTime<Utc>,
}

impl SourceRecord {
    /// A source seen for the first time: no cursor, never run.
    #[must_use]
    pub fn new(reference: &SourceRef, credential: Option<CredentialRef>) -> Self {
        let now = Utc::now();
        Self {
            id: reference.id(),
            provider: reference.provider.clone(),
            account: reference.account.clone(),
            locator: reference.locator.clone(),
            credential,
            cursor: Cursor::Null,
            last_job: None,
            last_run_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// The identity this record stands for.
    #[must_use]
    pub fn reference(&self) -> SourceRef {
        SourceRef {
            provider: self.provider.clone(),
            account: self.account.clone(),
            locator: self.locator.clone(),
        }
    }
}

/// What MemCastle remembers about one document of a source: the revision it last ingested and the drawers that came
/// of it. The memory that makes a re-mine a no-op and an edit a minimal supersession.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceDocumentRecord {
    /// The source the document belongs to.
    pub source: SourceId,
    /// The document's identity within its source.
    pub external_id: String,
    /// The revision last ingested.
    pub revision: String,
    /// The raw document, kept only for sources whose [`SourceCapabilities::retains_raw`] is set.
    pub raw: Option<String>,
    /// SHA-256 of the raw document.
    pub raw_hash: String,
    /// The document's title, if it has one.
    pub title: Option<String>,
    /// Provider-specific facts about the document.
    pub metadata: serde_json::Value,
    /// When the document was created or last changed at the source.
    pub occurred_at: Option<DateTime<Utc>>,
    /// The chunks of the last ingested revision, in order.
    pub chunks: Vec<ChunkRef>,
    /// When the document was last ingested.
    pub acquired_at: DateTime<Utc>,
    /// The job that last ingested it.
    pub job: Option<JobId>,
}

impl SourceDocumentRecord {
    /// The record's key: the same for the same document of the same source on every run.
    #[must_use]
    pub fn key(source: SourceId, external_id: &str) -> String {
        sha256_hex(format!("{source}\0{external_id}").as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(account: Option<&str>, locator: &str) -> SourceRef {
        SourceRef {
            provider: "demo".into(),
            account: account.map(str::to_string),
            locator: locator.into(),
        }
    }

    #[test]
    fn a_source_has_the_same_id_every_time_it_is_named() {
        assert_eq!(
            reference(None, "/a").id(),
            reference(None, "/a").id(),
            "two jobs mining the same place would otherwise each get a cursor of their own"
        );
    }

    #[test]
    fn sources_differing_in_provider_account_or_locator_have_different_ids() {
        let base = reference(None, "/a").id();
        assert_ne!(base, reference(None, "/b").id());
        assert_ne!(base, reference(Some("me"), "/a").id());
        let mut other = reference(None, "/a");
        other.provider = "other".into();
        assert_ne!(base, other.id());
    }

    #[test]
    fn identity_fields_cannot_be_shifted_into_each_other() {
        // ("demo", "ab", "") vs ("demo", "a", "b"): a bare concatenation would make these the same source.
        assert_ne!(
            reference(Some("ab"), "").id(),
            reference(Some("a"), "b").id()
        );
    }

    #[test]
    fn a_credential_reference_serialises_where_to_look_and_never_a_value() {
        let reference = CredentialRef::Env {
            name: "SLACK_TOKEN".into(),
        };
        let json = serde_json::to_value(&reference).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "env", "name": "SLACK_TOKEN"})
        );
    }

    #[test]
    fn a_new_source_record_starts_with_no_cursor_and_has_never_run() {
        let record = SourceRecord::new(&reference(None, "/a"), None);
        assert!(record.cursor.is_null());
        assert!(record.last_run_at.is_none());
        assert_eq!(record.id, reference(None, "/a").id());
    }

    #[test]
    fn a_document_key_is_stable_and_depends_on_both_source_and_document() {
        let a = reference(None, "/a").id();
        let b = reference(None, "/b").id();
        assert_eq!(
            SourceDocumentRecord::key(a, "x.md"),
            SourceDocumentRecord::key(a, "x.md")
        );
        assert_ne!(
            SourceDocumentRecord::key(a, "x.md"),
            SourceDocumentRecord::key(b, "x.md")
        );
        assert_ne!(
            SourceDocumentRecord::key(a, "x.md"),
            SourceDocumentRecord::key(a, "y.md")
        );
    }
}
