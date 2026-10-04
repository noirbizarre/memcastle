//! The mining engine: turns a source into drawers, without an agent.
//!
//! Mining is three stages (docs/adr/023), and only the first depends on what is being mined:
//!
//! ```text
//! acquire (adapter)  ->  normalize (adapter)  ->  chunk + ingest (core)  ->  [enrich: optional, later]
//!   discover, read          raw -> canonical        drawers, cursor
//! ```
//!
//! - [`adapter`] is the contract a source implements; [`adapters`] holds the sources MemCastle ships
//!   (`directory`, `pi-sessions`). Source-specific discovery and reading live there and nowhere else.
//! - [`pipeline`] is the one loop every source goes through: cursor, revision check, chunking, idempotent filing,
//!   cursor commit, checkpoint. It knows no provider by name.
//! - [`chunk`] cuts a canonical document into drawer-sized texts.
//!
//! [`run`] is the seam between the job and all of that: it resolves the job's source to an adapter and hands over.
//! Adding a source means adding an adapter and one arm in [`run`] and [`providers`]; `Scheduler::execute`'s
//! dispatch, the wire format and the pipeline never change.

pub mod adapter;
pub mod adapters;
pub mod chunk;
pub mod pipeline;

use std::path::Path;

use crate::domain::{Job, MiningSource, SourceCapabilities};
use crate::error::{Error, Result};
use crate::jobs::{JobContext, JobOutcome};

use adapter::SourceAdapter;
use adapters::directory::{self, DirectoryAdapter};
use adapters::pi_sessions::{self, PiSessionsAdapter};

/// What a `Mine` job needs, gathered from its [`crate::domain::JobKind`].
pub struct MiningParams {
    /// Where to mine from.
    pub source: MiningSource,
    /// The wing to file drawers under (defaults to one named after the source).
    pub wing: Option<String>,
    /// Ignore the source's cursor and read everything again.
    pub full: bool,
}

/// A source adapter the daemon ships, as `memcastle sources` and `GET /api/sources` describe it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderInfo {
    /// The name to give as `--source`.
    pub name: String,
    /// What it reads.
    pub description: String,
    /// What it can do.
    pub capabilities: SourceCapabilities,
}

/// Every provider, in the order they are listed.
#[must_use]
pub fn providers() -> Vec<ProviderInfo> {
    // Capabilities and descriptions do not depend on configuration, so default-configured adapters answer.
    let directory = DirectoryAdapter::new(0);
    let pi = PiSessionsAdapter::new(Some(Path::new("/")));
    vec![describe(&directory), describe(&pi)]
}

/// Whether `name` is a provider this daemon can mine.
#[must_use]
pub fn is_provider(name: &str) -> bool {
    providers().iter().any(|provider| provider.name == name)
}

fn describe(adapter: &impl SourceAdapter) -> ProviderInfo {
    ProviderInfo {
        name: adapter.provider().to_string(),
        description: adapter.description().to_string(),
        capabilities: adapter.capabilities(),
    }
}

