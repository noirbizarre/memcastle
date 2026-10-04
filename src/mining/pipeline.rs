//! The mining pipeline: one loop for every source.
//!
//! Given a [`SourceAdapter`], it discovers what is new past the source's stored cursor, reads each candidate,
//! skips what is unchanged, normalizes and chunks what is not, files the chunks as drawers, and only then moves
//! the cursor. It knows nothing about any particular source: no file system, no provider names (a test holds this
//! file to that), because the day it does, the next adapter needs a second pipeline.
//!
//! # Idempotence
//!
//! Re-reading is always safe, so the cursor is only an optimisation:
//!
//! - an unchanged document (same revision as last time) is skipped before it is even normalized;
//! - a changed document keeps every chunk whose hash is unchanged and supersedes only the ones that differ, so
//!   history stays (`valid_to`) and a point-in-time search still sees what was believed then;
//! - drawer ids are derived from (job, source, document, chunk, hash), so a replay after a crash lands on the
//!   records it already wrote (docs/adr/008).
//!
//! # Order of commits
//!
//! For each document: drawers, then the document record, then the source's cursor, then the job's checkpoint. A
//! crash between any two leaves the later ones behind the earlier ones and the replay redoes, harmlessly, what the
//! later ones had not yet recorded. The cursor is never ahead of the data.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::domain::{
    CanonicalDocument, ChunkRef, Drawer, DrawerId, Job, JobProgress, NameKind, Origin, Provenance,
    RawDocument, Source, SourceDocumentRecord, SourceRecord, content_hash, sha256_hex,
    validate_name,
};
use crate::error::Result;
use crate::jobs::{JobContext, JobOutcome};
use crate::store::SurrealStore;

use super::adapter::SourceAdapter;
use super::chunk::chunk;

/// What one mining run was asked to do.
pub struct Request<'a> {
    /// The source's locator, or `None` for the adapter's default.
    pub locator: Option<&'a str>,
    /// The wing to file under, or `None` for the adapter's default.
    pub wing: Option<&'a str>,
    /// Ignore the stored cursor and start from the beginning.
    pub full: bool,
}

/// What a run did, kept in the job's checkpoint (so a resumed run keeps counting) and reported as its result.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stats {
    /// Documents handled, whatever happened to them.
    pub documents: u64,
    /// Chunks filed as new drawers.
    pub created: u64,
    /// Chunks whose drawer was superseded by a changed one.
    pub superseded: u64,
    /// Chunks whose drawer was closed because the document shrank.
    pub retired: u64,
    /// Documents skipped because their revision had not changed.
    pub unchanged: u64,
    /// Documents the adapter chose not to read (binary, too large, gone).
    pub skipped: u64,
}

/// Mine one source with `adapter`, checking in with `ctx` between documents so the job can be paused, resumed or
/// cancelled.
///
/// # Errors
///
/// Returns an error if the source cannot be identified, discovered or read, or if a store write fails.
pub async fn mine<A: SourceAdapter>(
    adapter: &A,
    ctx: &JobContext,
    job: &mut Job,
    request: Request<'_>,
) -> Result<JobOutcome> {
    let store = ctx.store();
    let settings = ctx.mining();
    let reference = adapter.identify(request.locator)?;
    let source = store.get_or_create_source(&reference, None).await?;

    let wing_name = request
        .wing
        .map_or_else(|| adapter.default_wing(&reference), str::to_string);
    let wing = store.get_or_create_wing(&wing_name, None).await?;

    // A run that already checkpointed continues from its own cursor, whatever `full` says: `full` means "from the
    // beginning", and the beginning is where this run already started.
    let mut stats: Stats = job
        .checkpoint
        .get("stats")
        .and_then(|stats| serde_json::from_value(stats.clone()).ok())
        .unwrap_or_default();
    let cursor = match job.checkpoint.get("cursor") {
        Some(own) => own.clone(),
        None if request.full => serde_json::Value::Null,
        None => source.cursor.clone(),
    };

    let remaining = settings
        .max_documents
        .saturating_sub(usize::try_from(stats.documents).unwrap_or(usize::MAX));
    let discovery = adapter.discover(&reference, &cursor, remaining).await?;
    let truncated = !discovery.exhausted;
    let base = stats.documents;
    let total = base + discovery.candidates.len() as u64;
    // Counts only: names, paths and contents stay out of the log.
    tracing::info!(
        provider = adapter.provider(),
        candidates = discovery.candidates.len(),
        full = request.full,
        resumed = job.checkpoint.get("cursor").is_some(),
        truncated,
        "mining source"
    );

    for candidate in &discovery.candidates {
        if ctx.is_cancelled() {
            return Ok(JobOutcome::Cancelled);
        }
        if ctx.should_pause() {
            // Everything handled so far is already in the checkpoint: it is written after every document.
            return Ok(JobOutcome::Paused);
        }

        match adapter.read(&reference, candidate).await? {
            None => stats.skipped += 1,
            Some(raw) => {
                let existing = store
                    .get_source_document(source.id, &raw.external_id)
                    .await?;
                if existing
                    .as_ref()
                    .is_some_and(|known| known.revision == raw.revision)
                {
                    stats.unchanged += 1;
                } else {
                    let canonical = adapter.normalize(&raw)?;
                    let room_name = canonical
                        .room
                        .as_deref()
                        .filter(|name| validate_name(NameKind::Room, name).is_ok())
                        .unwrap_or_else(|| adapter.default_room());
                    let room = store.get_or_create_room(wing.id, room_name, None).await?;
                    let record = ingest(
                        store,
                        &Ingest {
                            provider: adapter.provider(),
                            retains_raw: adapter.capabilities().retains_raw,
                            chunk_chars: settings.chunk_chars,
                            source: &source,
                            room: room.id,
                            job,
                        },
                        &raw,
                        &canonical,
                        existing.as_ref(),
                        &mut stats,
                    )
                    .await?;
                    store.save_source_document(&record).await?;
                }
            }
        }
        stats.documents += 1;

        store
            .save_source_cursor(source.id, &candidate.cursor_after, job.id, Utc::now())
            .await?;
        let progress = JobProgress {
            current: u32::try_from(stats.documents).unwrap_or(u32::MAX),
            total: Some(u32::try_from(total).unwrap_or(u32::MAX)),
            message: Some(format!(
                "mined {}/{total} documents{}",
                stats.documents,
                if truncated {
                    " (limit reached: run again to continue)"
                } else {
                    ""
                }
            )),
        };
        ctx.checkpoint(
            job,
            progress,
            json!({ "cursor": candidate.cursor_after, "stats": stats }),
        )
        .await?;
    }

    // The only report a mining job leaves besides its progress line, and where a caller checks whether the whole
    // source was covered.
    job.result = Some(json!({
        "source": source.id,
        "provider": adapter.provider(),
        "documents": stats.documents,
        "created": stats.created,
        "superseded": stats.superseded,
        "retired": stats.retired,
        "unchanged": stats.unchanged,
        "skipped": stats.skipped,
        "limit": settings.max_documents,
        "truncated": truncated,
    }));
    Ok(JobOutcome::Completed)
}

