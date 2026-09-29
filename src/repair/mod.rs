//! The repair job handler: a narrow, dry-run-first set of destructive
//! palace-consistency fixes.
//!
//! Issue #17 named two candidate actions. Only one made it into this
//! handler:
//!
//! - **Remove orphan drawer** — the only action implemented. A drawer whose
//!   `room` no longer resolves (see `crate::audit::OrphanDrawer`) has
//!   nothing left to preserve, and deleting it is the only way to actually
//!   fix what `crate::audit` can only report.
//! - **Fail jobs stuck beyond a stale-lease/attempt-budget heuristic** —
//!   deliberately **dropped as redundant**, not merely deferred:
//!   `jobs::Scheduler::recover` already runs at every daemon startup and
//!   fails any crash-recovered `Running` job whose `attempt` has reached
//!   `max_attempts` (see that function's doc comment). There is no lease
//!   TTL yet (`domain::Job::lease_expires_at` is unpopulated — see
//!   `crate::audit`'s module doc), so there is no live signal this handler
//!   could use to find *additional* stuck jobs while the daemon stays up.
//!   The population `AuditReport::stuck_failed_jobs` counts is already
//!   `Failed`, not `Running` — a repair action to "fail" it again would be
//!   a no-op; the only real way out of that state is the existing,
//!   deliberately human-triggered `JobEvent::Retry` (`memcastle jobs
//!   retry`).
//!
//! `dry_run` (`true` by default at every CLI/API entry point) never
//! mutates: it only records in [`RepairReport::actions`] what *would* be
//! done. `dry_run = false` performs exactly those same actions — no
//! auto-repair beyond what a prior dry run would have shown.
//!
//! `based_on_job`, when given, must reference a completed
//! [`crate::domain::JobKind::Audit`] job and narrows this run's actions to
//! the orphan drawers *that audit* found. It never widens or replaces a
//! fresh scan, though: [`run`] always recomputes the live orphan set first
//! and only intersects it with the referenced audit's findings, so a room
//! that was created after that audit ran is correctly never touched, no
//! matter what the (by then stale) report said.
//!
//! A cancel request is honoured before each delete of an applied repair
//! (the codebase's only destructive loop); a pause request is not honoured
//! at all — see `crate::audit`'s note that these handlers run to completion
//! — so `POST /api/jobs/{id}/pause` on an audit or repair only *requests*
//! a pause, which such a job ignores.
//!
//! Like `crate::audit`, this handler does not chunk its work with a
//! per-unit checkpoint — see that module's doc comment for why a palace
//! scan (plus, here, a handful of deletes) doesn't need resumable partial
//! state.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::audit::OrphanDrawer;
use crate::domain::{DrawerId, Job, JobId, JobKind, JobProgress};
use crate::error::{Error, Result};
use crate::jobs::{JobContext, JobOutcome};
use crate::store::{SurrealStore, bindable};

/// One repair action, planned or applied — an internally-tagged enum
/// (rather than a bare `Vec<OrphanDrawer>`) so [`RepairReport::actions`]
/// stays self-describing once a second action type exists (see issue #41,
/// "Expand audit/repair coverage").
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RepairAction {
    /// Remove a drawer whose `room` no longer resolves.
    RemoveOrphanDrawer(OrphanDrawer),
}

/// The full result of one repair run — see this module's doc comment for
/// exactly what it does and why `dry_run`/`based_on_job` behave the way
/// they do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairReport {
    /// Whether [`Self::actions`] were only planned (`true`) or actually
    /// applied (`false`).
    pub dry_run: bool,
    /// The audit job this run's actions were narrowed to, if any.
    pub based_on_job: Option<JobId>,
    /// Actions planned or applied, in the order they were considered.
    pub actions: Vec<RepairAction>,
    /// When this report was generated.
    pub generated_at: DateTime<Utc>,
}

/// What a `Repair` job needs, gathered from its [`crate::domain::JobKind`].
pub struct RepairParams {
    /// Report what would be removed without removing it.
    pub dry_run: bool,
    /// Narrow the repair to what this completed audit also found.
    pub based_on_job: Option<JobId>,
}

