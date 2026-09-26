//! The mining engine: turns a directory on disk into drawers.
//!
//! Intentionally the simplest thing that is still real mining, not a stub:
//! one drawer per file, verbatim (no chunking, no entity extraction, no
//! embeddings — see the architecture doc's non-goals list for what's
//! deliberately deferred). What matters for this bootstrap is that the
//! *pipeline* — job submission -> scheduler -> checkpointed execution ->
//! durable writes -> lexical search over the result — is genuinely
//! end-to-end, so later chunking/extraction phases slot into
//! [`run`]'s per-file loop rather than requiring a rewrite of it.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::domain::Job;
use crate::domain::{Drawer, DrawerId, Provenance, Source, SourceKind};
use crate::error::Result;
use crate::jobs::{JobContext, JobOutcome};
use crate::store::SurrealStore;

/// Directories never worth mining — build output, VCS metadata, dependency
/// trees. Skipped by name at any depth, mirroring the cheap denylist
/// mempalace-rs's miner uses before anything fancier (gitignore-awareness)
/// is worth the added dependency.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    ".cache",
];

/// Above this size, a file is skipped rather than truncated silently into a
/// misleading drawer.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// A hard ceiling on how many files one mining job will file, so pointing it
/// at an enormous tree fails predictably (a job stuck for hours with no
/// visible progress is worse than one that stops early with a clear count).
const MAX_FILES: usize = 2_000;

/// Mine `path` into `wing` (or a wing named after `path`'s final component),
/// one drawer per file, checking in with `ctx` between files so the job can
/// be paused, resumed, or cancelled.
///
/// # Errors
///
/// Returns an error if `path` cannot be walked, or if a store write fails.
pub async fn run(
    store: &SurrealStore,
    ctx: &JobContext,
    job: &mut Job,
    path: &Path,
    wing: Option<&str>,
) -> Result<JobOutcome> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|source| crate::Error::io(path.display().to_string(), source))?;

    let wing_name = wing.map(str::to_string).unwrap_or_else(|| {
        canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".to_string())
    });
    let wing = store.get_or_create_wing(&wing_name, None).await?;
    let room = store.get_or_create_room(wing.id, "files", None).await?;

    // Rebuilding and re-sorting the file list on every attempt (rather than
    // persisting it) is what makes the checkpoint below meaningful: as long
    // as the tree hasn't changed, "resume from file N" means the same file
    // N both times.
    let mut files = Vec::new();
    collect_files(&canonical, &mut files);
    files.sort();
    files.truncate(MAX_FILES);

    let start = job
        .checkpoint
        .get("next_index")
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| n as usize);

    for (index, file) in files.iter().enumerate().skip(start) {
        if ctx.is_cancelled() {
            return Ok(JobOutcome::Cancelled);
        }
        if ctx.should_pause() {
            checkpoint_at(ctx, job, index, files.len()).await?;
            return Ok(JobOutcome::Paused);
        }

        if let Some(content) = read_mineable(file) {
            let now = Utc::now();
            let mut hasher = Sha256::new();
            hasher.update(content.as_bytes());
            let content_hash = hex_encode(&hasher.finalize());

            let drawer = Drawer {
                id: DrawerId::new(),
                room: room.id,
                content,
                content_hash,
                source: Source {
                    kind: SourceKind::File,
                    uri: Some(file.display().to_string()),
                    agent: None,
                },
                tags: vec![],
                embedding: None,
                provenance: Provenance {
                    requested_by: job.requested_by.clone(),
                    job_id: Some(job.id),
                },
                valid_from: now,
                valid_to: None,
                created_at: now,
                updated_at: now,
            };
            store.create_drawer(&drawer).await?;
        }

        checkpoint_at(ctx, job, index + 1, files.len()).await?;
    }

    Ok(JobOutcome::Completed)
}

async fn checkpoint_at(ctx: &JobContext, job: &mut Job, index: usize, total: usize) -> Result<()> {
    let progress = crate::domain::JobProgress {
        current: index as u32,
        total: Some(total as u32),
        message: Some(format!("mined {index}/{total} files")),
    };
    ctx.checkpoint(job, progress, json!({ "next_index": index }))
        .await
}

/// Recursively collect file paths under `dir`, skipping noisy subtrees and
/// symlinks (a symlink escaping the project root is a real hazard a project
/// miner has to guard against — see the reference implementations' own
/// canonicalize-and-`starts_with`-root checks; here it's simpler still:
/// don't follow symlinks at all).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_files(&path, out);
        } else if file_type.is_file() {
            out.push(path);
        }
        if out.len() >= MAX_FILES {
            return;
        }
    }
}

/// Lowercase hex, without pulling in a dependency just for this — `sha2`'s
/// `finalize()` returns a fixed-size byte array, not something that
/// implements `LowerHex` directly.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read `path` as UTF-8 text, or `None` if it's too large, empty, or not
/// valid UTF-8 — a silent skip rather than a job-ending error, since a mixed
/// tree of text and binary files is the normal case, not an exceptional one.
fn read_mineable(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    String::from_utf8(bytes).ok()
}
