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

    /// Move the adapter's name from `provider` to `source` everywhere it was stored, returning how many records
    /// changed.
    ///
    /// A mined place's identifier used to be `source` and the adapter that reads it `provider`; the adapter is now
    /// the `source` and the identifier is `source_id`, so a record that names both must be moved in that order or
    /// the identifier is overwritten by the name.
    /// Four places hold it:
    ///
    /// - a mining job (`kind.provider`) and its report (`result.source` and `result.provider`);
    /// - a drawer's origin, and the origin copied onto the facts extracted from it (`mentions` and `relates_to`);
    /// - the `source` table's own `provider` column.
    ///
    /// A record's `source_id` is written only when the record still has a `provider`, so a second run finds
    /// nothing and a record already renamed is never touched.
    pub(crate) async fn rename_provider_to_source(&self) -> Result<u64> {
        // The table and the paths are constants of this function, never a caller's, so formatting them in is not an
        // injection path (SurrealQL cannot bind identifiers).
        let origins = [
            ("drawer", "source.origin"),
            ("mentions", "provenance.origin"),
            ("relates_to", "provenance.origin"),
        ];
        let mut renamed = 0;
        for (table, origin) in origins {
            renamed += self
                .rename_where(&format!(
                    "UPDATE {table} SET {origin}.source_id = {origin}.source, {origin}.source = {origin}.provider, \
                     {origin}.provider = NONE WHERE {origin}.provider != NONE RETURN VALUE record::id(id)"
                ))
                .await?;
        }
        // A job's report names the place by `source` too, so it is moved before the adapter's name takes the key.
        renamed += self
            .rename_where(
                "UPDATE job SET result.source_id = result.source, result.source = result.provider, \
                 result.provider = NONE WHERE kind.type = 'mine' AND result.provider != NONE \
                 RETURN VALUE record::id(id)",
            )
            .await?;
        renamed += self
            .rename_where(
                "UPDATE job SET kind.source = kind.provider, kind.provider = NONE \
                 WHERE kind.type = 'mine' AND kind.provider != NONE RETURN VALUE record::id(id)",
            )
            .await?;
        // The column is no longer declared, so the write drops `provider` itself; naming it `NONE` is for the reader.
        renamed += self
            .rename_where(
                "UPDATE source SET source = provider, provider = NONE WHERE provider != NONE \
                 RETURN VALUE record::id(id)",
            )
            .await?;
        Ok(renamed)
    }

    /// Run one renaming `UPDATE ... RETURN VALUE record::id(id)` and count the records it changed.
    async fn rename_where(&self, statement: &str) -> Result<u64> {
        super::retrying_on_conflict(|| async {
            let mut response = super::checked(self.db.query(statement.to_string()).await?)?;
            let changed: Vec<String> = super::take_rows(&mut response, 0)?;
            Ok(changed.len() as u64)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        Drawer, DrawerId, Job, JobKind, MiningSource, Priority, Provenance, RoomId, Source,
        SourceKind,
    };
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

    #[tokio::test]
    async fn a_mined_drawer_keeps_its_place_id_and_gains_the_adapter_name_under_source() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let drawer = Drawer::new(
            DrawerId::new(),
            RoomId::new(),
            "Mined from a file.".to_string(),
            Source::new(SourceKind::File, None, None),
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        );
        store.create_drawer(&drawer).await.unwrap();
        // The origin as 0.2.0 wrote it: `source` was the place's id and `provider` the adapter.
        let place = crate::domain::SourceId::new();
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('drawer', '{}') SET source.origin = {{ source: '{place}', provider: 'directory', \
                 document: 'a.md', chunk: 0, revision: 'r1' }}",
                drawer.id
            ))
            .await;

        assert_eq!(store.rename_provider_to_source().await.unwrap(), 1);

        let origin = store
            .get_drawer(drawer.id)
            .await
            .unwrap()
            .and_then(|drawer| drawer.source.origin)
            .expect("the origin survived");
        assert_eq!(origin.source_id, place, "the place keeps its identifier");
        assert_eq!(
            origin.source, "directory",
            "the adapter takes the name `source`"
        );
        assert_eq!(
            store.rename_provider_to_source().await.unwrap(),
            0,
            "a second run finds nothing to rename, and so cannot overwrite the id with the name"
        );
    }

    #[tokio::test]
    async fn a_stored_mine_job_and_its_report_name_the_adapter_under_source() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let job = Job::new(
            JobKind::Mine {
                source: MiningSource::Named {
                    source: "placeholder".into(),
                    locator: None,
                },
                wing: None,
                full: false,
            },
            Priority::Normal,
            "test",
        );
        store.save_job(&job).await.unwrap();
        let place = crate::domain::SourceId::new();
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('job', '{}') SET \
                 kind = {{ type: 'mine', provider: 'pi', locator: NONE, wing: NONE, full: true }}, \
                 result = {{ source: '{place}', provider: 'pi', documents: 3 }}",
                job.id
            ))
            .await;

        store.rename_provider_to_source().await.unwrap();

        let after = store
            .get_job(job.id)
            .await
            .unwrap()
            .expect("the job still loads");
        assert!(
            matches!(
                after.kind,
                JobKind::Mine { source: MiningSource::Named { ref source, .. }, full: true, .. } if source == "pi"
            ),
            "got {:?}",
            after.kind
        );
        let report = after.result.expect("the report");
        assert_eq!(report["source"], "pi");
        assert_eq!(report["source_id"], place.to_string());
        assert!(
            report.get("provider").is_none(),
            "no stale key is left behind"
        );
    }
}
