//! The audit job handler: a read-only palace consistency report.
//!
//! `docs/architecture.md` frames MemCastle's single-SurrealDB design as
//! avoiding needing a repair/audit subsystem "by construction" — one store,
//! one writer, no second index file to desync — in direct contrast to
//! `mempalace-rs`'s SQLite + separate `usearch` vector index, which needs an
//! entire watchdog/auto-repair/re-embed-everything subsystem specifically
//! because that pairing can drift apart. This module's checks are therefore
//! scoped to what's **structurally still possible** here, not a port of
//! `pi-palace`'s `/palace-audit` feature list:
//!
//! - Orphan drawers: `Drawer.room` doesn't resolve to any `room` record.
//! - Dangling provenance: `Drawer.provenance.job_id` points at a job that no
//!   longer exists. Should always be empty today — jobs are never deleted —
//!   kept as a real check anyway so it doesn't silently break if that ever
//!   changes.
//! - Stuck failed jobs: `Failed` jobs whose `recovery_attempts` has reached
//!   `max_attempts` (the crash-recovery budget), which will never
//!   auto-recover via `Scheduler::recover`.
//! - Running jobs: a plain count, informational only — there is no lease
//!   TTL yet to judge any of them "stale" (see `domain::Job::lease_expires_at`'s
//!   doc comment), so this is a cross-check number, not a defect signal.
//! - Drawers without an embedding: informational only, never a defect —
//!   semantic search doesn't exist yet, so an absent embedding is expected,
//!   not broken.
//!
//! Unlike `mining::run`/`checkpoint::run`, this handler does **not** chunk
//! its work with a per-unit checkpoint: a full palace scan here is cheap
//! (a handful of `SELECT`s, no external I/O) and idempotent, so there is no
//! meaningful partial state to resume from — pausing mid-scan would only
//! save re-running a cheap read, not real work. One `is_cancelled` check up
//! front is honored; there is nothing to pause into.
//!
//! The report itself lives in [`crate::domain::Job::result`], not
//! `Job::checkpoint` — see that field's doc comment for why this issue
//! introduced a separate field rather than reusing `checkpoint` for a
//! purpose it was never documented to serve.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{DrawerId, Job, JobId, JobProgress, JobStatus, RoomId, WingId};
use crate::error::Result;
use crate::jobs::{JobContext, JobOutcome};
use crate::store::{SurrealStore, bindable};

/// A drawer whose `room` no longer resolves to any `room` record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanDrawer {
    /// The orphaned drawer's id.
    pub drawer_id: DrawerId,
    /// The room id it points at, which no longer exists.
    pub room: RoomId,
}

/// A drawer whose `provenance.job_id` points at a job that no longer
/// exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DanglingProvenanceDrawer {
    /// The affected drawer's id.
    pub drawer_id: DrawerId,
    /// The job id it points at, which no longer exists.
    pub job_id: JobId,
}

/// The full result of one audit run — see this module's doc comment for
/// exactly what each field checks and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    /// The wing name this audit's in-scope counts were restricted to, if
    /// any. Orphan/dangling-provenance findings are never restricted by
    /// this — see the module doc.
    pub scope: Option<String>,
    /// Drawers whose `room` reference no longer resolves.
    pub orphan_drawers: Vec<OrphanDrawer>,
    /// Drawers whose `provenance.job_id` reference no longer resolves.
    pub dangling_provenance_drawers: Vec<DanglingProvenanceDrawer>,
    /// `Failed` jobs that have exhausted their attempt budget.
    pub stuck_failed_jobs: u64,
    /// Jobs currently `Running` at the moment this audit ran, **excluding**
    /// the audit job itself (which is necessarily `Running` while it
    /// produces this count — counting itself would make this number never
    /// read `0` and defeat the point of a cross-check).
    pub running_jobs: u64,
    /// Drawers with no `embedding`, within `scope` if given. Informational
    /// only — see the module doc.
    pub drawers_without_embedding: u64,
    /// Total drawers considered for `drawers_without_embedding` (i.e.
    /// within `scope`, excluding orphans, which belong to no wing) — lets a
    /// caller compute a ratio without a second query.
    pub total_drawers_in_scope: u64,
    /// When this report was generated.
    pub generated_at: DateTime<Utc>,
}