/// Mine `params.source`, checking in with `ctx` between documents so the job can be paused, resumed, or
/// cancelled.
///
/// # Errors
///
/// Returns an error if the provider is unknown, the source cannot be reached or read, or a store write fails.
pub async fn run(ctx: &JobContext, job: &mut Job, params: MiningParams) -> Result<JobOutcome> {
    let MiningParams { source, wing, full } = params;
    // A directory job and a `directory` provider job are the same source: one wire form predates the other.
    let (provider, locator) = match &source {
        MiningSource::Directory { path } => (
            directory::PROVIDER,
            Some(path.to_string_lossy().into_owned()),
        ),
        MiningSource::Provider { provider, locator } => (provider.as_str(), locator.clone()),
    };
    let request = pipeline::Request {
        locator: locator.as_deref(),
        wing: wing.as_deref(),
        full,
    };
    let mining = ctx.mining();
    match provider {
        directory::PROVIDER => {
            pipeline::mine(
                &DirectoryAdapter::new(mining.max_file_bytes),
                ctx,
                job,
                request,
            )
            .await
        }
        pi_sessions::PROVIDER => {
            pipeline::mine(
                &PiSessionsAdapter::new(mining.pi_sessions_dir.as_deref()),
                ctx,
                job,
                request,
            )
            .await
        }
        other => Err(Error::invalid_input(
            "provider",
            format!(
                "unknown source `{other}`; known sources: {}",
                providers()
                    .iter()
                    .map(|provider| provider.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::config::MiningConfig;
    use crate::domain::{Drawer, JobEvent, JobKind, Priority, SourceKind};
    use crate::jobs::JobControl;
    use crate::store::SurrealStore;
    use serde_json::json;

    fn directory_job(dir: &Path, requested_by: &str) -> Job {
        let mut job = Job::new(
            JobKind::Mine {
                source: MiningSource::Directory {
                    path: dir.to_path_buf(),
                },
                wing: Some("docs".to_string()),
                full: false,
            },
            Priority::Background,
            requested_by,
        );
        job.apply(JobEvent::Claim).unwrap();
        job
    }

    fn params_of(job: &Job) -> MiningParams {
        let JobKind::Mine { source, wing, full } = job.kind.clone() else {
            unreachable!("every test job is a mine job")
        };
        MiningParams { source, wing, full }
    }

    async fn run_job(
        store: &SurrealStore,
        job: &mut Job,
        mining: MiningConfig,
    ) -> Result<JobOutcome> {
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone()).with_mining(mining);
        run(&ctx, job, params_of(job)).await
    }

    /// Mine `dir` as a fresh job and return it.
    async fn mine(store: &SurrealStore, dir: &Path, mining: MiningConfig) -> Job {
        let mut job = directory_job(dir, "test");
        assert_eq!(
            run_job(store, &mut job, mining).await.unwrap(),
            JobOutcome::Completed
        );
        job
    }

    async fn drawers(store: &SurrealStore) -> Vec<Drawer> {
        store.list_drawers(None).await.unwrap()
    }

    async fn open(store: &SurrealStore) -> Vec<Drawer> {
        let mut open: Vec<_> = drawers(store)
            .await
            .into_iter()
            .filter(|drawer| drawer.valid_to.is_none())
            .collect();
        open.sort_by(|a, b| a.content.cmp(&b.content));
        open
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Give `name` a modification time `seconds` after the epoch, so ordering never depends on how fast a test
    /// writes its files.
    fn touch(dir: &Path, name: &str, seconds: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }

    fn result(job: &Job) -> &serde_json::Value {
        job.result.as_ref().expect("a mining job reports a summary")
    }

    #[tokio::test]
    async fn every_file_of_a_directory_becomes_a_drawer_named_by_its_path() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "hello");
        write(dir.path(), "src/lib.rs", "fn main() {}");

        let job = mine(&store, dir.path(), MiningConfig::default()).await;

        assert_eq!(result(&job)["created"], 2);
        let wing = store.get_wing("docs").await.unwrap().unwrap();
        let room = store.get_room(wing.id, "files").await.unwrap().unwrap();
        for name in ["README.md", "src/lib.rs"] {
            assert!(
                store
                    .get_drawer_by_name(room.id, name)
                    .await
                    .unwrap()
                    .is_some(),
                "{name} should be addressable by its path"
            );
        }
    }

    #[tokio::test]
    async fn a_mined_drawer_records_its_source_document_and_chunk() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "contents");

        mine(&store, dir.path(), MiningConfig::default()).await;

        let drawer = &drawers(&store).await[0];
        let origin = drawer
            .source
            .origin
            .as_ref()
            .expect("a mined drawer has an origin");
        assert_eq!(origin.provider, "directory");
        assert_eq!(origin.document, "a.txt");
        assert_eq!(origin.chunk, 0);
        assert_eq!(drawer.source.kind, SourceKind::File);
        let sources = store.list_sources().await.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(origin.source, sources[0].id);
    }

    #[tokio::test]
    async fn a_mined_drawer_records_the_channel_as_requested_by_and_no_agent() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "contents");
        let mut job = directory_job(dir.path(), "cli");
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        let drawers = drawers(&store).await;
        assert_eq!(drawers[0].provenance.requested_by, "cli");
        assert_eq!(
            drawers[0].source.agent, None,
            "mining has no agent identity"
        );
    }

    #[tokio::test]
    async fn re_mining_an_unchanged_directory_files_nothing_new() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "one");
        write(dir.path(), "b.txt", "two");

        mine(&store, dir.path(), MiningConfig::default()).await;
        let second = mine(&store, dir.path(), MiningConfig::default()).await;

        assert_eq!(
            drawers(&store).await.len(),
            2,
            "a re-mine must not duplicate"
        );
        assert_eq!(result(&second)["created"], 0);
        assert_eq!(
            result(&second)["documents"],
            0,
            "the cursor is past both files, so the second run finds nothing to read"
        );
    }

    #[tokio::test]
    async fn a_full_re_mine_reads_everything_again_and_still_files_nothing_new() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "one");
        write(dir.path(), "b.txt", "two");
        mine(&store, dir.path(), MiningConfig::default()).await;

        let mut job = directory_job(dir.path(), "test");
        job.kind = JobKind::Mine {
            source: MiningSource::Directory {
                path: dir.path().to_path_buf(),
            },
            wing: Some("docs".into()),
            full: true,
        };
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        assert_eq!(
            result(&job)["documents"],
            2,
            "full starts from the beginning"
        );
        assert_eq!(
            result(&job)["unchanged"],
            2,
            "but unchanged documents are recognised and skipped"
        );
        assert_eq!(drawers(&store).await.len(), 2);
    }

    #[tokio::test]
    async fn an_edited_file_supersedes_its_old_drawer_and_keeps_the_history() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "first version");
        touch(dir.path(), "a.txt", 1_000);
        mine(&store, dir.path(), MiningConfig::default()).await;

        write(dir.path(), "a.txt", "second version");
        touch(dir.path(), "a.txt", 2_000);
        let job = mine(&store, dir.path(), MiningConfig::default()).await;

        assert_eq!(result(&job)["superseded"], 1);
        let all = drawers(&store).await;
        assert_eq!(all.len(), 2, "history is kept, not rewritten");
        let current = open(&store).await;
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].content, "second version");
        assert_eq!(
            current[0].name.as_deref(),
            Some("a.txt"),
            "the name moves to the drawer that replaces it"
        );
    }

    #[tokio::test]
    async fn a_file_larger_than_a_chunk_becomes_several_drawers() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        let content = "a line of text\n".repeat(100);
        write(dir.path(), "big.txt", &content);
        let mining = MiningConfig {
            chunk_chars: 400,
            ..MiningConfig::default()
        };

        let job = mine(&store, dir.path(), mining).await;

        let mut chunks = drawers(&store).await;
        assert!(chunks.len() > 1, "1500 characters at 400 per chunk");
        assert_eq!(result(&job)["created"], chunks.len());
        chunks.sort_by_key(|drawer| drawer.source.origin.as_ref().unwrap().chunk);
        let joined: String = chunks.iter().map(|d| d.content.as_str()).collect();
        assert_eq!(joined, content, "chunking must not lose or reorder text");
        assert_eq!(
            chunks.iter().filter(|d| d.name.is_some()).count(),
            1,
            "only the first chunk carries the document's name"
        );
    }

    #[tokio::test]
    async fn editing_the_end_of_a_large_file_supersedes_only_the_chunks_that_changed() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        let base = "a line of text\n".repeat(100);
        write(dir.path(), "big.txt", &base);
        touch(dir.path(), "big.txt", 1_000);
        let mining = || MiningConfig {
            chunk_chars: 400,
            ..MiningConfig::default()
        };
        mine(&store, dir.path(), mining()).await;
        let chunk_count = drawers(&store).await.len();

        write(dir.path(), "big.txt", &format!("{base}and one more line\n"));
        touch(dir.path(), "big.txt", 2_000);
        let job = mine(&store, dir.path(), mining()).await;

        assert_eq!(
            result(&job)["superseded"],
            1,
            "an append rewrites the last chunk and leaves every earlier one alone"
        );
        assert_eq!(drawers(&store).await.len(), chunk_count + 1);
    }

    #[tokio::test]
    async fn a_file_that_shrinks_closes_the_chunks_past_its_new_end() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.txt", &"a line of text\n".repeat(100));
        touch(dir.path(), "big.txt", 1_000);
        let mining = || MiningConfig {
            chunk_chars: 400,
            ..MiningConfig::default()
        };
        mine(&store, dir.path(), mining()).await;
        assert!(open(&store).await.len() > 1);

        write(dir.path(), "big.txt", "short now\n");
        touch(dir.path(), "big.txt", 2_000);
        let job = mine(&store, dir.path(), mining()).await;

        assert!(result(&job)["retired"].as_u64().unwrap() >= 1);
        let current = open(&store).await;
        assert_eq!(
            current.len(),
            1,
            "stale text must not stay searchable as if it were still in the file"
        );
        assert_eq!(current[0].content, "short now\n");
    }

    #[tokio::test]
    async fn a_file_reverted_to_an_earlier_text_opens_a_new_drawer_instead_of_colliding_with_the_closed_one()
     {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for (content, at) in [("alpha", 1_000), ("beta", 2_000), ("alpha", 3_000)] {
            write(dir.path(), "a.txt", content);
            touch(dir.path(), "a.txt", at);
            mine(&store, dir.path(), MiningConfig::default()).await;
        }
        let current = open(&store).await;
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].content, "alpha");
        assert_eq!(drawers(&store).await.len(), 3);
    }

    #[tokio::test]
    async fn a_second_job_continues_where_the_first_stopped_at_its_document_limit() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            write(dir.path(), &format!("{name}.txt"), &format!("file {name}"));
            touch(dir.path(), &format!("{name}.txt"), 1_000 + i as u64);
        }
        let limited = || MiningConfig {
            max_documents: 2,
            ..MiningConfig::default()
        };

        let first = mine(&store, dir.path(), limited()).await;
        assert_eq!(result(&first)["truncated"], true);
        assert_eq!(result(&first)["documents"], 2);
        assert!(
            first
                .progress
                .message
                .as_deref()
                .unwrap()
                .contains("run again"),
            "`mined 2/2` alone would look complete"
        );

        let second = mine(&store, dir.path(), limited()).await;
        assert_eq!(
            result(&second)["documents"],
            2,
            "the next two, not the first two again"
        );
        assert_eq!(result(&second)["truncated"], true);
        let third = mine(&store, dir.path(), limited()).await;
        assert_eq!(result(&third)["documents"], 1);
        assert_eq!(result(&third)["truncated"], false);

        assert_eq!(
            drawers(&store).await.len(),
            5,
            "five files, filed once each across three jobs"
        );
    }

    #[tokio::test]
    async fn a_source_at_exactly_the_document_limit_is_not_reported_as_truncated() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            write(dir.path(), &format!("{name}.txt"), name);
        }
        let job = mine(
            &store,
            dir.path(),
            MiningConfig {
                max_documents: 3,
                ..MiningConfig::default()
            },
        )
        .await;
        assert_eq!(result(&job)["truncated"], false);
    }

    #[tokio::test]
    async fn replaying_a_job_whose_checkpoint_was_never_saved_files_no_duplicates() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            write(dir.path(), name, &format!("contents of {name}"));
        }
        let mut job = directory_job(dir.path(), "test");
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        // Crash: every drawer was written, but the job's saved checkpoint (and the source's cursor) predate them,
        // so the resumed attempt walks the same files again.
        job.checkpoint = json!({});
        let source = store.list_sources().await.unwrap().remove(0);
        store
            .save_source_cursor(
                source.id,
                &crate::domain::Cursor::Null,
                job.id,
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        assert_eq!(
            drawers(&store).await.len(),
            3,
            "each file must be filed exactly once across the replay"
        );
    }

    #[tokio::test]
    async fn a_replay_after_the_drawers_were_written_but_not_the_document_record_adds_nothing() {
        // The window between "drawer written" and "document recorded": the replay must find its own drawer.
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "contents");
        let mut job = directory_job(dir.path(), "test");
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        let source = store.list_sources().await.unwrap().remove(0);
        store.delete_source_documents_for_tests(source.id).await;
        job.checkpoint = json!({});
        store
            .save_source_cursor(
                source.id,
                &crate::domain::Cursor::Null,
                job.id,
                chrono::Utc::now(),
            )
            .await
            .unwrap();
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        assert_eq!(drawers(&store).await.len(), 1);
    }

    #[tokio::test]
    async fn a_checkpoint_in_the_pre_source_format_restarts_without_duplicating_what_was_filed() {
        // Jobs already on disk carry `{"next_index": N}` and no cursor: they must still run to completion.
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            write(dir.path(), name, &format!("contents of {name}"));
        }
        let mut job = directory_job(dir.path(), "test");
        job.checkpoint = json!({ "next_index": 2 });
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();
        assert_eq!(
            drawers(&store).await.len(),
            3,
            "the legacy checkpoint is ignored and the source is mined whole"
        );
    }

    #[tokio::test]
    async fn a_run_that_checkpointed_resumes_from_its_own_cursor_not_the_sources() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            write(dir.path(), &format!("{name}.txt"), name);
            touch(dir.path(), &format!("{name}.txt"), 1_000 + i as u64);
        }
        let mut job = directory_job(dir.path(), "test");
        // A previous attempt finished `a` and `b`.
        job.checkpoint = json!({
            "cursor": {"mtime_ns": 1_001_000_000_000i64, "key": "b.txt"},
            "stats": {"documents": 2, "created": 2, "superseded": 0, "retired": 0, "unchanged": 0, "skipped": 0}
        });
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        let current = open(&store).await;
        assert_eq!(current.len(), 1, "only `c` was left to do");
        assert_eq!(current[0].content, "c");
        assert_eq!(result(&job)["documents"], 3, "counts carry over a resume");
    }

    #[tokio::test]
    async fn a_pause_request_stops_before_filing_anything_and_a_later_attempt_finishes() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "one");
        let mut job = directory_job(dir.path(), "test");
        let control = JobControl::default();
        control.request_pause();
        let ctx = JobContext::new(job.id, control, store.clone());
        let params = params_of(&job);
        let outcome = run(&ctx, &mut job, params).await.unwrap();
        assert_eq!(outcome, JobOutcome::Paused);
        assert!(drawers(&store).await.is_empty());

        assert_eq!(
            run_job(&store, &mut job, MiningConfig::default())
                .await
                .unwrap(),
            JobOutcome::Completed
        );
        assert_eq!(drawers(&store).await.len(), 1);
    }

    #[tokio::test]
    async fn a_cancel_request_stops_the_run() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "one");
        let mut job = directory_job(dir.path(), "test");
        let control = JobControl::default();
        control.request_cancel();
        let ctx = JobContext::new(job.id, control, store.clone());
        let params = params_of(&job);
        let outcome = run(&ctx, &mut job, params).await.unwrap();
        assert_eq!(outcome, JobOutcome::Cancelled);
        assert!(drawers(&store).await.is_empty());
    }

    #[tokio::test]
    async fn a_stored_cursor_the_adapter_cannot_continue_from_fails_the_job_with_a_fix() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "one");
        mine(&store, dir.path(), MiningConfig::default()).await;
        let source = store.list_sources().await.unwrap().remove(0);
        store
            .save_source_cursor(
                source.id,
                &json!({"offset": 7}),
                crate::domain::JobId::new(),
                chrono::Utc::now(),
            )
            .await
            .unwrap();

        let mut job = directory_job(dir.path(), "test");
        let error = run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::SourceCursorInvalid { .. }),
            "{error}"
        );

        let mut full = directory_job(dir.path(), "test");
        full.kind = JobKind::Mine {
            source: MiningSource::Directory {
                path: dir.path().to_path_buf(),
            },
            wing: None,
            full: true,
        };
        assert_eq!(
            run_job(&store, &mut full, MiningConfig::default())
                .await
                .unwrap(),
            JobOutcome::Completed,
            "`--full` is the documented way out"
        );
    }

    #[tokio::test]
    async fn an_unknown_provider_is_refused_naming_the_known_ones() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let mut job = Job::new(
            JobKind::Mine {
                source: MiningSource::Provider {
                    provider: "carrier-pigeon".into(),
                    locator: None,
                },
                wing: None,
                full: false,
            },
            Priority::Background,
            "test",
        );
        job.apply(JobEvent::Claim).unwrap();
        let error = run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("carrier-pigeon") && message.contains("pi-sessions"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn mining_a_missing_directory_fails_instead_of_completing_empty() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        let mut job = directory_job(&dir.path().join("nope"), "test");
        assert!(
            run_job(&store, &mut job, MiningConfig::default())
                .await
                .is_err()
        );
    }

    fn pi_job(root: &Path, full: bool) -> Job {
        let mut job = Job::new(
            JobKind::Mine {
                source: MiningSource::Provider {
                    provider: "pi-sessions".into(),
                    locator: Some(root.display().to_string()),
                },
                wing: None,
                full,
            },
            Priority::Background,
            "test",
        );
        job.apply(JobEvent::Claim).unwrap();
        job
    }

    const PI_FIXTURE: &str = include_str!("../../tests/fixtures/sources/pi/session.jsonl");

    #[tokio::test]
    async fn pi_sessions_are_discovered_read_and_mined_with_no_agent_involved() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl",
            PI_FIXTURE,
        );

        let mut job = pi_job(root.path(), false);
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        assert_eq!(result(&job)["provider"], "pi-sessions");
        let filed = drawers(&store).await;
        assert_eq!(filed.len(), 1);
        let drawer = &filed[0];
        assert_eq!(drawer.source.kind, SourceKind::Transcript);
        assert_eq!(drawer.source.agent, None);
        assert!(drawer.content.contains("How do I rotate the signing keys?"));
        assert!(!drawer.content.contains("SECRET-TOOL-OUTPUT"));
        let wing = store
            .get_wing("pi")
            .await
            .unwrap()
            .expect("default wing is `pi`");
        let room = store
            .get_room(wing.id, "project")
            .await
            .unwrap()
            .expect("room is the working directory");
        assert!(
            store
                .get_drawer_by_name(room.id, "2026-07-14T14-27-12-546Z_019f6106")
                .await
                .unwrap()
                .is_some()
        );
        let documents = store.list_sources().await.unwrap();
        assert_eq!(
            store.count_source_documents(documents[0].id).await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn the_raw_session_is_kept_next_to_the_drawers_cut_from_it() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "p/s.jsonl", PI_FIXTURE);
        let mut job = pi_job(root.path(), false);
        run_job(&store, &mut job, MiningConfig::default())
            .await
            .unwrap();

        let source = store.list_sources().await.unwrap().remove(0);
        let document = store
            .get_source_document(source.id, "p/s.jsonl")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(document.raw.as_deref(), Some(PI_FIXTURE));
        assert_eq!(document.chunks.len(), 1);
    }

    #[tokio::test]
    async fn a_session_that_grew_adds_only_what_is_new_on_the_next_run() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let root = tempfile::tempdir().unwrap();
        // Enough messages to span several chunks, so the untouched ones are visibly untouched.
        let mut body = String::from(r#"{"type":"session","version":3,"id":"s","cwd":"/work/app"}"#);
        body.push('\n');
        for i in 0..30 {
            body.push_str(&format!(
                r#"{{"type":"message","timestamp":"2026-07-14T10:00:{i:02}Z","message":{{"role":"user","content":"message number {i} with some padding text to take up room"}}}}"#
            ));
            body.push('\n');
        }
        write(root.path(), "p/s.jsonl", &body);
        touch(root.path(), "p/s.jsonl", 1_000);
        let mining = || MiningConfig {
            chunk_chars: 500,
            ..MiningConfig::default()
        };

        let mut first = pi_job(root.path(), false);
        run_job(&store, &mut first, mining()).await.unwrap();
        let chunk_count = drawers(&store).await.len();
        assert!(chunk_count > 2);

        let grown = format!(
            "{body}{}\n",
            r#"{"type":"message","timestamp":"2026-07-14T11:00:00Z","message":{"role":"user","content":"a brand new question"}}"#
        );
        write(root.path(), "p/s.jsonl", &grown);
        touch(root.path(), "p/s.jsonl", 2_000);
        let mut second = pi_job(root.path(), false);
        run_job(&store, &mut second, mining()).await.unwrap();

        assert_eq!(result(&second)["documents"], 1);
        assert!(
            result(&second)["superseded"].as_u64().unwrap()
                + result(&second)["created"].as_u64().unwrap()
                <= 2,
            "only the tail changed: {}",
            result(&second)
        );
        assert!(
            open(&store)
                .await
                .iter()
                .any(|d| d.content.contains("a brand new question"))
        );

        let mut third = pi_job(root.path(), false);
        run_job(&store, &mut third, mining()).await.unwrap();
        assert_eq!(
            result(&third)["documents"],
            0,
            "nothing changed since the second run"
        );
    }

    #[tokio::test]
    async fn two_sources_keep_independent_cursors() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        write(a.path(), "x.txt", "in a");
        write(b.path(), "x.txt", "in b");
        mine(&store, a.path(), MiningConfig::default()).await;
        let second = mine(&store, b.path(), MiningConfig::default()).await;
        assert_eq!(
            result(&second)["created"],
            1,
            "b's first run must not be hidden by a's cursor"
        );
        assert_eq!(store.list_sources().await.unwrap().len(), 2);
    }

    #[test]
    fn every_shipped_provider_is_listed_with_its_capabilities() {
        let names: Vec<_> = providers().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["directory", "pi-sessions"]);
        assert!(is_provider("pi-sessions") && !is_provider("nope"));
        let pi = providers()
            .into_iter()
            .find(|p| p.name == "pi-sessions")
            .unwrap();
        assert!(
            pi.capabilities.incremental
                && pi.capabilities.retains_raw
                && !pi.capabilities.needs_credentials
        );
    }
}
