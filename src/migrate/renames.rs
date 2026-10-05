//! Piece of migration 3 (`since-0.2`): move stored values to the field names this release gave them.
//!
//! The audit job called its wing `scope`; every other job and every surface says `wing`.
//! A stored job keeps whatever key it was written with, so this moves the value across.
//!
//! It runs before the other pieces: they read records through the current types, which expect the new names.
//! Idempotent: a record that already has the new name no longer matches.

use super::MigrationFuture;
use crate::store::SurrealStore;

/// This migration's step, for [`super::DATA_MIGRATIONS`].
pub(super) fn apply(store: &SurrealStore) -> MigrationFuture<'_> {
    Box::pin(async move {
        let audits = store.rename_audit_scope_to_wing().await?;
        tracing::info!(audits, "renamed stored fields to their current names");
        Ok(())
    })
}
