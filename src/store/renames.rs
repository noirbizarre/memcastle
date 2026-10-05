//! Rewriting stored field names that a release renamed.
//!
//! Only for `crate::migrate` (migration 3, `since-0.2`).
//! A renamed field is not a schema change: the job's `kind` and `result` are `FLEXIBLE` objects the database does not
//! look inside, so an old row would keep its old key forever and the new code would read it as absent.
//! A pass that moves the value under the new key is the one-off that closes that gap.

use crate::error::Result;

use super::SurrealStore;

impl SurrealStore {
    /// Move an audit job's `kind.scope` and `result.scope` to `wing`, returning how many jobs changed.
    ///
    /// An audit job named its wing `scope` while the embed and extract jobs said `wing`, so the one name now
    /// serves every job kind.
    /// Without this a queued or paused audit would silently run palace-wide, losing the filter it was submitted with.
    /// Idempotent: a job that has no `scope` left no longer matches.
    pub(crate) async fn rename_audit_scope_to_wing(&self) -> Result<u64> {
        super::retrying_on_conflict(|| async {
            let mut response = super::checked(
                self.db
                    // Two statements, because a job that has not run yet has no `result`, and setting a nested
                    // key on it would invent an empty report.
                    .query(
                        "UPDATE job SET kind.wing = kind.scope, kind.scope = NONE \
                         WHERE kind.type = 'audit' AND kind.scope != NONE RETURN VALUE record::id(id); \
                         UPDATE job SET result.wing = result.scope, result.scope = NONE \
                         WHERE kind.type = 'audit' AND result.scope != NONE RETURN VALUE record::id(id)",
                    )
                    .await?,
            )?;
            let kinds: Vec<String> = super::take_rows(&mut response, 0)?;
            let results: Vec<String> = super::take_rows(&mut response, 1)?;
            Ok((kinds.len() + results.len()) as u64)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{Job, JobKind, Priority};
    use crate::store::SurrealStore;

    #[tokio::test]
    async fn an_audit_job_stored_with_a_scope_gains_the_wing_it_was_submitted_with() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let job = Job::new(JobKind::Audit { wing: None }, Priority::Normal, "test");
        store.save_job(&job).await.unwrap();
        // The row as 0.2.0 wrote it: the wing under `scope`, in the job and in its report.
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('job', '{}') SET \
                 kind = {{ type: 'audit', scope: 'project-x' }}, result = {{ scope: 'project-x', orphan_drawers: [] }}",
                job.id
            ))
            .await;

        assert_eq!(
            store.rename_audit_scope_to_wing().await.unwrap(),
            2,
            "the job's kind and its report are two renames"
        );

        let after = store.get_job(job.id).await.unwrap().expect("the job");
        assert!(
            matches!(after.kind, JobKind::Audit { wing: Some(ref wing) } if wing == "project-x"),
            "the filter must survive the rename, got {:?}",
            after.kind
        );
        let report = after.result.expect("the report");
        assert_eq!(report["wing"], "project-x");
        assert!(report.get("scope").is_none(), "no stale key is left behind");
        assert_eq!(
            store.rename_audit_scope_to_wing().await.unwrap(),
            0,
            "a second run finds nothing to rename"
        );
    }

    #[tokio::test]
    async fn an_audit_job_with_no_report_yet_is_renamed_without_inventing_one() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let job = Job::new(JobKind::Audit { wing: None }, Priority::Normal, "test");
        store.save_job(&job).await.unwrap();
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('job', '{}') SET kind = {{ type: 'audit', scope: 'project-y' }}",
                job.id
            ))
            .await;

        store.rename_audit_scope_to_wing().await.unwrap();

        let after = store.get_job(job.id).await.unwrap().expect("the job");
        assert!(
            matches!(after.kind, JobKind::Audit { wing: Some(ref wing) } if wing == "project-y")
        );
        assert!(
            after.result.is_none(),
            "a queued audit has no report to rename"
        );
    }
}
