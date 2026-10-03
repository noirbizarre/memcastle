//! The embed job handler: give every drawer without a vector one.
//!
//! The sweep keeps no cursor of its own. Each pass asks the store for the
//! next drawers that *have no embedding*, so the database is the checkpoint:
//! a crash or a pause loses nothing, a resumed job simply asks again, and a
//! drawer that already has a vector is never touched or re-embedded
//! (ADR-008's replay safety, by construction). Only the `embedding` field is
//! written; the drawer's content, hash and timestamps are as mining or a
//! checkpoint left them.
//!
//! A provider failure fails the job rather than skipping the drawer: silently
//! leaving holes would make semantic search quietly worse, while a failed job
//! is visible in `memcastle job list`. The scheduler's retry budget and the
//! next sweep cover a provider that comes back.

use crate::domain::{Job, JobProgress};
use crate::error::{Error, Result};
use crate::jobs::{JobContext, JobOutcome};

/// How many drawers one pass reads and embeds. Bounds memory (a drawer can
/// hold 256 KiB) and how much work a stop request waits behind.
const PASS_SIZE: u32 = 32;

/// What an `Embed` job needs, gathered from its [`crate::domain::JobKind`].
pub struct EmbedParams {
    /// Restrict the sweep to one wing by name.
    pub wing: Option<String>,
}

