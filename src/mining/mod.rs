//! The mining engine: turns a mining source into drawers.
//!
//! Intentionally the simplest thing that is still real mining, not a stub:
//! one drawer per file, verbatim (no chunking, no entity extraction, no
//! embeddings — see the architecture doc's non-goals list for what's
//! deliberately deferred). What matters for this bootstrap is that the
//! *pipeline* — job submission -> scheduler -> checkpointed execution ->
//! durable writes -> lexical search over the result — is genuinely
//! end-to-end, so later chunking/extraction phases slot into
//! [`mine_directory`]'s per-file loop rather than requiring a rewrite of it.
//!
//! [`run`] is the seam Phase 5 slots a non-filesystem reader behind: it
//! dispatches on `domain::MiningSource`, today's only variant
//! (`Directory`) routing to [`mine_directory`] below. Adding a source kind
//! means adding a `MiningSource` variant (`domain::job`) and a matching arm
//! here — `Scheduler::execute`'s dispatch, which just forwards `JobKind::Mine`'s
//! fields through unchanged, never needs to change.

use std::path::{Path, PathBuf};

use crate::domain::Job;
use crate::domain::{Drawer, DrawerId, MiningSource, Provenance, Source, SourceKind};
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
/// at an enormous tree finishes in bounded time (a job stuck for hours with no
/// visible progress is worse than one that stops early). Files beyond the
/// ceiling are silently not mined: the progress total is capped at this
/// value, not the tree's real size.
const MAX_FILES: usize = 2_000;

/// Mine `source` into `wing`, checking in with `ctx` between units of work
/// so the job can be paused, resumed, or cancelled.
///
/// # Errors
///
/// Returns an error if `source` cannot be read, or if a store write fails.
pub async fn run(
    store: &SurrealStore,
    ctx: &JobContext,
    job: &mut Job,
    source: &MiningSource,
    wing: Option<&str>,
) -> Result<JobOutcome> {
    match source {
        MiningSource::Directory { path } => mine_directory(store, ctx, job, path, wing).await,
    }
}

/// Mine `path` into `wing` (or a wing named after `path`'s final component),
/// one drawer per file, checking in with `ctx` between files so the job can
/// be paused, resumed, or cancelled.
///
/// # Errors
///
/// Returns an error if `path` cannot be walked, or if a store write fails.
async fn mine_directory(
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
            ctx.checkpoint_at(job, index, files.len(), "mined", "files")
                .await?;
            return Ok(JobOutcome::Paused);
        }

        if let Some(content) = read_mineable(file) {
            let drawer = Drawer::new(
                // Derived, not random: a crash between the write below and
                // the checkpoint at the loop's end makes the resumed attempt
                // redo this same file, and a fresh id would store it twice.
                DrawerId::derive(job.id.0, &format!("mine-drawer:{index}")),
                room.id,
                content,
                Source {
                    kind: SourceKind::File,
                    uri: Some(file.display().to_string()),
                    agent: None,
                },
                vec![],
                Provenance {
                    requested_by: job.requested_by.clone(),
                    job_id: Some(job.id),
                },
            );
            store.create_drawer_once(&drawer).await?;
        }

        ctx.checkpoint_at(job, index + 1, files.len(), "mined", "files")
            .await?;
    }

    Ok(JobOutcome::Completed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{JobKind, Priority};
    use crate::jobs::JobControl;
    use serde_json::json;

    #[tokio::test]
    async fn replaying_files_whose_checkpoint_was_never_saved_mines_no_duplicates() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), format!("contents of {name}")).unwrap();
        }
        let source = MiningSource::Directory {
            path: dir.path().to_path_buf(),
        };
        let mut job = Job::new(
            JobKind::Mine {
                source: source.clone(),
                wing: Some("docs".to_string()),
            },
            Priority::Background,
            "test",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();

        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());
        run(&store, &ctx, &mut job, &source, Some("docs"))
            .await
            .unwrap();

        // Crash: every drawer was written, but the saved checkpoint predates
        // them, so the resumed attempt walks the same files again.
        job.checkpoint = json!({});
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());
        run(&store, &ctx, &mut job, &source, Some("docs"))
            .await
            .unwrap();

        assert_eq!(
            store.list_all_drawers().await.unwrap().len(),
            3,
            "each file must be mined exactly once across the replay"
        );
    }
}