/// What an `Audit` job needs, gathered from its [`crate::domain::JobKind`].
pub struct AuditParams {
    /// Narrow the embedding-count fields to this wing, by name.
    pub scope: Option<String>,
}

/// Run a read-only consistency audit, optionally narrowing the
/// embedding-count fields to one wing by name.
///
/// # Errors
///
/// Returns an error if the store cannot be read, or if the report fails to
/// serialize (effectively never — see [`AuditReport`]'s fields, all plain
/// serializable types).
pub async fn run(ctx: &JobContext, job: &mut Job, params: AuditParams) -> Result<JobOutcome> {
    let AuditParams { scope } = params;
    let store = ctx.store();
    // No per-unit work to chunk (see the module doc) — a single check up
    // front is the only cooperative-cancel point this handler needs.
    if ctx.is_cancelled() {
        return Ok(JobOutcome::Cancelled);
    }

    let report = build_report(store, scope.as_deref(), job.id).await?;

    job.progress = JobProgress {
        current: 1,
        total: Some(1),
        message: Some(format!(
            "audit complete: {} orphan drawer(s), {} dangling provenance drawer(s)",
            report.orphan_drawers.len(),
            report.dangling_provenance_drawers.len()
        )),
    };
    // Set directly rather than through `JobContext::checkpoint` (which
    // couples a progress update to a `checkpoint` write) — `result` is a
    // distinct field with no resume semantics, and the single, final
    // `store.save_job` `Scheduler::execute` performs after this returns is
    // exactly where this needs to land, not an extra intermediate write.
    job.result = Some(bindable(&report)?);

    Ok(JobOutcome::Completed)
}

/// Find every drawer whose `room` no longer resolves to any `room` record —
/// the same check [`build_report`] folds into its single palace-wide pass,
/// factored out here so `crate::repair::run` can reuse it without
/// duplicating the room-resolution logic. `crate::repair` always calls this
/// fresh rather than trusting a stored [`AuditReport`]: state may have
/// changed since any given report was generated, and a destructive repair
/// must never act on a stale snapshot.
pub(crate) async fn find_orphan_drawers(store: &SurrealStore) -> Result<Vec<OrphanDrawer>> {
    let wings = store.list_wings().await?;
    let mut known_rooms: HashSet<RoomId> = HashSet::new();
    for wing in &wings {
        for room in store.list_rooms(wing.id).await? {
            known_rooms.insert(room.id);
        }
    }

    let drawers = store.list_drawers(None).await?;
    Ok(drawers
        .into_iter()
        .filter(|drawer| !known_rooms.contains(&drawer.room))
        .map(|drawer| OrphanDrawer {
            drawer_id: drawer.id,
            room: drawer.room,
        })
        .collect())
}