/// Run a repair: plan (and, unless `dry_run`, apply) the orphan-drawer
/// removals currently found in the palace, optionally narrowed to what a
/// prior audit job also found.
///
/// # Errors
///
/// Returns an error if the store cannot be read or written, if
/// `based_on_job` doesn't resolve to any job, or if it resolves to a job
/// that isn't a completed [`crate::domain::JobKind::Audit`] (see
/// [`Error::InvalidBasedOnJob`]).
pub async fn run(ctx: &JobContext, job: &mut Job, params: RepairParams) -> Result<JobOutcome> {
    let RepairParams {
        dry_run,
        based_on_job,
    } = params;
    let store = ctx.store();
    // No per-unit work to chunk (see the module doc) — a single check up
    // front is the only cooperative-cancel point this handler needs.
    if ctx.is_cancelled() {
        return Ok(JobOutcome::Cancelled);
    }

    // Always the current, live orphan set — never the possibly-stale
    // contents of a stored report, even when `based_on_job` is given (see
    // the module doc on why `based_on_job` can only narrow this, never
    // replace it).
    let mut orphans = crate::audit::find_orphan_drawers(store).await?;
    if let Some(audit_id) = based_on_job {
        let audited_ids = load_audited_orphan_ids(store, audit_id).await?;
        orphans.retain(|orphan| audited_ids.contains(&orphan.drawer_id));
    }

    let (actions, cancelled) = plan_or_apply(store, ctx, orphans, dry_run).await?;

    job.progress = JobProgress {
        current: 1,
        total: Some(1),
        message: Some(format!(
            "{}{}: {} orphan drawer(s)",
            if dry_run { "planned" } else { "applied" },
            if cancelled {
                " before cancellation"
            } else {
                ""
            },
            actions.len()
        )),
    };
    // Set directly, not through `JobContext::checkpoint` — same reasoning
    // as `audit::run`: `result` has no resume semantics, and the single
    // final `store.save_job` `Scheduler::execute` performs after this
    // returns is exactly where this needs to land.
    job.result = Some(bindable(&RepairReport {
        dry_run,
        based_on_job,
        actions,
        generated_at: Utc::now(),
    })?);

    // The report above is kept even when cancelled: deletions are
    // irreversible, so what was already removed must stay on record.
    Ok(if cancelled {
        JobOutcome::Cancelled
    } else {
        JobOutcome::Completed
    })
}

/// Plan (dry run) or apply the removal of each orphan, returning the actions
/// taken and whether a cancel stopped the loop early.
///
/// The cancel check sits before *each* delete, not just at the top of
/// [`run`]: this is the one destructive loop in the codebase, and a cancel a
/// user sends mid-repair must stop further deletions rather than be honoured
/// only after the last one. Pause is deliberately not honoured — a
/// half-applied repair has no checkpoint worth resuming from, since the next
/// run recomputes the live orphan set.
async fn plan_or_apply(
    store: &SurrealStore,
    ctx: &JobContext,
    orphans: Vec<crate::audit::OrphanDrawer>,
    dry_run: bool,
) -> Result<(Vec<RepairAction>, bool)> {
    let mut actions = Vec::with_capacity(orphans.len());
    for orphan in orphans {
        if !dry_run {
            if ctx.is_cancelled() {
                return Ok((actions, true));
            }
            store.delete_drawer(orphan.drawer_id).await?;
        }
        actions.push(RepairAction::RemoveOrphanDrawer(orphan));
    }
    Ok((actions, false))
}

