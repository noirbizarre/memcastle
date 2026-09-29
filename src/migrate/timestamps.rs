//! Migration 2: bring every stored optional timestamp into the canonical
//! form (`store::stored`).
//!
//! Earlier versions wrote the `option<string>` timestamp columns
//! (`drawer.valid_to`, `relates_to.valid_to`, `job.started_at`,
//! `job.completed_at`, `job.lease_expires_at`) with `DateTime::to_rfc3339`,
//! whose precision varies and whose suffix is `+00:00`. Rows in that form
//! still parse, but they do not sort against canonical ones, so they are
//! rewritten once. The instant is unchanged; only its spelling is.
//!
//! Idempotent: a canonical value is left alone, so a second run writes
//! nothing.

use super::MigrationFuture;
use crate::store::SurrealStore;

/// This migration's step, for [`super::DATA_MIGRATIONS`].
pub(super) fn apply(store: &SurrealStore) -> MigrationFuture<'_> {
    Box::pin(async move {
        let rewritten = store.canonicalize_optional_timestamps().await?;
        tracing::info!(rewritten, "rewrote legacy timestamps to the canonical form");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::super::{DataMigration, run_with};
    use crate::domain::{Job, JobKind, Priority};
    use crate::store::SurrealStore;

    const MIGRATIONS: &[DataMigration] = &[DataMigration {
        version: 2,
        name: "canonical-timestamps",
        apply: super::apply,
    }];

    /// A job row as version 0.x wrote it: `to_rfc3339`'s `+00:00`, and a
    /// precision that varies with the value.
    async fn seed_legacy_job(store: &SurrealStore) -> Job {
        let job = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        store.save_job(&job).await.unwrap();
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('job', '{}') SET \
                 started_at = '2026-01-02T03:04:05.5+00:00', \
                 completed_at = '2026-01-02T03:04:06+00:00'",
                job.id
            ))
            .await;
        job
    }

    #[tokio::test]
    async fn legacy_timestamps_are_rewritten_to_the_same_instant_in_canonical_form() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let job = seed_legacy_job(&store).await;

        let report = run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!(report.applied, ["canonical-timestamps"]);
        let migrated = store.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(
            migrated.started_at.map(crate::store::stored).as_deref(),
            Some("2026-01-02T03:04:05.500000000Z")
        );
        assert_eq!(
            migrated.completed_at.map(crate::store::stored).as_deref(),
            Some("2026-01-02T03:04:06.000000000Z")
        );
        assert_eq!(
            migrated.started_at.unwrap().timestamp_subsec_millis(),
            500,
            "the instant must not move, only its spelling"
        );
    }

    #[tokio::test]
    async fn canonical_rows_are_not_rewritten_and_a_second_run_changes_nothing() {
        let store = SurrealStore::connect_memory_for_tests().await;
        seed_legacy_job(&store).await;

        assert_eq!(store.canonicalize_optional_timestamps().await.unwrap(), 2);
        assert_eq!(
            store.canonicalize_optional_timestamps().await.unwrap(),
            0,
            "the second pass must find everything already canonical"
        );
    }

    #[tokio::test]
    async fn a_value_that_is_not_a_timestamp_fails_the_migration_naming_the_record() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let job = Job::new(JobKind::Demo { steps: 1 }, Priority::Normal, "test");
        store.save_job(&job).await.unwrap();
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('job', '{}') SET started_at = 'yesterday-ish'",
                job.id
            ))
            .await;

        let error = run_with(&store, MIGRATIONS).await.unwrap_err();

        assert!(
            error.to_string().contains(&job.id.to_string()),
            "the failure must name the record: {error}"
        );
    }
}
