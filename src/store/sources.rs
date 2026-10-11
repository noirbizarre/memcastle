//! Mining source repository methods: the `source` and `source_document` tables.
//!
//! Bookkeeping for incremental, idempotent mining (docs/adr/023). Drawers stay the canonical memory; nothing here
//! is read by search.
//!
//! Optional objects (`credential`, `cursor`) are stored as `{}` when absent rather than `NONE`: an absent value of
//! an `object` field is a schema error, and `{}` costs nothing to read back.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::domain::{
    ChunkRef, CredentialRef, Cursor, JobId, SourceDocumentRecord, SourceId, SourceRecord, SourceRef,
};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The projection every source read shares.
const SOURCE_COLUMNS: &str = "record::id(id) AS id, source, account, locator, credential, cursor, last_job, \
     last_run_at, <string>created_at AS created_at, <string>updated_at AS updated_at";

/// The projection every source document read shares.
const DOCUMENT_COLUMNS: &str = "source, external_id, revision, raw, raw_hash, title, metadata, occurred_at, chunks, \
     <string>acquired_at AS acquired_at, job";

/// A `source` row as stored, before `{}` is turned back into "none".
#[derive(Deserialize)]
struct SourceRow {
    id: SourceId,
    source: String,
    #[serde(default)]
    account: Option<String>,
    locator: String,
    #[serde(default)]
    credential: Value,
    #[serde(default)]
    cursor: Value,
    #[serde(default)]
    last_job: Option<JobId>,
    #[serde(default)]
    last_run_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SourceRow {
    fn into_record(self) -> Result<SourceRecord> {
        // `{}` is how "no credential" is stored; anything else must be a valid reference.
        let credential = match self.credential {
            Value::Object(ref map) if map.is_empty() => None,
            Value::Null => None,
            other => Some(
                serde_json::from_value::<CredentialRef>(other)
                    .map_err(|source| Error::store_malformed(source.to_string()))?,
            ),
        };
        Ok(SourceRecord {
            id: self.id,
            source: self.source,
            account: self.account,
            locator: self.locator,
            credential,
            cursor: from_stored_cursor(self.cursor),
            last_job: self.last_job,
            last_run_at: self.last_run_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// A cursor as the `object` column holds it: an empty object is "from the beginning".
fn to_stored_cursor(cursor: &Cursor) -> Value {
    if cursor.is_null() {
        Value::Object(Map::new())
    } else {
        cursor.clone()
    }
}

/// The inverse of [`to_stored_cursor`].
fn from_stored_cursor(stored: Value) -> Cursor {
    match stored {
        Value::Object(ref map) if map.is_empty() => Cursor::Null,
        other => other,
    }
}

impl SurrealStore {
    /// Whether this source ID has ever been mined, even when its old package was removed.
    pub async fn has_mined_source_name(&self, name: &str) -> Result<bool> {
        let mut response = self
            .db
            .query("SELECT VALUE record::id(id) FROM source WHERE source = $name LIMIT 1")
            .bind(("name", name.to_string()))
            .await?;
        let rows: Vec<String> = super::take_rows(&mut response, 0)?;
        Ok(!rows.is_empty())
    }
    /// The source with this id, or `None`.
    pub async fn get_source(&self, id: SourceId) -> Result<Option<SourceRecord>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {SOURCE_COLUMNS} FROM source WHERE id = type::record('source', $id)"
            ))
            .bind(("id", id.to_string()))
            .await?;
        let mut rows: Vec<SourceRow> = super::take_rows(&mut response, 0)?;
        rows.pop().map(SourceRow::into_record).transpose()
    }

    /// The source named by `reference`, creating it (no cursor, never run) the first time a job mines it.
    ///
    /// `credential` only applies on creation: a source's stored credential reference is changed deliberately,
    /// never as a side effect of mining.
    pub async fn get_or_create_source(
        &self,
        reference: &SourceRef,
        credential: Option<CredentialRef>,
    ) -> Result<SourceRecord> {
        if let Some(existing) = self.get_source(reference.id()).await? {
            return Ok(existing);
        }
        let record = SourceRecord::new(reference, credential);
        let created = self
            .db
            .query(
                "CREATE type::record('source', $id) SET \
                 source = $source, account = $account, locator = $locator, credential = $credential, \
                 cursor = $cursor, last_job = NONE, last_run_at = NONE, \
                 created_at = <datetime>$created_at, updated_at = <datetime>$updated_at",
            )
            .bind(("id", record.id.to_string()))
            .bind(("source", record.source.clone()))
            .bind(("account", record.account.clone()))
            .bind(("locator", record.locator.clone()))
            .bind((
                "credential",
                match &record.credential {
                    Some(reference) => super::bindable(reference)?,
                    None => Value::Object(Map::new()),
                },
            ))
            .bind(("cursor", to_stored_cursor(&record.cursor)))
            .bind(("created_at", super::stored(record.created_at)))
            .bind(("updated_at", super::stored(record.updated_at)))
            .await?
            .check();
        // Two daemons sharing a remote palace can race to create the same source: the loser finds it there.
        if let Err(error) = created {
            return match self.get_source(record.id).await? {
                Some(existing) => Ok(existing),
                None => Err(error.into()),
            };
        }
        Ok(record)
    }

    /// Every source ever mined, by adapter then locator.
    pub async fn list_sources(&self) -> Result<Vec<SourceRecord>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {SOURCE_COLUMNS} FROM source ORDER BY source, locator"
            ))
            .await?;
        let rows: Vec<SourceRow> = super::take_rows(&mut response, 0)?;
        rows.into_iter().map(SourceRow::into_record).collect()
    }

    /// Record where `job` stopped on source `id`.
    ///
    /// Called only after everything before the cursor is durable (drawers, then the document record), so a crash
    /// leaves the cursor behind the data, never ahead of it.
    pub async fn save_source_cursor(
        &self,
        id: SourceId,
        cursor: &Cursor,
        job: JobId,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.db
            .query(
                "UPDATE type::record('source', $id) SET cursor = $cursor, last_job = $job, \
                 last_run_at = $at, updated_at = <datetime>$at",
            )
            .bind(("id", id.to_string()))
            .bind(("cursor", to_stored_cursor(cursor)))
            .bind(("job", job.to_string()))
            .bind(("at", super::stored(at)))
            .await?
            .check()?;
        Ok(())
    }

    /// The stored record of one document of a source, or `None` if it was never ingested.
    pub async fn get_source_document(
        &self,
        source: SourceId,
        external_id: &str,
    ) -> Result<Option<SourceDocumentRecord>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {DOCUMENT_COLUMNS} FROM source_document WHERE id = type::record('source_document', $key)"
            ))
            .bind(("key", SourceDocumentRecord::key(source, external_id)))
            .await?;
        let mut rows: Vec<SourceDocumentRecord> = super::take_rows(&mut response, 0)?;
        Ok(rows.pop())
    }

    /// Store `document`, replacing whatever was recorded for the same document before.
    pub async fn save_source_document(&self, document: &SourceDocumentRecord) -> Result<()> {
        let chunks: Vec<ChunkRef> = document.chunks.clone();
        self.db
            .query(
                "UPSERT type::record('source_document', $key) SET \
                 source = $source, external_id = $external_id, revision = $revision, raw = $raw, \
                 raw_hash = $raw_hash, title = $title, metadata = $metadata, occurred_at = $occurred_at, \
                 chunks = $chunks, acquired_at = <datetime>$acquired_at, job = $job",
            )
            .bind((
                "key",
                SourceDocumentRecord::key(document.source, &document.external_id),
            ))
            .bind(("source", document.source.to_string()))
            .bind(("external_id", document.external_id.clone()))
            .bind(("revision", document.revision.clone()))
            .bind(("raw", document.raw.clone()))
            .bind(("raw_hash", document.raw_hash.clone()))
            .bind(("title", document.title.clone()))
            .bind((
                "metadata",
                if document.metadata.is_object() {
                    document.metadata.clone()
                } else {
                    Value::Object(Map::new())
                },
            ))
            .bind(("occurred_at", document.occurred_at.map(super::stored)))
            .bind(("chunks", super::bindable(&chunks)?))
            .bind(("acquired_at", super::stored(document.acquired_at)))
            .bind(("job", document.job.map(|job| job.to_string())))
            .await?
            .check()?;
        Ok(())
    }

    /// How many documents of source `id` have been ingested.
    pub async fn count_source_documents(&self, id: SourceId) -> Result<u64> {
        #[derive(Deserialize)]
        struct Count {
            count: u64,
        }
        let mut response = self
            .db
            .query("SELECT count() AS count FROM source_document WHERE source = $source GROUP ALL")
            .bind(("source", id.to_string()))
            .await?;
        let counts: Vec<Count> = super::take_rows(&mut response, 0)?;
        Ok(counts.into_iter().next().map_or(0, |c| c.count))
    }
}