/// Everything [`ingest`] needs besides the document itself.
struct Ingest<'a> {
    provider: &'a str,
    retains_raw: bool,
    chunk_chars: usize,
    source: &'a SourceRecord,
    room: crate::domain::RoomId,
    job: &'a Job,
}

/// File `canonical` as drawers, reusing, superseding or retiring those `existing` already holds, and return the
/// record of what the document now is.
async fn ingest(
    store: &SurrealStore,
    ctx: &Ingest<'_>,
    raw: &RawDocument,
    canonical: &CanonicalDocument,
    existing: Option<&SourceDocumentRecord>,
    stats: &mut Stats,
) -> Result<SourceDocumentRecord> {
    let texts = chunk(&canonical.segments, ctx.chunk_chars);
    let old_chunks: &[ChunkRef] = existing.map_or(&[], |known| known.chunks.as_slice());
    let mut refs = Vec::with_capacity(texts.len());

    for (position, text) in texts.iter().enumerate() {
        let index = u32::try_from(position).unwrap_or(u32::MAX);
        let hash = content_hash(text);
        let old = old_chunks.iter().find(|old| old.index == index);
        // The same text at the same position is already filed: nothing to write.
        if let Some(old) = old.filter(|old| old.hash == hash) {
            refs.push(old.clone());
            continue;
        }

        // Derived from the job as well as the chunk: a document reverted to an earlier text must open a new
        // drawer rather than collide with the closed one, while a replay inside this job still lands on the same
        // id (docs/adr/008).
        let id = DrawerId::derive(
            ctx.job.id.0,
            &format!(
                "mine-chunk:{}:{}:{index}:{hash}",
                ctx.source.id, raw.external_id
            ),
        );
        // Only the first chunk is addressable by name, and only if the name is free or is the one held by the
        // drawer this replaces (or by this very drawer, on a replay). A taken name is left alone: the new drawer
        // is unnamed instead of failing the job (docs/adr/018).
        let name = match (&canonical.name, index) {
            (Some(name), 0) => match store.get_drawer_by_name(ctx.room, name).await? {
                Some(holder) if holder.id != id && Some(holder.id) != old.map(|old| old.drawer) => {
                    None
                }
                _ => Some(name.clone()),
            },
            _ => None,
        };
        let drawer = Drawer::new(
            id,
            ctx.room,
            text.clone(),
            Source {
                kind: canonical.kind,
                uri: canonical.uri.clone(),
                agent: None,
                origin: Some(Origin {
                    source: ctx.source.id,
                    provider: ctx.provider.to_string(),
                    document: raw.external_id.clone(),
                    chunk: index,
                    revision: raw.revision.clone(),
                }),
            },
            canonical.tags.clone(),
            Provenance {
                requested_by: ctx.job.requested_by.clone(),
                job_id: Some(ctx.job.id),
            },
        )
        .with_name(name);

        let superseded = match old {
            // One transaction closes the old drawer and opens this one. `false` means the old one was already
            // closed (an earlier attempt of this job did it, or a person did): fall through to a plain create,
            // which is a no-op if this drawer exists already.
            Some(old) => {
                store
                    .supersede_drawer(old.drawer, Some(&drawer), Utc::now())
                    .await?
            }
            None => false,
        };
        if superseded {
            stats.superseded += 1;
        } else if store.create_drawer_once(&drawer).await? {
            stats.created += 1;
        }
        refs.push(ChunkRef {
            index,
            hash,
            drawer: id,
        });
    }

    // A document that got shorter leaves chunks past its new end: close them rather than leave stale text
    // searchable as if it were still in the source.
    for old in old_chunks
        .iter()
        .filter(|old| old.index as usize >= texts.len())
    {
        if store.supersede_drawer(old.drawer, None, Utc::now()).await? {
            stats.retired += 1;
        }
    }

    Ok(SourceDocumentRecord {
        source: ctx.source.id,
        external_id: raw.external_id.clone(),
        revision: raw.revision.clone(),
        raw: ctx.retains_raw.then(|| raw.body.clone()),
        raw_hash: sha256_hex(raw.body.as_bytes()),
        title: canonical.title.clone(),
        metadata: raw.metadata.clone(),
        occurred_at: raw.occurred_at,
        chunks: refs,
        acquired_at: Utc::now(),
        job: Some(ctx.job.id),
    })
}