/// Load the drawer ids a prior audit job flagged as orphans.
///
/// Errors are caller-facing mistakes (a typo'd id, a `Mine`/`Demo` job's id
/// passed by accident, an audit that's still running), not storage
/// failures — see [`Error::InvalidBasedOnJob`].
async fn load_audited_orphan_ids(
    store: &SurrealStore,
    audit_id: JobId,
) -> Result<HashSet<DrawerId>> {
    let audit_job = store
        .get_job(audit_id)
        .await?
        .ok_or_else(|| Error::JobNotFound {
            id: audit_id.to_string(),
        })?;
    if !matches!(audit_job.kind, JobKind::Audit { .. }) {
        return Err(Error::InvalidBasedOnJob {
            id: audit_id.to_string(),
            message: "must reference an audit job, not another job kind".to_string(),
        });
    }
    let result = audit_job.result.ok_or_else(|| Error::InvalidBasedOnJob {
        id: audit_id.to_string(),
        message: "the referenced audit has no result yet — it may not have completed".to_string(),
    })?;
    let report: crate::audit::AuditReport =
        serde_json::from_value(result).map_err(|source| Error::InvalidBasedOnJob {
            id: audit_id.to_string(),
            message: format!("its result is not a readable audit report: {source}"),
        })?;
    Ok(report
        .orphan_drawers
        .into_iter()
        .map(|orphan| orphan.drawer_id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DrawerId, Priority, Provenance, RoomId, Source, SourceKind};
    use crate::jobs::JobControl;
    use crate::store::SurrealStore;

    async fn memory_store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    fn ctx_for(store: &SurrealStore, job: &Job, control: JobControl) -> JobContext {
        JobContext::new(job.id, control, store.clone())
    }

    fn repair_job(dry_run: bool, based_on_job: Option<JobId>) -> Job {
        Job::new(
            JobKind::Repair {
                dry_run,
                based_on_job,
            },
            Priority::Normal,
            "test",
        )
    }

    fn report_of(job: &Job) -> RepairReport {
        serde_json::from_value(job.result.clone().expect("repair job must set a result"))
            .expect("result must deserialize as a RepairReport")
    }

    /// A drawer created directly through the store (bypassing
    /// mining/checkpoint, which never produce this shape) with a `room`
    /// that was never created — the same fixture `audit::mod`'s tests use.
    async fn create_orphan_drawer(store: &SurrealStore) -> (DrawerId, RoomId) {
        let orphan_room = RoomId::new();
        let now = Utc::now();
        let drawer_id = DrawerId::new();
        let drawer = crate::domain::Drawer {
            id: drawer_id,
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
        (drawer_id, orphan_room)
    }

    #[tokio::test]
    async fn a_cancel_stops_an_applied_repair_before_it_deletes_anything_further() {
        let store = memory_store().await;
        let (first, _) = create_orphan_drawer(&store).await;
        let (second, _) = create_orphan_drawer(&store).await;
        let orphans = crate::audit::find_orphan_drawers(&store).await.unwrap();
        assert_eq!(orphans.len(), 2);

        let control = JobControl::default();
        control.request_cancel();
        let job = repair_job(false, None);
        let ctx = ctx_for(&store, &job, control);

        let (actions, cancelled) = plan_or_apply(&store, &ctx, orphans, false).await.unwrap();

        assert!(cancelled, "the loop must report that it was cut short");
        assert!(
            actions.is_empty(),
            "nothing may be reported as done that was not"
        );
        assert!(store.drawer_exists(first).await.unwrap());
        assert!(store.drawer_exists(second).await.unwrap());
    }

    #[tokio::test]
    async fn a_cancel_does_not_stop_a_dry_run_from_finishing_its_plan() {
        // A dry run deletes nothing, so there is nothing for a cancel to
        // protect and the plan is complete and cheap.
        let store = memory_store().await;
        create_orphan_drawer(&store).await;
        let orphans = crate::audit::find_orphan_drawers(&store).await.unwrap();
        let control = JobControl::default();
        control.request_cancel();
        let ctx = ctx_for(&store, &repair_job(true, None), control);

        let (actions, cancelled) = plan_or_apply(&store, &ctx, orphans, true).await.unwrap();

        assert!(!cancelled);
        assert_eq!(actions.len(), 1);
    }

    #[tokio::test]
    async fn a_fresh_palaces_repair_plans_and_applies_nothing() {
        let store = memory_store().await;

        for dry_run in [true, false] {
            let mut job = repair_job(dry_run, None);
            let ctx = ctx_for(&store, &job, JobControl::default());
            let outcome = run(
                &ctx,
                &mut job,
                RepairParams {
                    dry_run,
                    based_on_job: None,
                },
            )
            .await
            .expect("run");
            assert_eq!(outcome, JobOutcome::Completed);

            let report = report_of(&job);
            assert_eq!(report.dry_run, dry_run);
            assert!(report.actions.is_empty());
        }
    }

    /// The issue's required test: dry-run lists the planned removal but
    /// never actually removes the drawer.
    #[tokio::test]
    async fn dry_run_plans_an_orphan_removal_without_deleting_it() {
        let store = memory_store().await;
        let (drawer_id, room) = create_orphan_drawer(&store).await;

        let mut job = repair_job(true, None);
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(
            &ctx,
            &mut job,
            RepairParams {
                dry_run: true,
                based_on_job: None,
            },
        )
        .await
        .expect("run");

        let report = report_of(&job);
        assert_eq!(report.actions.len(), 1);
        let RepairAction::RemoveOrphanDrawer(orphan) = &report.actions[0];
        assert_eq!(orphan.drawer_id, drawer_id);
        assert_eq!(orphan.room, room);

        let remaining = store.list_drawers(None).await.expect("list drawers");
        assert!(
            remaining.iter().any(|d| d.id == drawer_id),
            "dry_run must never delete anything"
        );
    }

    /// The issue's required test: applying removes the orphan, and a
    /// subsequent audit shows zero findings.
    #[tokio::test]
    async fn applying_removes_the_orphan_drawer_and_a_subsequent_audit_shows_zero_findings() {
        let store = memory_store().await;
        let (drawer_id, _room) = create_orphan_drawer(&store).await;

        let mut job = repair_job(false, None);
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(
            &ctx,
            &mut job,
            RepairParams {
                dry_run: false,
                based_on_job: None,
            },
        )
        .await
        .expect("run");

        let report = report_of(&job);
        assert_eq!(report.actions.len(), 1);

        let remaining = store.list_drawers(None).await.expect("list drawers");
        assert!(
            !remaining.iter().any(|d| d.id == drawer_id),
            "apply must actually delete the orphan drawer"
        );

        let mut audit_job = Job::new(JobKind::Audit { scope: None }, Priority::Normal, "test");
        let audit_ctx = ctx_for(&store, &audit_job, JobControl::default());
        crate::audit::run(
            &audit_ctx,
            &mut audit_job,
            crate::audit::AuditParams { scope: None },
        )
        .await
        .expect("audit run");
        let audit_report: crate::audit::AuditReport =
            serde_json::from_value(audit_job.result.expect("audit must set a result"))
                .expect("audit result must deserialize");
        assert!(
            audit_report.orphan_drawers.is_empty(),
            "the orphan must be gone by the time a follow-up audit runs"
        );
    }

    #[tokio::test]
    async fn based_on_job_narrows_repair_to_what_that_audit_found() {
        let store = memory_store().await;

        // Orphan A exists before the audit runs, so the audit finds it.
        let (drawer_a, _room_a) = create_orphan_drawer(&store).await;

        let mut audit_job = Job::new(JobKind::Audit { scope: None }, Priority::Normal, "test");
        let audit_ctx = ctx_for(&store, &audit_job, JobControl::default());
        crate::audit::run(
            &audit_ctx,
            &mut audit_job,
            crate::audit::AuditParams { scope: None },
        )
        .await
        .expect("audit run");
        // `audit::run` only mutates its in-memory `Job`; persisting it is
        // normally `Scheduler::execute`'s job after the handler returns —
        // `load_audited_orphan_ids` reads it back via `store.get_job`, so
        // the fixture must persist it itself here.
        store.save_job(&audit_job).await.expect("save audit job");

        // Orphan B appears only after the audit ran — a live scan would
        // find it too, but this audit's report never mentioned it.
        let (drawer_b, _room_b) = create_orphan_drawer(&store).await;

        let mut job = repair_job(true, Some(audit_job.id));
        let ctx = ctx_for(&store, &job, JobControl::default());
        run(
            &ctx,
            &mut job,
            RepairParams {
                dry_run: true,
                based_on_job: Some(audit_job.id),
            },
        )
        .await
        .expect("run");

        let report = report_of(&job);
        assert_eq!(
            report.actions.len(),
            1,
            "only what the referenced audit found must be planned"
        );
        let RepairAction::RemoveOrphanDrawer(orphan) = &report.actions[0];
        assert_eq!(orphan.drawer_id, drawer_a);
        assert_ne!(
            orphan.drawer_id, drawer_b,
            "an orphan created after the audit ran must not be included"
        );
    }

    #[tokio::test]
    async fn an_unresolvable_based_on_job_is_rejected() {
        let store = memory_store().await;
        let never_saved = JobId::new();

        let mut job = repair_job(true, Some(never_saved));
        let ctx = ctx_for(&store, &job, JobControl::default());
        let error = run(
            &ctx,
            &mut job,
            RepairParams {
                dry_run: true,
                based_on_job: Some(never_saved),
            },
        )
        .await
        .expect_err("must reject an id that doesn't resolve");
        assert!(matches!(error, Error::JobNotFound { .. }));
    }

    #[tokio::test]
    async fn a_based_on_job_that_isnt_an_audit_job_is_rejected() {
        let store = memory_store().await;
        let demo_job = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        store.save_job(&demo_job).await.expect("save demo job");

        let mut job = repair_job(true, Some(demo_job.id));
        let ctx = ctx_for(&store, &job, JobControl::default());
        let error = run(
            &ctx,
            &mut job,
            RepairParams {
                dry_run: true,
                based_on_job: Some(demo_job.id),
            },
        )
        .await
        .expect_err("must reject a non-audit job kind");
        assert!(matches!(error, Error::InvalidBasedOnJob { .. }));
    }
}
