//! The mining engine: turns a mining source into drawers.
//!
//! Intentionally the simplest thing that is still real mining, not a stub:
//! one drawer per file, verbatim (no chunking, no entity extraction, no
//! embeddings; the architecture doc's non-goals list covers the last two, and
//! chunking is deferred along with them). What matters for this bootstrap is that the
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
/// ceiling are not mined, and the job says so: its progress message names the
/// limit and its `result` records `truncated: true`, so `mined 2000/2000` is
/// never mistaken for "the whole tree".
const MAX_FILES: usize = 2_000;

/// Mine `source` into `wing`, checking in with `ctx` between units of work
/// so the job can be paused, resumed, or cancelled.
///
/// # Errors
///
/// Returns an error if `source` cannot be read, or if a store write fails.
pub async fn run(ctx: &JobContext, job: &mut Job, params: MiningParams) -> Result<JobOutcome> {
    let MiningParams { source, wing } = params;
    let store = ctx.store();
    match source {
        MiningSource::Directory { path } => {
            mine_directory(store, ctx, job, &path, wing.as_deref(), MAX_FILES).await
        }
    }
}

/// What a `Mine` job needs, gathered from its [`crate::domain::JobKind`].
pub struct MiningParams {
    /// Where to mine from.
    pub source: MiningSource,
    /// The wing to file drawers under (defaults to one named after the
    /// source).
    pub wing: Option<String>,
}

/// Mine `path` into `wing` (or a wing named after `path`'s final component),
/// one drawer per file, checking in with `ctx` between files so the job can
/// be paused, resumed, or cancelled.
///
/// At most `max_files` files are mined (see [`MAX_FILES`]); if the tree holds
/// more, that is recorded on the job rather than left to be inferred.
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
    max_files: usize,
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
    // One past the limit, so "exactly at the limit" and "over it" can be told
    // apart without walking the rest of a tree that may be enormous.
    collect_files(&canonical, &mut files, max_files + 1);
    files.sort();
    let truncated = files.len() > max_files;
    files.truncate(max_files);
    // Said on every progress line, not just at the end: a job someone is
    // watching mid-run must not look like it is covering the whole tree.
    let unit = if truncated {
        format!("files (limit of {max_files} reached: the rest of the tree is skipped)")
    } else {
        "files".to_string()
    };

    let start = JobContext::resume_index(job, "next_index");
    // Counts only: file names and contents stay out of the log.
    tracing::info!(
        files = files.len(),
        resume_from = start,
        truncated,
        "mining directory"
    );

    for (index, file) in files.iter().enumerate().skip(start) {
        if ctx.is_cancelled() {
            return Ok(JobOutcome::Cancelled);
        }
        if ctx.should_pause() {
            ctx.checkpoint_at(job, index, files.len(), "mined", &unit)
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

        ctx.checkpoint_at(job, index + 1, files.len(), "mined", &unit)
            .await?;
    }

    // The only report a mining job leaves besides its progress line, and the
    // place a caller checks whether the whole tree was covered.
    job.result = Some(serde_json::json!({
        "files_considered": files.len(),
        "limit": max_files,
        "truncated": truncated,
    }));
    Ok(JobOutcome::Completed)
}

