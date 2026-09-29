//! Migration 1: give legacy diary drawers the same provenance meaning as
//! every other writer.
//!
//! Before this migration `AppServices::diary_write` stored the agent identity
//! in both `source.agent` and `provenance.requested_by`, while mining and
//! checkpoint stored the submitting channel (`cli`/`http`/`mcp`) in
//! `requested_by`. The rule is now one meaning per field — see
//! [`crate::domain::Provenance`] — so an old diary drawer's `requested_by`
//! is rewritten to `"unknown"`: the channel it came through was never
//! recorded, and pretending otherwise would be a guess. `source.agent` is
//! untouched (diary reads filter on it).
//!
//! Idempotent: a rewritten drawer no longer has `requested_by` equal to its
//! agent, so a second run matches nothing.

use super::MigrationFuture;
use crate::store::SurrealStore;

/// The value a legacy diary drawer's `requested_by` is rewritten to.
const UNKNOWN_CHANNEL: &str = "unknown";

/// This migration's step, for [`super::DATA_MIGRATIONS`].
pub(super) fn apply(store: &SurrealStore) -> MigrationFuture<'_> {
    Box::pin(async move {
        let rewritten = store
            .rewrite_legacy_diary_requested_by(UNKNOWN_CHANNEL)
            .await?;
        tracing::info!(rewritten, "rewrote legacy diary provenance");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::super::{DataMigration, run_with};
    use crate::domain::{Drawer, DrawerId, Provenance, Source, SourceKind};
    use crate::store::SurrealStore;

    const MIGRATIONS: &[DataMigration] = &[DataMigration {
        version: 1,
        name: "diary-provenance",
        apply: super::apply,
    }];

    /// A drawer as the old code wrote it (or as any writer writes it now,
    /// depending on `requested_by`/`agent`).
    async fn write_drawer(
        store: &SurrealStore,
        agent: Option<&str>,
        requested_by: &str,
        job_id: Option<crate::domain::JobId>,
    ) -> DrawerId {
        let wing = store.get_or_create_wing("w", None).await.unwrap();
        let room = store
            .get_or_create_room(wing.id, "diary", None)
            .await
            .unwrap();
        let drawer = Drawer::new(
            DrawerId::new(),
            room.id,
            "content".to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: agent.map(str::to_string),
            },
            vec![],
            Provenance {
                requested_by: requested_by.to_string(),
                job_id,
            },
        );
        store.create_drawer(&drawer).await.unwrap();
        drawer.id
    }

    async fn requested_by_of(store: &SurrealStore, id: DrawerId) -> String {
        store
            .list_drawers(None)
            .await
            .unwrap()
            .into_iter()
            .find(|drawer| drawer.id == id)
            .expect("drawer exists")
            .provenance
            .requested_by
    }

    #[tokio::test]
    async fn legacy_diary_drawers_lose_the_agent_identity_in_requested_by_and_keep_it_in_source() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let legacy = write_drawer(&store, Some("pi-agent"), "pi-agent", None).await;

        let report = run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!((report.from_version, report.to_version), (0, 1));
        assert_eq!(requested_by_of(&store, legacy).await, "unknown");
        let drawer = store
            .list_drawers(None)
            .await
            .unwrap()
            .into_iter()
            .find(|drawer| drawer.id == legacy)
            .unwrap();
        assert_eq!(
            drawer.source.agent.as_deref(),
            Some("pi-agent"),
            "diary reads filter on source.agent, which must survive"
        );
    }

    #[tokio::test]
    async fn drawers_that_already_follow_the_rule_are_left_alone() {
        let store = SurrealStore::connect_memory_for_tests().await;
        // A diary write under the new rule: channel + agent.
        let current = write_drawer(&store, Some("pi-agent"), "mcp", None).await;
        // A mined file: a channel, no agent, a producing job.
        let mined = write_drawer(&store, None, "cli", Some(crate::domain::JobId::new())).await;
        // A checkpoint item: agent identity in source, channel in requested_by.
        let checkpointed = write_drawer(
            &store,
            Some("pi-agent"),
            "http",
            Some(crate::domain::JobId::new()),
        )
        .await;
        // A hand-named channel from the HTTP API that happens to differ.
        let custom = write_drawer(&store, Some("pi-agent"), "dashboard", None).await;

        run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!(requested_by_of(&store, current).await, "mcp");
        assert_eq!(requested_by_of(&store, mined).await, "cli");
        assert_eq!(requested_by_of(&store, checkpointed).await, "http");
        assert_eq!(requested_by_of(&store, custom).await, "dashboard");
    }

    #[tokio::test]
    async fn running_the_migration_twice_changes_nothing_the_second_time() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let legacy = write_drawer(&store, Some("pi-agent"), "pi-agent", None).await;
        run_with(&store, MIGRATIONS).await.expect("first run");

        // The runner skips it via the watermark; calling the step directly
        // proves the step itself is idempotent, not just gated.
        super::apply(&store).await.expect("re-applied step");
        let second = run_with(&store, MIGRATIONS).await.expect("second run");

        assert!(second.applied.is_empty());
        assert_eq!(requested_by_of(&store, legacy).await, "unknown");
    }
}
