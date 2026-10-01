//! The checkpoint job handler: turns an already-classified
//! [`CheckpointPayload`] into durable drawers (and, where an item carries
//! one, a knowledge-graph mutation).
//!
//! Structurally this mirrors `crate::mining::run` almost exactly — per-item
//! cooperative pause/cancel, checkpointing `{"next_index": i}` after each —
//! because the same "job submission -> scheduler -> checkpointed execution
//! -> durable writes" pipeline applies here unchanged. What's different is
//! the unit of work: not a file on disk, but an item a caller has already
//! decided is worth keeping (MemCastle does not classify content itself —
//! see `domain::checkpoint`'s module doc).
//!
//! Every item gets a drawer, written first — always, not conditionally —
//! and *additionally* applies its `fact` mutation if one is present. This
//! keeps every checkpoint item auditable as a drawer even when it also
//! changes the graph, rather than treating "write a drawer" and "mutate a
//! fact" as mutually exclusive outcomes.
//!
//! Deliberately has **no artificial per-item delay** (unlike
//! `jobs::demo`'s `STEP_DELAY`): a checkpoint job is the latency-sensitive
//! one — an emergency checkpoint exists specifically to save state before a
//! crash, so adding synthetic latency here would work against the feature's
//! own purpose. Resumability is instead proven deterministically in this
//! module's own tests, by calling [`run`] twice against a forced pause
//! rather than racing wall-clock time against a live scheduler.

use chrono::Utc;

use crate::domain::{
    CheckpointDestination, CheckpointPayload, Drawer, DrawerId, FactMutation, Job, NewRelationship,
    Provenance, RelationshipId, RoomId,
};
use crate::error::Result;
use crate::jobs::{JobContext, JobOutcome};
use crate::store::SurrealStore;

/// What a `Checkpoint` job needs, gathered from its [`crate::domain::JobKind`].
pub struct CheckpointParams {
    /// The items to write.
    pub payload: CheckpointPayload,
}

/// Process `payload`'s items in order, checking in with `ctx` between each
/// so the job can be paused, resumed, or cancelled.
///
/// # Errors
///
/// Returns an error if a store write fails, or if a `fact` mutation is
/// rejected (e.g. an empty predicate — see `crate::Error::EmptyLabel`).
pub async fn run(ctx: &JobContext, job: &mut Job, params: CheckpointParams) -> Result<JobOutcome> {
    let CheckpointParams { payload } = params;
    let store = ctx.store();
    let start = JobContext::resume_index(job, "next_index");
    tracing::info!(
        items = payload.items.len(),
        resume_from = start,
        "writing checkpoint items"
    );

    for (index, item) in payload.items.iter().enumerate().skip(start) {
        if ctx.is_cancelled() {
            return Ok(JobOutcome::Cancelled);
        }
        if ctx.should_pause() {
            ctx.checkpoint_at(job, index, payload.items.len(), "checkpointed", "items")
                .await?;
            return Ok(JobOutcome::Paused);
        }

        let room = resolve_room(store, item.destination, item.wing.as_deref()).await?;

        let drawer = Drawer::new(
            // Derived, not random: a crash between the write below and the
            // checkpoint at the loop's end makes the resumed attempt redo
            // this same item, and a fresh id would store it twice.
            DrawerId::derive(job.id.0, &format!("checkpoint-drawer:{index}")),
            room,
            item.content.clone(),
            item.source.clone(),
            item.tags.clone(),
            Provenance {
                requested_by: job.requested_by.clone(),
                job_id: Some(job.id),
            },
        );
        store.create_drawer_once(&drawer).await?;

        if let Some(fact) = &item.fact {
            apply_fact_mutation(store, job, index, fact).await?;
        }

        ctx.checkpoint_at(job, index + 1, payload.items.len(), "checkpointed", "items")
            .await?;
    }

    Ok(JobOutcome::Completed)
}