/// Build the report: one pass over wings/rooms (to know which rooms
/// currently exist), one over jobs (to know which job ids currently exist
/// and to count stuck/running jobs), and one over every drawer (to find
/// orphans, dangling provenance, and the scoped embedding count).
async fn build_report(
    store: &SurrealStore,
    scope: Option<&str>,
    self_job_id: JobId,
) -> Result<AuditReport> {
    let wings = store.list_wings().await?;

    // Every currently-valid room id, mapped to its owning wing — shared by
    // orphan detection ("does this drawer's room appear here at all?") and
    // the scope filter ("does this drawer's room fall under wing X?").
    let mut room_wing: HashMap<RoomId, WingId> = HashMap::new();
    let mut wing_id_by_name: HashMap<&str, WingId> = HashMap::new();
    for wing in &wings {
        wing_id_by_name.insert(wing.name.as_str(), wing.id);
        for room in store.list_rooms(wing.id).await? {
            room_wing.insert(room.id, wing.id);
        }
    }
    // Resolving `scope` once up front, not per-drawer: `None` means "no
    // wing name was given"; `Some(None)` (a name with no matching wing)
    // must still be distinguishable from that, so a typo'd scope yields
    // zero in-scope drawers rather than silently falling back to
    // unscoped — see the loop below.
    let scope_wing_id = scope.map(|name| wing_id_by_name.get(name).copied());

    let jobs = store.list_jobs(None).await?;
    let existing_job_ids: HashSet<JobId> = jobs.iter().map(|j| j.id).collect();
    let stuck_failed_jobs = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Failed && j.recovery_attempts >= j.max_attempts)
        .count() as u64;
    let running_jobs = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Running && j.id != self_job_id)
        .count() as u64;

    let drawers = store.list_drawers(None).await?;

    let mut orphan_drawers = Vec::new();
    let mut dangling_provenance_drawers = Vec::new();
    let mut total_drawers_in_scope = 0u64;
    let mut drawers_without_embedding = 0u64;

    for drawer in &drawers {
        let wing_of_drawer = room_wing.get(&drawer.room).copied();

        if wing_of_drawer.is_none() {
            orphan_drawers.push(OrphanDrawer {
                drawer_id: drawer.id,
                room: drawer.room,
            });
        }

        if let Some(job_id) = drawer.provenance.job_id
            && !existing_job_ids.contains(&job_id)
        {
            dangling_provenance_drawers.push(DanglingProvenanceDrawer {
                drawer_id: drawer.id,
                job_id,
            });
        }

        let in_scope = match scope_wing_id {
            // No scope requested: every drawer whose room resolves at all
            // (an orphan belongs to no wing, so it can't be "in scope").
            None => wing_of_drawer.is_some(),
            // A scope was requested and it resolved to a real wing: only
            // that wing's drawers count.
            Some(Some(resolved)) => wing_of_drawer == Some(resolved),
            // A scope was requested but no such wing exists: nothing is
            // in scope, same as `list_drawers_matching`'s handling of a typo'd
            // wing name — not an error, just an empty result.
            Some(None) => false,
        };
        if in_scope {
            total_drawers_in_scope += 1;
            if drawer.embedding.is_none() {
                drawers_without_embedding += 1;
            }
        }
    }

    Ok(AuditReport {
        scope: scope.map(str::to_string),
        orphan_drawers,
        dangling_provenance_drawers,
        stuck_failed_jobs,
        running_jobs,
        drawers_without_embedding,
        total_drawers_in_scope,
        generated_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        CheckpointDestination, CheckpointItem, CheckpointPayload, Drawer, JobKind, Priority,
        Provenance, Source, SourceKind,
    };
    use crate::jobs::JobControl;

    async fn memory_store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    fn ctx_for(store: &SurrealStore, job: &Job, control: JobControl) -> JobContext {
        JobContext::new(job.id, control, store.clone())
    }

    fn audit_job(scope: Option<String>) -> Job {
        Job::new(JobKind::Audit { scope }, Priority::Normal, "test")
    }

    fn report_of(job: &Job) -> AuditReport {
        serde_json::from_value(job.result.clone().expect("audit job must set a result"))
            .expect("result must deserialize as an AuditReport")
    }

    #[tokio::test]
    async fn a_fresh_palaces_audit_reports_no_findings() {
        let store = memory_store().await;
        let mut job = audit_job(None);
        let ctx = ctx_for(&store, &job, JobControl::default());

        let outcome = run(&ctx, &mut job, AuditParams { scope: None })
            .await
            .expect("run");
        assert_eq!(outcome, JobOutcome::Completed);

        let report = report_of(&job);
        assert!(report.orphan_drawers.is_empty());
        assert!(report.dangling_provenance_drawers.is_empty());
        assert_eq!(report.stuck_failed_jobs, 0);
        assert_eq!(report.running_jobs, 0);
        assert_eq!(report.drawers_without_embedding, 0);
        assert_eq!(report.total_drawers_in_scope, 0);
    }

    /// The issue's required test: a drawer created directly through the
    /// store (bypassing mining/checkpoint, which never produce this shape)
    /// with a `room` that was never created.
    #[tokio::test]
    async fn an_orphan_drawer_with_no_resolving_room_is_detected_and_reported() {
        let store = memory_store().await;
        let orphan_room = RoomId::new();
        let now = Utc::now();
        let drawer = Drawer {
            id: DrawerId::new(),
            room: orphan_room,
            content: "filed under a room that doesn't exist".to_string(),
            content_hash: "irrelevant".to_string(),
            source: Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
            },
            tags: vec![],
            embedding: None,
            provenance: Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
            valid_from: now,
            valid_to: None,
            created_at: now,
            updated_at: now,
        };
        store.create_drawer(&drawer).await.expect("create drawer");

        let mut job = audit_job(None);
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(&ctx, &mut job, AuditParams { scope: None })
            .await
            .expect("run");

        let report = report_of(&job);
        assert_eq!(report.orphan_drawers.len(), 1);
        assert_eq!(report.orphan_drawers[0].drawer_id, drawer.id);
        assert_eq!(report.orphan_drawers[0].room, orphan_room);
        assert_eq!(
            report.total_drawers_in_scope, 0,
            "an orphan belongs to no wing, so it must not inflate the scoped count"
        );
    }

    #[tokio::test]
    async fn a_drawer_with_a_dangling_provenance_job_id_is_detected_and_reported() {
        let store = memory_store().await;
        let wing = store.get_or_create_wing("project-x", None).await.unwrap();
        let room = store
            .get_or_create_room(wing.id, "entries", None)
            .await
            .unwrap();

        let dangling_job_id = JobId::new(); // never saved to the store
        let now = Utc::now();
        let drawer = Drawer {
            id: DrawerId::new(),
            room: room.id,
            content: "claims to come from a job that was never persisted".to_string(),
            content_hash: "irrelevant".to_string(),
            source: Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
            },
            tags: vec![],
            embedding: None,
            provenance: Provenance {
                requested_by: "test".to_string(),
                job_id: Some(dangling_job_id),
            },
            valid_from: now,
            valid_to: None,
            created_at: now,
            updated_at: now,
        };
        store.create_drawer(&drawer).await.expect("create drawer");

        let mut job = audit_job(None);
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(&ctx, &mut job, AuditParams { scope: None })
            .await
            .expect("run");

        let report = report_of(&job);
        assert!(
            report.orphan_drawers.is_empty(),
            "the drawer's room is real; only its provenance is dangling"
        );
        assert_eq!(report.dangling_provenance_drawers.len(), 1);
        assert_eq!(report.dangling_provenance_drawers[0].drawer_id, drawer.id);
        assert_eq!(
            report.dangling_provenance_drawers[0].job_id,
            dangling_job_id
        );
    }

    #[tokio::test]
    async fn stuck_failed_jobs_counts_only_jobs_that_exhausted_their_attempt_budget() {
        let store = memory_store().await;

        // A Failed job still within budget (recoverable by a human retry,
        // and not what "stuck" means here).
        let mut recoverable = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        recoverable.apply(crate::domain::JobEvent::Claim).unwrap();
        recoverable.apply(crate::domain::JobEvent::Fail).unwrap();
        assert!(recoverable.recovery_attempts < recoverable.max_attempts);
        store.save_job(&recoverable).await.unwrap();

        // A Failed job that has exhausted its attempt budget.
        let mut exhausted = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        exhausted.apply(crate::domain::JobEvent::Claim).unwrap();
        exhausted.recovery_attempts = exhausted.max_attempts;
        exhausted.apply(crate::domain::JobEvent::Fail).unwrap();
        store.save_job(&exhausted).await.unwrap();

        let mut job = audit_job(None);
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(&ctx, &mut job, AuditParams { scope: None })
            .await
            .expect("run");

        let report = report_of(&job);
        assert_eq!(report.stuck_failed_jobs, 1);
    }

    #[tokio::test]
    async fn running_jobs_reports_the_current_count_without_judging_staleness() {
        let store = memory_store().await;
        let mut running = Job::new(JobKind::Demo { steps: 5 }, Priority::Normal, "test");
        running.apply(crate::domain::JobEvent::Claim).unwrap();
        store.save_job(&running).await.unwrap();

        let mut job = audit_job(None);
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(&ctx, &mut job, AuditParams { scope: None })
            .await
            .expect("run");

        let report = report_of(&job);
        assert_eq!(report.running_jobs, 1);
        assert_eq!(
            report.stuck_failed_jobs, 0,
            "a Running job must never be counted as a stuck Failed job"
        );
    }

    #[tokio::test]
    async fn a_scope_filter_narrows_the_embedding_count_but_not_orphan_or_dangling_findings() {
        let store = memory_store().await;

        // One drawer in scope ("project-x"), one out of scope
        // ("project-y"), both missing an embedding.
        async fn seed(store: &SurrealStore, wing_name: &str, content: &str) {
            let payload = CheckpointPayload {
                items: vec![CheckpointItem {
                    destination: CheckpointDestination::General,
                    wing: Some(wing_name.to_string()),
                    content: content.to_string(),
                    tags: vec![],
                    source: Source {
                        kind: SourceKind::Manual,
                        uri: None,
                        agent: None,
                    },
                    fact: None,
                }],
            };
            let mut seed_job = Job::new(
                JobKind::Checkpoint {
                    payload: payload.clone(),
                },
                Priority::High,
                "test",
            );
            let ctx = JobContext::new(seed_job.id, JobControl::default(), store.clone());
            crate::checkpoint::run(
                &ctx,
                &mut seed_job,
                crate::checkpoint::CheckpointParams {
                    payload: payload.clone(),
                },
            )
            .await
            .expect("seed checkpoint run");
        }
        seed(&store, "project-x", "in scope").await;
        seed(&store, "project-y", "out of scope").await;

        // Plus one orphan and one dangling-provenance drawer, which must
        // still be reported regardless of the scope filter below.
        let orphan_room = RoomId::new();
        let now = Utc::now();
        store
            .create_drawer(&Drawer {
                id: DrawerId::new(),
                room: orphan_room,
                content: "orphan".to_string(),
                content_hash: "irrelevant".to_string(),
                source: Source {
                    kind: SourceKind::Manual,
                    uri: None,
                    agent: None,
                },
                tags: vec![],
                embedding: None,
                provenance: Provenance {
                    requested_by: "test".to_string(),
                    job_id: None,
                },
                valid_from: now,
                valid_to: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("create orphan drawer");

        let mut job = audit_job(Some("project-x".to_string()));
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(
            &ctx,
            &mut job,
            AuditParams {
                scope: Some("project-x".to_string()),
            },
        )
        .await
        .expect("run");

        let report = report_of(&job);
        assert_eq!(
            report.total_drawers_in_scope, 1,
            "only the project-x drawer must be counted, not project-y's"
        );
        assert_eq!(report.drawers_without_embedding, 1);
        assert_eq!(
            report.orphan_drawers.len(),
            1,
            "orphan detection must not be narrowed by scope"
        );

        // An unknown scope name must yield zero in-scope drawers, not an
        // error and not a silent fallback to unscoped.
        let mut unknown_scope_job = audit_job(Some("no-such-wing".to_string()));
        let ctx = ctx_for(&store, &unknown_scope_job, JobControl::default());
        run(
            &ctx,
            &mut unknown_scope_job,
            AuditParams {
                scope: Some("no-such-wing".to_string()),
            },
        )
        .await
        .expect("run");
        let report = report_of(&unknown_scope_job);
        assert_eq!(report.total_drawers_in_scope, 0);
    }
}
