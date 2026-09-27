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
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::domain::{
    CheckpointDestination, CheckpointPayload, Drawer, DrawerId, FactMutation, Job, NewRelationship,
    Provenance, RoomId,
};
use crate::error::Result;
use crate::jobs::{JobContext, JobOutcome};
use crate::store::SurrealStore;

/// Process `payload`'s items in order, checking in with `ctx` between each
/// so the job can be paused, resumed, or cancelled.
///
/// # Errors
///
/// Returns an error if a store write fails, or if a `fact` mutation is
/// rejected (e.g. an empty predicate — see `store::entities::EmptyLabel`).
pub async fn run(
    store: &SurrealStore,
    ctx: &JobContext,
    job: &mut Job,
    payload: &CheckpointPayload,
) -> Result<JobOutcome> {
    let start = job
        .checkpoint
        .get("next_index")
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| n as usize);

    for (index, item) in payload.items.iter().enumerate().skip(start) {
        if ctx.is_cancelled() {
            return Ok(JobOutcome::Cancelled);
        }
        if ctx.should_pause() {
            checkpoint_at(ctx, job, index, payload.items.len()).await?;
            return Ok(JobOutcome::Paused);
        }

        let room = resolve_room(store, item.destination, item.wing.as_deref()).await?;

        let now = Utc::now();
        let mut hasher = Sha256::new();
        hasher.update(item.content.as_bytes());
        let content_hash = hex_encode(&hasher.finalize());

        let drawer = Drawer {
            id: DrawerId::new(),
            room,
            content: item.content.clone(),
            content_hash,
            source: item.source.clone(),
            tags: item.tags.clone(),
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

        if let Some(fact) = &item.fact {
            apply_fact_mutation(store, fact).await?;
        }

        checkpoint_at(ctx, job, index + 1, payload.items.len()).await?;
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
async fn apply_fact_mutation(store: &SurrealStore, fact: &FactMutation) -> Result<()> {
    match fact {
        FactMutation::Add {
            subject,
            predicate,
            object,
            confidence,
        } => {
            store
                .create_relationship(*subject, *object, predicate, *confidence)
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
                    NewRelationship {
                        from: *from,
                        to: *to,
                        predicate: predicate.clone(),
                        confidence: *confidence,
                    },
                )
                .await?;
        }
        FactMutation::Invalidate { relationship_id } => {
            store.invalidate_relationship(*relationship_id).await?;
        }
    }
    Ok(())
}

async fn checkpoint_at(ctx: &JobContext, job: &mut Job, index: usize, total: usize) -> Result<()> {
    let progress = crate::domain::JobProgress {
        current: index as u32,
        total: Some(total as u32),
        message: Some(format!("checkpointed {index}/{total} items")),
    };
    ctx.checkpoint(job, progress, json!({ "next_index": index }))
        .await
}

/// Lowercase hex — see `mining`'s identical helper for why this is a few
/// lines of its own rather than a shared dependency.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CheckpointItem, JobKind, Priority, Source, SourceKind};
    use crate::jobs::JobControl;

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

        let outcome = run(&store, &ctx, &mut job, &payload).await.expect("run");
        assert_eq!(outcome, JobOutcome::Completed);
        assert_eq!(job.progress.current, 3);
        assert_eq!(job.checkpoint, json!({ "next_index": 3 }));

        let general = store.get_or_create_wing("general", None).await.unwrap();
        let general_room = store
            .get_or_create_room(general.id, "entries", None)
            .await
            .unwrap();
        assert_eq!(store.list_drawers(general_room.id).await.unwrap().len(), 1);

        let preferences = store.get_or_create_wing("preferences", None).await.unwrap();
        let preferences_room = store
            .get_or_create_room(preferences.id, "entries", None)
            .await
            .unwrap();
        assert_eq!(
            store.list_drawers(preferences_room.id).await.unwrap().len(),
            1
        );

        let diary = store.get_or_create_wing("diary", None).await.unwrap();
        let diary_room = store
            .get_or_create_room(diary.id, "diary", None)
            .await
            .unwrap();
        let diary_drawers = store.list_drawers(diary_room.id).await.unwrap();
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

        run(&store, &ctx, &mut job, &payload).await.expect("run");

        let default_projects = store.get_or_create_wing("projects", None).await.unwrap();
        let default_room = store
            .get_or_create_room(default_projects.id, "entries", None)
            .await
            .unwrap();
        assert!(
            store
                .list_drawers(default_room.id)
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
            store.list_drawers(project_foo_room.id).await.unwrap().len(),
            1,
            "the item must land in the overridden wing instead"
        );
    }

    #[tokio::test]
    async fn an_add_fact_mutation_creates_a_relationship() {
        let store = memory_store().await;
        let alice = store
            .create_entity("Alice", "person", json!({}))
            .await
            .expect("alice");
        let acme = store
            .create_entity("Acme", "organization", json!({}))
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

        run(&store, &ctx, &mut job, &payload).await.expect("run");

        let relationships = store.list_relationships(alice.id, false).await.unwrap();
        assert_eq!(relationships.len(), 1);
        assert_eq!(relationships[0].predicate, "employee_of");
        assert_eq!(relationships[0].to, acme.id);
    }

    #[tokio::test]
    async fn an_invalidate_fact_mutation_closes_the_edge() {
        let store = memory_store().await;
        let alice = store
            .create_entity("Alice", "person", json!({}))
            .await
            .expect("alice");
        let acme = store
            .create_entity("Acme", "organization", json!({}))
            .await
            .expect("acme");
        let relationship = store
            .create_relationship(alice.id, acme.id, "employee_of", 0.9)
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

        run(&store, &ctx, &mut job, &payload).await.expect("run");

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

        let outcome = run(&store, &ctx, &mut job, &payload).await.expect("run");
        assert_eq!(outcome, JobOutcome::Paused);
        assert_eq!(job.checkpoint, json!({ "next_index": 0 }));

        let general = store.get_or_create_wing("general", None).await.unwrap();
        let room = store
            .get_or_create_room(general.id, "entries", None)
            .await
            .unwrap();
        assert!(
            store.list_drawers(room.id).await.unwrap().is_empty(),
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
        let outcome = run(&store, &ctx, &mut job, &first_attempt)
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
        let outcome = run(&store, &ctx, &mut job, &payload)
            .await
            .expect("resumed attempt");
        assert_eq!(outcome, JobOutcome::Completed);

        let general = store.get_or_create_wing("general", None).await.unwrap();
        let room = store
            .get_or_create_room(general.id, "entries", None)
            .await
            .unwrap();
        let drawers = store.list_drawers(room.id).await.unwrap();
        assert_eq!(
            drawers.len(),
            payload.items.len(),
            "every item must land exactly once across the simulated restart, got {drawers:?}"
        );
    }
}