/// Resolve which room a checkpoint item files under: `wing` (the item's own
/// override, if any) or else `destination`'s fixed default wing, always
/// paired with `destination`'s fixed room name — see
/// `CheckpointDestination::{default_wing, room_name}`'s doc comments for why
/// only the wing is overridable in this issue's payload shape.
///
/// Resolved per item, not cached: at the scale a checkpoint job's payload
/// operates at (a handful of items, not thousands like mining's files),
/// the extra idempotent `get_or_create_*` round trips are not worth the
/// bookkeeping a cache would add.
async fn resolve_room(
    store: &SurrealStore,
    destination: CheckpointDestination,
    wing: Option<&str>,
) -> Result<RoomId> {
    let wing_name = wing.unwrap_or_else(|| destination.default_wing());
    let wing = store.get_or_create_wing(wing_name, None).await?;
    let room = store
        .get_or_create_room(wing.id, destination.room_name(), None)
        .await?;
    Ok(room.id)
}

/// Apply one item's knowledge-graph mutation, dispatching to the
/// corresponding `store::entities` operation.
///
/// Replay-safe, like the drawer write before it: a new edge gets an id
/// derived from (job, item index) and is skipped if it already exists, and
/// invalidating an edge that is already closed changes nothing. Without
/// that, resuming after a crash between this call and the checkpoint would
/// open a second current edge for the same fact.
async fn apply_fact_mutation(
    store: &SurrealStore,
    job: &Job,
    index: usize,
    fact: &FactMutation,
) -> Result<()> {
    let edge_id = RelationshipId::derive(job.id.0, &format!("checkpoint-edge:{index}"));
    match fact {
        FactMutation::Add {
            subject,
            predicate,
            object,
            confidence,
        } => {
            store
                .create_relationship(
                    edge_id,
                    NewRelationship {
                        from: *subject,
                        to: *object,
                        predicate: predicate.clone(),
                        confidence: *confidence,
                    },
                    Utc::now(),
                )
                .await?;
        }
        FactMutation::Supersede {
            relationship_id,
            from,
            to,
            predicate,
            confidence,
        } => {
            store
                .supersede_relationship(
                    *relationship_id,
                    edge_id,
                    NewRelationship {
                        from: *from,
                        to: *to,
                        predicate: predicate.clone(),
                        confidence: *confidence,
                    },
                    Utc::now(),
                )
                .await?;
        }
        FactMutation::Invalidate { relationship_id } => {
            store
                .invalidate_relationship(*relationship_id, Utc::now())
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CheckpointItem, JobKind, Priority, Source, SourceKind};
    use crate::jobs::JobControl;
    use serde_json::json;

    async fn memory_store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    fn ctx_for(store: &SurrealStore, job: &Job, control: JobControl) -> JobContext {
        JobContext::new(job.id, control, store.clone())
    }

    fn item(destination: CheckpointDestination, content: &str) -> CheckpointItem {
        CheckpointItem {
            destination,
            wing: None,
            content: content.to_string(),
            tags: vec![],
            source: Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: Some("test-agent".to_string()),
            },
            fact: None,
        }
    }

    #[tokio::test]
    async fn checkpoint_run_creates_one_drawer_per_item_and_advances_progress() {
        let store = memory_store().await;
        let payload = CheckpointPayload {
            items: vec![
                item(CheckpointDestination::General, "first note"),
                item(CheckpointDestination::Preference, "a lasting preference"),
                item(CheckpointDestination::Diary, "what happened today"),
            ],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        let ctx = ctx_for(&store, &job, JobControl::default());

        let outcome = run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("run");
        assert_eq!(outcome, JobOutcome::Completed);
        assert_eq!(job.progress.current, 3);
        assert_eq!(job.checkpoint, json!({ "next_index": 3 }));

        let general = store.get_or_create_wing("general", None).await.unwrap();
        let general_room = store
            .get_or_create_room(general.id, "entries", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .list_drawers(Some(general_room.id))
                .await
                .unwrap()
                .len(),
            1
        );

        let preferences = store.get_or_create_wing("preferences", None).await.unwrap();
        let preferences_room = store
            .get_or_create_room(preferences.id, "entries", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .list_drawers(Some(preferences_room.id))
                .await
                .unwrap()
                .len(),
            1
        );

        let diary = store.get_or_create_wing("diary", None).await.unwrap();
        let diary_room = store
            .get_or_create_room(diary.id, "diary", None)
            .await
            .unwrap();
        let diary_drawers = store.list_drawers(Some(diary_room.id)).await.unwrap();
        assert_eq!(diary_drawers.len(), 1);
        assert_eq!(diary_drawers[0].source.agent.as_deref(), Some("test-agent"));
    }

    #[tokio::test]
    async fn an_items_wing_override_files_it_under_that_wing_instead_of_the_default() {
        let store = memory_store().await;
        let mut overridden = item(CheckpointDestination::Project, "scoped to one project");
        overridden.wing = Some("project-foo".to_string());
        let payload = CheckpointPayload {
            items: vec![overridden],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        let ctx = ctx_for(&store, &job, JobControl::default());

        run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("run");

        let default_projects = store.get_or_create_wing("projects", None).await.unwrap();
        let default_room = store
            .get_or_create_room(default_projects.id, "entries", None)
            .await
            .unwrap();
        assert!(
            store
                .list_drawers(Some(default_room.id))
                .await
                .unwrap()
                .is_empty(),
            "the item must not land in the destination's default wing once overridden"
        );

        let project_foo = store.get_or_create_wing("project-foo", None).await.unwrap();
        let project_foo_room = store
            .get_or_create_room(project_foo.id, "entries", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .list_drawers(Some(project_foo_room.id))
                .await
                .unwrap()
                .len(),
            1,
            "the item must land in the overridden wing instead"
        );
    }

    #[tokio::test]
    async fn an_add_fact_mutation_creates_a_relationship() {
        let store = memory_store().await;
        let alice = store
            .get_or_create_entity("Alice", "person", json!({}))
            .await
            .expect("alice");
        let acme = store
            .get_or_create_entity("Acme", "organization", json!({}))
            .await
            .expect("acme");

        let mut with_fact = item(CheckpointDestination::General, "Alice joined Acme");
        with_fact.fact = Some(FactMutation::Add {
            subject: alice.id,
            predicate: "employee_of".to_string(),
            object: acme.id,
            confidence: 0.9,
        });
        let payload = CheckpointPayload {
            items: vec![with_fact],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        let ctx = ctx_for(&store, &job, JobControl::default());

        run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("run");

        let relationships = store.list_relationships(alice.id, false).await.unwrap();
        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].predicate, "employee_of");
        assert_eq!(relationships[0].to, acme.id);
    }

    #[tokio::test]
    async fn an_invalidate_fact_mutation_closes_the_edge() {
        let store = memory_store().await;
        let alice = store
            .get_or_create_entity("Alice", "person", json!({}))
            .await
            .expect("alice");
        let acme = store
            .get_or_create_entity("Acme", "organization", json!({}))
            .await
            .expect("acme");
        let relationship = store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from: alice.id,
                    to: acme.id,
                    predicate: "employee_of".to_string(),
                    confidence: 0.9,
                },
                Utc::now(),
            )
            .await
            .expect("create relationship");

        let mut with_fact = item(CheckpointDestination::General, "Alice left Acme");
        with_fact.fact = Some(FactMutation::Invalidate {
            relationship_id: relationship.id,
        });
        let payload = CheckpointPayload {
            items: vec![with_fact],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        let ctx = ctx_for(&store, &job, JobControl::default());

        run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("run");

        let current = store.list_relationships(alice.id, false).await.unwrap();
        assert!(
            current.is_empty(),
            "an invalidated relationship must not read back as current"
        );
    }

    #[tokio::test]
    async fn requesting_pause_before_any_work_stops_immediately_and_checkpoints_at_the_start_index()
    {
        let store = memory_store().await;
        let payload = CheckpointPayload {
            items: (0..3)
                .map(|i| item(CheckpointDestination::General, &format!("item {i}")))
                .collect(),
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();

        // Pause requested before `run` is even called: `should_pause()` is
        // checked at the *top* of each loop iteration (before that
        // iteration's work), so this must stop before touching item 0 at
        // all — the same code path a genuinely in-flight pause request
        // hits, just guaranteed (rather than raced) to land on the very
        // first check.
        let control = JobControl::default();
        control.request_pause();
        let ctx = ctx_for(&store, &job, control);

        let outcome = run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("run");
        assert_eq!(outcome, JobOutcome::Paused);
        assert_eq!(job.checkpoint, json!({ "next_index": 0 }));

        let general = store.get_or_create_wing("general", None).await.unwrap();
        let room = store
            .get_or_create_room(general.id, "entries", None)
            .await
            .unwrap();
        assert!(
            store.list_drawers(Some(room.id)).await.unwrap().is_empty(),
            "no item should have been processed before the pause check fired"
        );
    }

    #[tokio::test]
    async fn resuming_after_a_simulated_restart_lands_every_item_exactly_once() {
        let store = memory_store().await;
        let payload = CheckpointPayload {
            items: (0..5)
                .map(|i| item(CheckpointDestination::General, &format!("item {i}")))
                .collect(),
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();

        // Simulate the daemon crashing (or being asked to stop) after the
        // first 2 of 5 items were durably written — this is exactly what
        // an interrupted attempt leaves behind, since every item's drawer
        // write is followed immediately by a checkpoint (see `run`'s
        // loop): a real prior run against a truncated view of the payload,
        // not a pause flag.
        let first_attempt = CheckpointPayload {
            items: payload.items[..2].to_vec(),
        };
        let ctx = ctx_for(&store, &job, JobControl::default());
        let outcome = run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: first_attempt.clone(),
            },
        )
        .await
        .expect("first attempt");
        assert_eq!(outcome, JobOutcome::Completed);
        assert_eq!(
            job.checkpoint,
            json!({ "next_index": 2 }),
            "the checkpoint left behind must record exactly how far the interrupted attempt got"
        );

        // "Restart": a fresh `JobContext`, the *full* payload this time —
        // `start` reads the same `next_index: 2` a real restart would read
        // back from the persisted job, so items 0-1 are not redone.
        let ctx = ctx_for(&store, &job, JobControl::default());
        let outcome = run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("resumed attempt");
        assert_eq!(outcome, JobOutcome::Completed);

        let general = store.get_or_create_wing("general", None).await.unwrap();
        let room = store
            .get_or_create_room(general.id, "entries", None)
            .await
            .unwrap();
        let drawers = store.list_drawers(Some(room.id)).await.unwrap();
        assert_eq!(
            drawers.len(),
            payload.items.len(),
            "every item must land exactly once across the simulated restart, got {drawers:?}"
        );
    }

    /// The crash window the checkpoint exists to bound: every write of an
    /// item landed, but the process died before the job's `next_index` was
    /// saved, so the resumed attempt replays the whole item.
    #[tokio::test]
    async fn replaying_items_whose_checkpoint_was_never_saved_writes_no_duplicates() {
        let store = memory_store().await;
        let subject = store
            .get_or_create_entity("Alice", "person", json!({}))
            .await
            .unwrap();
        let object = store
            .get_or_create_entity("Rust", "language", json!({}))
            .await
            .unwrap();
        let mut with_fact = item(CheckpointDestination::General, "alice likes rust");
        with_fact.fact = Some(FactMutation::Add {
            subject: subject.id,
            predicate: "likes".to_string(),
            object: object.id,
            confidence: 1.0,
        });
        let payload = CheckpointPayload {
            items: vec![with_fact, item(CheckpointDestination::General, "second")],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();

        let ctx = ctx_for(&store, &job, JobControl::default());
        run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("first attempt");

        // Crash: everything was written, but the saved checkpoint is from
        // before any of it.
        job.checkpoint = json!({});
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(
            &ctx,
            &mut job,
            CheckpointParams {
                payload: payload.clone(),
            },
        )
        .await
        .expect("replayed attempt");

        assert_eq!(
            store.list_drawers(None).await.unwrap().len(),
            payload.items.len(),
            "a replayed item must not be stored twice"
        );
        assert_eq!(
            store
                .list_relationships(subject.id, false)
                .await
                .unwrap()
                .len(),
            1,
            "a replayed fact must not open a second current edge"
        );
    }

    #[tokio::test]
    async fn replaying_a_supersede_does_not_open_a_second_replacement_edge() {
        let store = memory_store().await;
        let a = store
            .get_or_create_entity("A", "thing", json!({}))
            .await
            .unwrap();
        let b = store
            .get_or_create_entity("B", "thing", json!({}))
            .await
            .unwrap();
        let old = store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from: a.id,
                    to: b.id,
                    predicate: "knows".to_string(),
                    confidence: 0.5,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let mut superseding = item(CheckpointDestination::General, "now they are friends");
        superseding.fact = Some(FactMutation::Supersede {
            relationship_id: old.id,
            from: a.id,
            to: b.id,
            predicate: "friends".to_string(),
            confidence: 1.0,
        });
        let payload = CheckpointPayload {
            items: vec![superseding],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        job.apply(crate::domain::JobEvent::Claim).unwrap();

        for _ in 0..2 {
            job.checkpoint = json!({});
            let ctx = ctx_for(&store, &job, JobControl::default());
            run(
                &ctx,
                &mut job,
                CheckpointParams {
                    payload: payload.clone(),
                },
            )
            .await
            .expect("attempt");
        }

        let current = store.list_relationships(a.id, false).await.unwrap();
        assert_eq!(current.len(), 1, "exactly one current edge: {current:?}");
        assert_eq!(current[0].predicate, "friends");
    }

    /// `{"next_index": N}` is persisted in job records already on disk, so a
    /// handler refactor must keep reading exactly that shape.
    #[tokio::test]
    async fn a_checkpoint_persisted_in_the_current_format_resumes_past_the_finished_items() {
        let store = memory_store().await;
        let payload = CheckpointPayload {
            items: vec![
                item(CheckpointDestination::General, "already written"),
                item(CheckpointDestination::General, "already written too"),
                item(CheckpointDestination::General, "still to do"),
            ],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "test",
        );
        job.checkpoint = json!({ "next_index": 2 });
        let ctx = ctx_for(&store, &job, JobControl::default());

        run(&ctx, &mut job, CheckpointParams { payload })
            .await
            .expect("run");

        let drawers = store.list_drawers(None).await.unwrap();
        assert_eq!(drawers.len(), 1, "only the unfinished item may be written");
        assert_eq!(drawers[0].content, "still to do");
    }

    #[tokio::test]
    async fn a_checkpointed_drawer_records_the_channel_as_requested_by_and_the_item_agent_as_source()
     {
        let store = memory_store().await;
        let payload = CheckpointPayload {
            items: vec![item(CheckpointDestination::General, "a note")],
        };
        let mut job = Job::new(
            JobKind::Checkpoint {
                payload: payload.clone(),
            },
            Priority::High,
            "http",
        );
        let ctx = ctx_for(&store, &job, JobControl::default());

        run(&ctx, &mut job, CheckpointParams { payload })
            .await
            .expect("run");

        let drawers = store.list_drawers(None).await.unwrap();
        assert_eq!(drawers[0].provenance.requested_by, "http");
        assert_eq!(drawers[0].source.agent.as_deref(), Some("test-agent"));
    }
}
