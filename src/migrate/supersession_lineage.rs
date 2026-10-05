//! Piece of migration 3 (`since-0.2`): pair already-superseded drawers with their replacements (docs/adr/032).
//!
//! Drawers closed before supersession was recorded carry a `valid_to` but no `superseded_by`, and their
//! replacements no `supersedes`, so a history could not be walked across them. This links the pairs that can be
//! recognised without guessing: the successor opened at the instant the old drawer closed, or came from the same
//! mined document chunk a moment before.
//!
//! An ambiguous close is left unlinked, and so is a drawer closed without a replacement; nothing about a record
//! changes except the two link fields. Idempotent: a linked drawer no longer matches, so a second run writes
//! nothing.

use super::MigrationFuture;
use crate::store::SurrealStore;

/// This migration's step, for [`super::DATA_MIGRATIONS`].
pub(super) fn apply(store: &SurrealStore) -> MigrationFuture<'_> {
    Box::pin(async move {
        let linked = store.backfill_supersession_lineage().await?;
        tracing::info!(linked, "linked superseded drawers to their replacements");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::super::{DataMigration, run_with};
    use crate::domain::{
        Drawer, DrawerId, Origin, Provenance, RoomId, Source, SourceId, SourceKind,
    };
    use crate::store::SurrealStore;

    const MIGRATIONS: &[DataMigration] = &[DataMigration {
        version: 3,
        name: "supersession-lineage",
        apply: super::apply,
    }];

    fn drawer(room: RoomId, content: &str, origin: Option<Origin>) -> Drawer {
        Drawer::new(
            DrawerId::new(),
            room,
            content.to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin,
            },
            vec![],
            Provenance {
                requested_by: "test".to_string(),
                job_id: None,
            },
        )
    }

    fn origin(source: SourceId, chunk: u32) -> Origin {
        Origin {
            source_id: source,
            source: "directory".to_string(),
            document: "notes.md".to_string(),
            chunk,
            revision: "r1".to_string(),
        }
    }

    /// Forget the links, as a palace written before they existed would not have them.
    async fn forget_links(store: &SurrealStore) {
        store
            .execute_for_tests("UPDATE drawer SET supersedes = NONE, superseded_by = NONE")
            .await;
    }

    #[tokio::test]
    async fn a_manual_replacement_is_paired_with_the_drawer_it_closed() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let old = drawer(room, "We use Postgres.", None);
        let new = drawer(room, "We use SurrealDB.", None);
        store.create_drawer(&old).await.unwrap();
        // `supersede_drawer` opens the replacement at `at`, which is what the old code path also did.
        let at = Utc::now();
        let replacement = Drawer {
            valid_from: at,
            ..new.clone()
        };
        assert!(
            store
                .supersede_drawer(old.id, Some(&replacement), at)
                .await
                .unwrap()
        );
        forget_links(&store).await;

        let report = run_with(&store, MIGRATIONS).await.expect("migrate");

        assert_eq!(report.applied, ["supersession-lineage"]);
        let old_after = store.get_drawer(old.id).await.unwrap().unwrap();
        let new_after = store.get_drawer(new.id).await.unwrap().unwrap();
        assert_eq!(old_after.superseded_by, Some(new.id));
        assert_eq!(new_after.supersedes, Some(old.id));
    }

    #[tokio::test]
    async fn a_mined_replacement_that_opened_just_before_the_close_is_paired_by_its_chunk() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let source = SourceId::new();
        let old = drawer(room, "First draft of the notes.", Some(origin(source, 0)));
        let mut new = drawer(room, "Second draft of the notes.", Some(origin(source, 0)));
        store.create_drawer(&old).await.unwrap();
        // The pipeline built the replacement, then read the clock to close the old drawer.
        let at = Utc::now();
        new.valid_from = at - Duration::milliseconds(7);
        store
            .supersede_drawer(old.id, Some(&new), at)
            .await
            .unwrap();
        forget_links(&store).await;

        run_with(&store, MIGRATIONS).await.expect("migrate");

        let old_after = store.get_drawer(old.id).await.unwrap().unwrap();
        assert_eq!(old_after.superseded_by, Some(new.id));
    }

    #[tokio::test]
    async fn a_drawer_closed_without_a_replacement_stays_unlinked() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let old = drawer(room, "No longer true.", None);
        store.create_drawer(&old).await.unwrap();
        store
            .supersede_drawer(old.id, None, Utc::now())
            .await
            .unwrap();
        forget_links(&store).await;

        run_with(&store, MIGRATIONS).await.expect("migrate");

        let after = store.get_drawer(old.id).await.unwrap().unwrap();
        assert_eq!(after.superseded_by, None);
    }

    #[tokio::test]
    async fn an_ambiguous_successor_is_never_guessed() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let old = drawer(room, "Original.", None);
        store.create_drawer(&old).await.unwrap();
        let at = Utc::now();
        let first = Drawer {
            valid_from: at,
            ..drawer(room, "Candidate one.", None)
        };
        let second = Drawer {
            valid_from: at,
            ..drawer(room, "Candidate two.", None)
        };
        store
            .supersede_drawer(old.id, Some(&first), at)
            .await
            .unwrap();
        store.create_drawer(&second).await.unwrap();
        forget_links(&store).await;

        run_with(&store, MIGRATIONS).await.expect("migrate");

        let after = store.get_drawer(old.id).await.unwrap().unwrap();
        assert_eq!(
            after.superseded_by, None,
            "two drawers opened at the close, so neither can be called the replacement"
        );
    }

    #[tokio::test]
    async fn running_the_migration_twice_changes_nothing_the_second_time() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let room = RoomId::new();
        let old = drawer(room, "Before.", None);
        store.create_drawer(&old).await.unwrap();
        let at = Utc::now();
        let new = Drawer {
            valid_from: at,
            ..drawer(room, "After.", None)
        };
        store
            .supersede_drawer(old.id, Some(&new), at)
            .await
            .unwrap();
        forget_links(&store).await;

        run_with(&store, MIGRATIONS).await.expect("migrate");
        assert_eq!(store.backfill_supersession_lineage().await.unwrap(), 0);
        let second = run_with(&store, MIGRATIONS).await.expect("migrate again");
        assert!(second.applied.is_empty());
    }
}