/// Embed drawers until none are left without a vector, or the job is stopped.
///
/// # Errors
///
/// [`Error::EmbeddingsNotConfigured`] without a provider, [`Error::EmbeddingFailed`]
/// when it fails, or a store error.
pub async fn run(ctx: &JobContext, job: &mut Job, params: EmbedParams) -> Result<JobOutcome> {
    let EmbedParams { wing } = params;
    let embeddings = ctx.embeddings();
    if !embeddings.is_configured() {
        return Err(Error::EmbeddingsNotConfigured);
    }
    let store = ctx.store();
    // A resumed job continues its running count rather than restarting it.
    let mut done = u64::try_from(JobContext::resume_index(job, "embedded")).unwrap_or(u64::MAX);
    tracing::info!(already_embedded = done, "embedding sweep started");

    loop {
        if let Some(stop) = ctx.stop_requested() {
            return Ok(stop);
        }
        let drawers = store
            .list_drawers_without_embedding(wing.as_deref(), PASS_SIZE)
            .await?;
        if drawers.is_empty() {
            break;
        }
        let texts: Vec<String> = drawers.iter().map(|d| d.content.clone()).collect();
        let vectors = embeddings.embed(&texts).await?;
        let mut stored = 0u64;
        for (drawer, vector) in drawers.iter().zip(&vectors) {
            // `false` means the drawer was deleted since it was listed: nothing
            // to embed, and it no longer appears in the next pass either.
            if store.set_drawer_embedding(drawer.id, vector).await? {
                stored += 1;
            }
        }
        done += stored;
        let progress = JobProgress {
            current: u32::try_from(done).unwrap_or(u32::MAX),
            total: None,
            message: Some(format!("embedded {done} drawer(s)")),
        };
        ctx.checkpoint(job, progress, serde_json::json!({ "embedded": done }))
            .await?;
        // A pass that stored nothing (every listed drawer vanished) would
        // otherwise spin on an empty page without progress; the next listing
        // is empty anyway, so this only guards a pathological store.
        if stored == 0 && drawers.len() == usize::try_from(PASS_SIZE).unwrap_or(usize::MAX) {
            tracing::warn!("an embedding pass stored nothing; stopping to avoid a loop");
            break;
        }
    }
    tracing::info!(embedded = done, "embedding sweep finished");
    Ok(JobOutcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Drawer, DrawerId, JobKind, Priority, Provenance, RoomId, Source, SourceKind,
    };
    use crate::embed::{EmbedFuture, Embedder, Embeddings, fake};
    use crate::jobs::JobControl;
    use crate::store::SurrealStore;

    async fn seeded(contents: &[&str]) -> (SurrealStore, Vec<Drawer>) {
        let store = SurrealStore::connect_memory_for_tests().await;
        let wing = store.get_or_create_wing("w", None).await.expect("wing");
        let room: RoomId = store
            .get_or_create_room(wing.id, "r", None)
            .await
            .expect("room")
            .id;
        let mut drawers = Vec::new();
        for content in contents {
            let drawer = Drawer::new(
                DrawerId::new(),
                room,
                (*content).to_string(),
                Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: None,
                },
                vec![],
                Provenance {
                    requested_by: "test".into(),
                    job_id: None,
                },
            );
            store.create_drawer(&drawer).await.expect("create drawer");
            drawers.push(drawer);
        }
        (store, drawers)
    }

    fn embed_job() -> Job {
        Job::new(JobKind::Embed { wing: None }, Priority::Background, "test")
    }

    fn context(store: &SurrealStore, job: &Job, embeddings: Embeddings) -> JobContext {
        JobContext::new(job.id, JobControl::default(), store.clone()).with_embeddings(embeddings)
    }

    #[tokio::test]
    async fn the_sweep_embeds_every_drawer_and_leaves_canonical_content_untouched() {
        let contents: Vec<String> = (0..70)
            .map(|i| format!("drawer number {i} about moats"))
            .collect();
        let refs: Vec<&str> = contents.iter().map(String::as_str).collect();
        let (store, drawers) = seeded(&refs).await;
        let mut job = embed_job();
        let ctx = context(&store, &job, Embeddings::new(fake::WordHashEmbedder, 16));

        let outcome = run(&ctx, &mut job, EmbedParams { wing: None })
            .await
            .expect("runs");

        assert_eq!(outcome, JobOutcome::Completed);
        assert_eq!(
            job.progress.current, 70,
            "more than one pass of {PASS_SIZE}"
        );
        assert!(
            store
                .list_drawers_without_embedding(None, 10)
                .await
                .unwrap()
                .is_empty()
        );
        for original in &drawers {
            let stored = store.get_drawer(original.id).await.unwrap().unwrap();
            assert_eq!(stored.content, original.content);
            assert_eq!(stored.content_hash, original.content_hash);
            assert_eq!(stored.updated_at, original.updated_at);
            assert_eq!(
                stored.embedding.as_deref(),
                Some(fake::vector_for(&original.content).as_slice())
            );
        }
    }

    #[tokio::test]
    async fn running_the_sweep_again_finds_nothing_to_do() {
        let (store, _) = seeded(&["one", "two"]).await;
        let mut job = embed_job();
        let ctx = context(&store, &job, Embeddings::new(fake::WordHashEmbedder, 4));
        run(&ctx, &mut job, EmbedParams { wing: None })
            .await
            .unwrap();

        let mut again = embed_job();
        let ctx = context(&store, &again, Embeddings::new(fake::WordHashEmbedder, 4));
        let outcome = run(&ctx, &mut again, EmbedParams { wing: None })
            .await
            .unwrap();

        assert_eq!(outcome, JobOutcome::Completed);
        assert_eq!(again.progress.current, 0, "nothing was re-embedded");
    }

    #[tokio::test]
    async fn a_sweep_without_a_provider_fails_with_an_actionable_error() {
        let (store, _) = seeded(&["one"]).await;
        let mut job = embed_job();
        let ctx = context(&store, &job, Embeddings::disabled());

        let error = run(&ctx, &mut job, EmbedParams { wing: None })
            .await
            .expect_err("no provider");

        assert!(matches!(error, Error::EmbeddingsNotConfigured), "{error}");
    }

    #[tokio::test]
    async fn a_pause_request_stops_the_sweep_before_any_work() {
        let (store, _) = seeded(&["one", "two"]).await;
        let mut job = embed_job();
        let control = JobControl::default();
        control.request_pause();
        let ctx = JobContext::new(job.id, control, store.clone())
            .with_embeddings(Embeddings::new(fake::WordHashEmbedder, 4));

        let outcome = run(&ctx, &mut job, EmbedParams { wing: None })
            .await
            .unwrap();

        assert_eq!(outcome, JobOutcome::Paused);
        assert_eq!(
            store
                .list_drawers_without_embedding(None, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_failing_provider_fails_the_job_and_changes_no_drawer() {
        struct Broken;
        impl Embedder for Broken {
            fn embed<'a>(&'a self, _texts: &'a [String]) -> EmbedFuture<'a> {
                Box::pin(async {
                    Err(Error::EmbeddingFailed {
                        message: "quota".into(),
                    })
                })
            }
        }
        let (store, _) = seeded(&["one"]).await;
        let mut job = embed_job();
        let ctx = context(&store, &job, Embeddings::new(Broken, 4));

        let error = run(&ctx, &mut job, EmbedParams { wing: None })
            .await
            .expect_err("provider fails");

        assert!(error.to_string().contains("quota"), "{error}");
        assert_eq!(
            store
                .list_drawers_without_embedding(None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_wing_scoped_sweep_leaves_other_wings_alone() {
        let (store, _) = seeded(&["in w"]).await;
        let other = store.get_or_create_wing("other", None).await.unwrap();
        let room = store.get_or_create_room(other.id, "r", None).await.unwrap();
        let outsider = Drawer::new(
            DrawerId::new(),
            room.id,
            "elsewhere".to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
            },
            vec![],
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        );
        store.create_drawer(&outsider).await.unwrap();
        let mut job = embed_job();
        let ctx = context(&store, &job, Embeddings::new(fake::WordHashEmbedder, 4));

        run(
            &ctx,
            &mut job,
            EmbedParams {
                wing: Some("w".into()),
            },
        )
        .await
        .unwrap();

        let pending = store
            .list_drawers_without_embedding(None, 10)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, outsider.id);
    }
}