#[cfg(test)]
impl SurrealStore {
    /// Forget every document record of a source, simulating a crash between filing drawers and recording them.
    pub(crate) async fn delete_source_documents_for_tests(&self, id: SourceId) {
        self.db
            .query("DELETE source_document WHERE source = $source")
            .bind(("source", id.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    async fn store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    fn reference(locator: &str) -> SourceRef {
        SourceRef::new("demo", None, locator)
    }

    fn document(source: SourceId, external_id: &str, revision: &str) -> SourceDocumentRecord {
        SourceDocumentRecord {
            source,
            external_id: external_id.into(),
            revision: revision.into(),
            raw: Some("raw".into()),
            raw_hash: "hash".into(),
            title: Some("title".into()),
            metadata: json!({"k": "v"}),
            occurred_at: Some(Utc::now()),
            chunks: vec![ChunkRef {
                index: 0,
                hash: "h0".into(),
                drawer: crate::domain::DrawerId::new(),
            }],
            acquired_at: Utc::now(),
            job: Some(JobId::new()),
        }
    }

    #[tokio::test]
    async fn a_source_is_created_once_and_found_again_by_its_identity() {
        let store = store().await;
        let first = store
            .get_or_create_source(&reference("/a"), None)
            .await
            .unwrap();
        let second = store
            .get_or_create_source(&reference("/a"), None)
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(store.list_sources().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_new_source_has_no_cursor_and_no_credential() {
        let store = store().await;
        let source = store
            .get_or_create_source(&reference("/a"), None)
            .await
            .unwrap();
        let read = store.get_source(source.id).await.unwrap().unwrap();
        assert!(
            read.cursor.is_null(),
            "an empty stored cursor reads back as the beginning"
        );
        assert!(read.credential.is_none());
        assert!(read.last_job.is_none() && read.last_run_at.is_none());
    }

    #[tokio::test]
    async fn a_credential_reference_round_trips_without_any_secret_value() {
        let store = store().await;
        let credential = CredentialRef::Env {
            name: "TOKEN_VAR".into(),
        };
        let source = store
            .get_or_create_source(&reference("/a"), Some(credential.clone()))
            .await
            .unwrap();
        let read = store.get_source(source.id).await.unwrap().unwrap();
        assert_eq!(read.credential, Some(credential));
    }

    #[tokio::test]
    async fn a_saved_cursor_survives_a_read_and_records_the_job_that_advanced_it() {
        let store = store().await;
        let source = store
            .get_or_create_source(&reference("/a"), None)
            .await
            .unwrap();
        let job = JobId::new();
        let cursor = json!({"mtime_ns": 42, "key": "a/b.md"});
        store
            .save_source_cursor(source.id, &cursor, job, Utc::now())
            .await
            .unwrap();
        let read = store.get_source(source.id).await.unwrap().unwrap();
        assert_eq!(read.cursor, cursor);
        assert_eq!(read.last_job, Some(job));
        assert!(read.last_run_at.is_some());
    }

    #[tokio::test]
    async fn resetting_a_cursor_to_the_beginning_reads_back_as_null() {
        let store = store().await;
        let source = store
            .get_or_create_source(&reference("/a"), None)
            .await
            .unwrap();
        store
            .save_source_cursor(source.id, &json!({"n": 1}), JobId::new(), Utc::now())
            .await
            .unwrap();
        store
            .save_source_cursor(source.id, &Cursor::Null, JobId::new(), Utc::now())
            .await
            .unwrap();
        let read = store.get_source(source.id).await.unwrap().unwrap();
        assert!(read.cursor.is_null());
    }

    #[tokio::test]
    async fn a_source_document_round_trips_with_its_chunks() {
        let store = store().await;
        let source = reference("/a").id();
        let saved = document(source, "x/y.md", "r1");
        store.save_source_document(&saved).await.unwrap();
        let read = store
            .get_source_document(source, "x/y.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.revision, "r1");
        assert_eq!(read.chunks, saved.chunks);
        assert_eq!(read.raw.as_deref(), Some("raw"));
        assert_eq!(read.metadata, json!({"k": "v"}));
    }

    #[tokio::test]
    async fn saving_a_document_again_replaces_it_instead_of_adding_a_second_row() {
        let store = store().await;
        let source = reference("/a").id();
        store
            .save_source_document(&document(source, "x.md", "r1"))
            .await
            .unwrap();
        store
            .save_source_document(&document(source, "x.md", "r2"))
            .await
            .unwrap();
        assert_eq!(store.count_source_documents(source).await.unwrap(), 1);
        let read = store
            .get_source_document(source, "x.md")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.revision, "r2");
    }

    #[tokio::test]
    async fn a_document_without_raw_or_title_is_stored_and_read_back_as_none() {
        let store = store().await;
        let source = reference("/a").id();
        let mut bare = document(source, "x.md", "r1");
        bare.raw = None;
        bare.title = None;
        bare.occurred_at = None;
        bare.job = None;
        store.save_source_document(&bare).await.unwrap();
        let read = store
            .get_source_document(source, "x.md")
            .await
            .unwrap()
            .unwrap();
        assert!(read.raw.is_none() && read.title.is_none() && read.job.is_none());
    }

    #[tokio::test]
    async fn documents_are_counted_per_source() {
        let store = store().await;
        let a = reference("/a").id();
        let b = reference("/b").id();
        store
            .save_source_document(&document(a, "1", "r"))
            .await
            .unwrap();
        store
            .save_source_document(&document(a, "2", "r"))
            .await
            .unwrap();
        store
            .save_source_document(&document(b, "1", "r"))
            .await
            .unwrap();
        assert_eq!(store.count_source_documents(a).await.unwrap(), 2);
        assert_eq!(store.count_source_documents(b).await.unwrap(), 1);
        assert_eq!(
            store
                .count_source_documents(reference("/c").id())
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn an_unknown_document_is_none() {
        let store = store().await;
        assert!(
            store
                .get_source_document(reference("/a").id(), "nope")
                .await
                .unwrap()
                .is_none()
        );
    }
}
