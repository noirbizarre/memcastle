//! Piece of migration 3 (`since-0.2`): derive the identity keys deduplication reads (docs/adr/025).
//!
//! Drawers written earlier have no `fingerprint` and entities have no `key` or aliases, so an old memory could not
//! be recognised as a duplicate of a new one and an old entity could not be converged on. This fills them in from
//! what is already stored (`content`, `name`); nothing else about a record changes.
//!
//! It never merges: two drawers or entities that already look like duplicates are left as they are. Merging
//! history is a judgement call this step has no business making for every palace at once; new writes and
//! sightings converge on one of them deterministically.
//!
//! Idempotent: a record that already has its key no longer matches, so a second run writes nothing.

use super::MigrationFuture;
use crate::store::SurrealStore;

/// This migration's step, for [`super::DATA_MIGRATIONS`].
pub(super) fn apply(store: &SurrealStore) -> MigrationFuture<'_> {
    Box::pin(async move {
        let drawers = store.backfill_drawer_fingerprints().await?;
        let entities = store.backfill_entity_keys().await?;
        tracing::info!(
            drawers,
            entities,
            "derived deduplication keys for existing records"
        );
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
        name: "dedup-keys",
        apply: super::apply,
    }];

    async fn seed_legacy_drawer(store: &SurrealStore, room: RoomId, content: &str) -> Drawer {
        let drawer = Drawer::new(
            DrawerId::new(),
            room,
            content.to_string(),
            Source::new(SourceKind::Manual, None, None),
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        );
        store.create_drawer(&drawer).await.unwrap();
        // The row as an earlier version wrote it: no fingerprint.
        store
            .execute_for_tests(&format!(
                "UPDATE type::record('drawer', '{}') SET fingerprint = NONE",
                drawer.id
            ))
            .await;
        drawer
    }

    #[tokio::test]
    async fn legacy_drawers_and_entities_gain_keys_and_become_matchable() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let legacy = seed_legacy_drawer(&store, room, "We chose SurrealDB.").await;
        store
            .execute_for_tests(
                "CREATE type::record('entity', '00000000-0000-4000-8000-000000000001') \
                 SET name = 'Ada Lovelace', kind = 'person', properties = {}",
            )
            .await;

        let report = run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!(report.applied, ["dedup-keys"]);
        let probe = DrawerId::new();
        let found = store
            .find_duplicate_candidates(
                room,
                "no-such-hash",
                Some(&crate::domain::fingerprint("we chose surrealdb")),
                probe,
            )
            .await
            .unwrap();
        assert_eq!(found.first().map(|c| c.id), Some(legacy.id));
        let (ada, _) = store
            .resolve_or_create_entity("ada lovelace", "person", true)
            .await
            .unwrap();
        assert_eq!(
            ada.name, "Ada Lovelace",
            "the legacy entity is found, not duplicated"
        );
    }

    #[tokio::test]
    async fn running_the_migration_twice_changes_nothing_the_second_time() {
        let store = SurrealStore::connect_memory_for_tests().await;
        seed_legacy_drawer(&store, RoomId::new(), "Something worth keeping.").await;

        let first = run_with(&store, MIGRATIONS).await.expect("migrate");
        assert_eq!(first.applied, ["dedup-keys"]);
        super::apply(&store).await.expect("a second application");
        assert_eq!(store.backfill_drawer_fingerprints().await.unwrap(), 0);
        assert_eq!(store.backfill_entity_keys().await.unwrap(), 0);
        let second = run_with(&store, MIGRATIONS).await.expect("migrate again");
        assert!(second.applied.is_empty());
    }

    #[tokio::test]
    async fn existing_duplicates_are_left_unmerged() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        seed_legacy_drawer(&store, room, "The same words.").await;
        seed_legacy_drawer(&store, room, "The same words.").await;

        run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!(store.list_drawers(Some(room)).await.unwrap().len(), 2);
    }
}