/// Recursively collect file paths under `dir`, skipping noisy subtrees and
/// symlinks (a symlink escaping the project root is a real hazard a project
/// miner has to guard against — see the reference implementations' own
/// canonicalize-and-`starts_with`-root checks; here it's simpler still:
/// don't follow symlinks at all).
///
/// Stops once `limit` files are collected. Entries are visited in name order,
/// so which files a truncated walk keeps does not depend on the filesystem's
/// directory order — the resume checkpoint means "file N" only if both
/// attempts saw the same first `limit` files.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
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
            collect_files(&path, out, limit);
        } else if file_type.is_file() {
            out.push(path);
        }
        if out.len() >= limit {
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
        run(
            &ctx,
            &mut job,
            MiningParams {
                source: source.clone(),
                wing: Some("docs".to_string()),
            },
        )
        .await
        .unwrap();

        // Crash: every drawer was written, but the saved checkpoint predates
        // them, so the resumed attempt walks the same files again.
        job.checkpoint = json!({});
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());
        run(
            &ctx,
            &mut job,
            MiningParams {
                source: source.clone(),
                wing: Some("docs".to_string()),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            store.list_drawers(None).await.unwrap().len(),
            3,
            "each file must be mined exactly once across the replay"
        );
    }

    /// `{"next_index": N}` is persisted in job records already on disk, so a
    /// handler refactor must keep reading exactly that shape.
    #[tokio::test]
    async fn a_checkpoint_persisted_in_the_current_format_resumes_past_the_finished_files() {
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
        job.checkpoint = json!({ "next_index": 2 });

        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());
        run(
            &ctx,
            &mut job,
            MiningParams {
                source,
                wing: Some("docs".to_string()),
            },
        )
        .await
        .unwrap();

        let drawers = store.list_drawers(None).await.unwrap();
        assert_eq!(drawers.len(), 1, "only the last file was still to mine");
        assert_eq!(drawers[0].content, "contents of c.txt");
    }

    #[tokio::test]
    async fn a_mined_drawer_records_the_channel_as_requested_by_and_no_agent() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "contents").unwrap();
        let source = MiningSource::Directory {
            path: dir.path().to_path_buf(),
        };
        let mut job = Job::new(
            JobKind::Mine {
                source: source.clone(),
                wing: None,
            },
            Priority::Background,
            "cli",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());

        run(&ctx, &mut job, MiningParams { source, wing: None })
            .await
            .unwrap();

        let drawers = store.list_drawers(None).await.unwrap();
        assert_eq!(drawers[0].provenance.requested_by, "cli");
        assert_eq!(
            drawers[0].source.agent, None,
            "mining has no agent identity"
        );
    }

    async fn mine_with_limit(files: usize, limit: usize) -> (Job, Vec<String>) {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for i in 0..files {
            std::fs::write(dir.path().join(format!("f{i:02}.txt")), format!("file {i}")).unwrap();
        }
        let mut job = Job::new(
            JobKind::Mine {
                source: MiningSource::Directory {
                    path: dir.path().to_path_buf(),
                },
                wing: Some("docs".to_string()),
            },
            Priority::Background,
            "test",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone());

        mine_directory(&store, &ctx, &mut job, dir.path(), Some("docs"), limit)
            .await
            .unwrap();

        let mut mined: Vec<String> = store
            .list_drawers(None)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|drawer| drawer.source.uri)
            // `Path`, not a split on `/`: the separator is `\` on Windows.
            .filter_map(|uri| {
                Path::new(&uri)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        mined.sort();
        (job, mined)
    }

    #[tokio::test]
    async fn a_tree_over_the_file_limit_says_files_were_skipped() {
        let (job, mined) = mine_with_limit(5, 3).await;

        assert_eq!(
            mined,
            ["f00.txt", "f01.txt", "f02.txt"],
            "only the first three files, in name order, are mined"
        );
        let result = job.result.expect("mining reports a summary");
        assert_eq!(result["truncated"], true);
        assert_eq!(result["limit"], 3);
        let message = job.progress.message.expect("progress message");
        assert!(
            message.contains("limit of 3") && message.contains("skipped"),
            "`mined 3/3 files` alone would look complete: {message}"
        );
    }

    #[tokio::test]
    async fn a_tree_exactly_at_the_limit_is_not_reported_as_truncated() {
        let (job, mined) = mine_with_limit(3, 3).await;

        assert_eq!(mined.len(), 3);
        assert_eq!(job.result.unwrap()["truncated"], false);
        assert_eq!(job.progress.message.as_deref(), Some("mined 3/3 files"));
    }

    #[tokio::test]
    async fn which_files_a_truncated_walk_keeps_does_not_depend_on_directory_order() {
        let (_, first) = mine_with_limit(6, 2).await;
        let (_, second) = mine_with_limit(6, 2).await;

        // Both walks keep f00 and f01; the resume checkpoint relies on it.
        assert_eq!(first, ["f00.txt", "f01.txt"]);
        assert_eq!(first, second);
    }
}
