//! Migration 3: everything that changed the stored shape since 0.2.0, as one step.
//!
//! 0.2.0 shipped migrations 1 and 2 only.
//! Nothing between that release and the next was published, so the steps written in the meantime are one step here:
//! a palace is either at 0.2.0's version 2 or new, and no palace exists at an intermediate version that would need each
//! piece applied on its own.
//! Each piece keeps its own module and tests, and this step runs them in the order they were written.
//!
//! The pieces are independent and idempotent, so a run that stops half-way and starts again repeats nothing harmful.

use super::{MigrationFuture, dedup_keys, supersession_lineage};
use crate::store::SurrealStore;

/// This migration's step, for [`super::DATA_MIGRATIONS`].
pub(super) fn apply(store: &SurrealStore) -> MigrationFuture<'_> {
    Box::pin(async move {
        // Order matters: the keys deduplication reads come first, the supersession links are independent of them.
        dedup_keys::apply(store).await?;
        supersession_lineage::apply(store).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::super::{DataMigration, run_with};
    use crate::domain::{Drawer, DrawerId, Provenance, RoomId, Source, SourceKind};
    use crate::store::SurrealStore;

    const MIGRATIONS: &[DataMigration] = &[DataMigration {
        version: 3,
        name: "since-0.2",
        apply: super::apply,
    }];

    #[tokio::test]
    async fn one_step_backfills_the_keys_and_the_lineage_of_a_0_2_palace() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let drawer = Drawer::new(
            DrawerId::new(),
            RoomId::new(),
            "We chose SurrealDB.".to_string(),
            Source::new(SourceKind::Manual, None, None),
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        );
        store.create_drawer(&drawer).await.unwrap();
        // The row as 0.2.0 wrote it: no fingerprint.
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('drawer', '{}') SET fingerprint = NONE",
                drawer.id
            ))
            .await;

        let report = run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!(report.applied, ["since-0.2"]);
        assert_eq!(
            store.backfill_drawer_fingerprints().await.unwrap(),
            0,
            "the fingerprint was filled by the single step"
        );
        let again = run_with(&store, MIGRATIONS).await.expect("migrate again");
        assert!(again.applied.is_empty());
    }
}
