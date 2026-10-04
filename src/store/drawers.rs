//! Drawer repository methods: writes and listing.
//!
//! Ranked retrieval (lexical, vector, hybrid) lives in `retrieval`; this
//! module is the plain create/read/delete surface.

use serde::Deserialize;

use crate::domain::{Drawer, DrawerId, RoomId};
use crate::error::Result;

use super::SurrealStore;

/// The column list every drawer read projects, so a native `id`/`room`
/// `RecordId` never has to be handled on the Rust side (see `store::mod`'s
/// module doc) and every datetime round-trips through a plain RFC3339
/// string that `chrono`'s default `serde` support parses directly.
pub(super) const DRAWER_COLUMNS: &str = "record::id(id) AS id, room, name, content, content_hash, source, tags, \
     embedding, provenance, <string>valid_from AS valid_from, valid_to, \
     <string>created_at AS created_at, <string>updated_at AS updated_at";

/// [`DRAWER_COLUMNS`] without the embedding, for ranked retrieval: a vector is
/// hundreds of floats the caller never asked for, and search results are
/// serialised straight onto the wire.
pub(super) const DRAWER_SEARCH_COLUMNS: &str = "record::id(id) AS id, room, name, content, content_hash, source, tags, \
     provenance, <string>valid_from AS valid_from, valid_to, \
     <string>created_at AS created_at, <string>updated_at AS updated_at";

/// The statement that writes one drawer, binding the names [`bind_drawer`] sets.
const CREATE_DRAWER: &str = "CREATE type::record('drawer', $id) SET \
     room = $room, name = $name, content = $content, content_hash = $content_hash, \
     source = $source, tags = $tags, embedding = $embedding, provenance = $provenance, \
     valid_from = <datetime>$valid_from, valid_to = $valid_to, \
     created_at = <datetime>$created_at, updated_at = <datetime>$updated_at";

/// Bind every parameter [`CREATE_DRAWER`] mentions, so the plain write and the
/// supersession transaction cannot drift apart on how a drawer is stored.
fn bind_drawer<'r>(
    query: surrealdb::method::Query<'r, surrealdb::engine::any::Any>,
    drawer: &Drawer,
) -> Result<surrealdb::method::Query<'r, surrealdb::engine::any::Any>> {
    Ok(query
        .bind(("id", drawer.id.to_string()))
        .bind(("room", drawer.room.to_string()))
        // `None` binds as `NONE`, which is what an `option<string>` field
        // wants: unnamed drawers must not collide on the unique index.
        .bind(("name", drawer.name.clone()))
        .bind(("content", drawer.content.clone()))
        .bind(("content_hash", drawer.content_hash.clone()))
        .bind(("source", super::bindable(&drawer.source)?))
        .bind(("tags", drawer.tags.clone()))
        .bind(("embedding", drawer.embedding.clone()))
        .bind(("provenance", super::bindable(&drawer.provenance)?))
        .bind(("valid_from", super::stored(drawer.valid_from)))
        .bind(("valid_to", drawer.valid_to.map(super::stored)))
        .bind(("created_at", super::stored(drawer.created_at)))
        .bind(("updated_at", super::stored(drawer.updated_at))))
}

impl SurrealStore {
    /// Persist a new drawer. Drawers are never updated in place (`content`
    /// is immutable — see [`Drawer`]'s doc comment), so this is always a
    /// fresh `CREATE`, never an upsert.
    pub async fn create_drawer(&self, drawer: &Drawer) -> Result<()> {
        // Retried because the write touches the drawer's search indexes, which
        // a background task also rewrites: see `retrying_on_conflict`.
        super::retrying_on_conflict(|| async {
            super::checked(bind_drawer(self.db.query(CREATE_DRAWER), drawer)?.await?)
                // `.await` alone only reports transport-level failures; a
                // rejected `SET` (e.g. a schema mismatch) would otherwise fail
                // silently and leave `create_drawer` reporting success for a
                // drawer that was never written. See `store::mod`'s doc comment.
                .map(|_| ())
        })
        .await
    }

    /// Close the validity of drawer `old` at `at` and, in the same
    /// transaction, open `replacement` from that instant.
    ///
    /// Supersession is how a drawer is corrected without rewriting history:
    /// the old drawer keeps its content, hash and id, and only gains a
    /// `valid_to`, so a point-in-time search still finds what was believed
    /// then. With no replacement it is an invalidation ("this was never
    /// true" or "no longer true"). Returns whether `old` was open and is now
    /// closed; `false` means it does not exist or was already closed, and
    /// nothing was written.
    ///
    /// `(room, name)` is unique, so the superseded drawer gives up its name
    /// (it stays addressable by id) for the replacement to take over.
    pub async fn supersede_drawer(
        &self,
        old: DrawerId,
        replacement: Option<&Drawer>,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        // `!valid_to` is the "still open" test (see `entities`'s module doc).
        let mut sql = String::from(
            "BEGIN TRANSACTION; \
             LET $closed = (UPDATE drawer SET valid_to = $at, updated_at = <datetime>$at, name = NONE \
                WHERE id = type::record('drawer', $old) AND !valid_to RETURN record::id(id) AS id); \
             IF array::len($closed) = 0 { THROW 'drawer not open'; }; ",
        );
        if replacement.is_some() {
            sql.push_str(CREATE_DRAWER);
            sql.push_str("; ");
        }
        sql.push_str("COMMIT TRANSACTION;");

        // Retried on a write conflict (see `retrying_on_conflict`): the
        // transaction wrote nothing, so running it again is safe.
        super::retrying_on_conflict(|| async {
            let query = self
                .db
                .query(sql.as_str())
                .bind(("old", old.to_string()))
                .bind(("at", super::stored(at)));
            let query = match replacement {
                Some(drawer) => bind_drawer(query, drawer)?,
                None => query,
            };
            let mut response = query.await?;
            // Every statement of a failed transaction reports an error, and
            // only one names the cause, so all of them are inspected rather
            // than `.check()`'s first.
            let errors: Vec<_> = response.take_errors().into_iter().collect();
            if errors.is_empty() {
                return Ok(true);
            }
            // The `THROW` above: nothing matched, nothing was written.
            if errors
                .iter()
                .any(|(_, error)| error.to_string().contains("drawer not open"))
            {
                return Ok(false);
            }
            Err(super::root_cause(errors)
                .expect("a non-empty list of errors has a root cause")
                .into())
        })
        .await
    }

    /// Persist `drawer` unless a drawer with that id already exists,
    /// returning whether it was written.
    ///
    /// For job handlers that derive a drawer's id from (job, item index): a
    /// crash between this write and the job's checkpoint makes the resumed
    /// attempt write the same item again, and this turns that replay into a
    /// no-op instead of a duplicate. The check and the write are two
    /// statements, which is safe only because one daemon owns the palace
    /// and one worker runs a given job — the same single-writer assumption
    /// `claim_next_job` documents.
    pub async fn create_drawer_once(&self, drawer: &Drawer) -> Result<bool> {
        if self.drawer_exists(drawer.id).await? {
            return Ok(false);
        }
        self.create_drawer(drawer).await?;
        Ok(true)
    }

    /// Whether a drawer with this id exists.
    pub async fn drawer_exists(&self, id: DrawerId) -> Result<bool> {
        #[derive(serde::Deserialize)]
        struct IdRow {
            #[allow(dead_code)]
            id: String,
        }
        let mut response = self
            .db
            .query("SELECT record::id(id) AS id FROM drawer WHERE id = type::record('drawer', $id)")
            .bind(("id", id.to_string()))
            .await?;
        let rows: Vec<IdRow> = super::take_rows(&mut response, 0)?;
        Ok(!rows.is_empty())
    }

    /// Permanently delete one drawer. Used by `crate::repair::run`'s "remove
    /// orphan drawer" action (an orphan has nothing left to preserve history
    /// for) and by `AppServices::delete_drawer`. Deleting a whole room or wing
    /// is [`Self::delete_room`] / [`Self::delete_wing`], which do it in one
    /// transaction. Succeeds for an id that does not exist: callers that must
    /// report that check first.
    pub async fn delete_drawer(&self, id: DrawerId) -> Result<()> {
        // Retried like `create_drawer`: deleting rewrites the search indexes.
        super::retrying_on_conflict(|| async {
            super::checked(
                self.db
                    .query("DELETE type::record('drawer', $id)")
                    .bind(("id", id.to_string()))
                    .await?,
            )
            // Same reasoning as `create_drawer`'s check — a rejected
            // `DELETE` must not silently report success.
            .map(|_| ())
        })
        .await
    }

    /// List the drawers filed under `room`, newest first — or, with `None`,
    /// every drawer in the palace across every room.
    ///
    /// The unscoped form exists because `audit::run` needs a palace-wide view
    /// regardless of its own optional wing scope: orphan and
    /// dangling-provenance detection would silently miss real findings
    /// outside whatever scope was requested (see that module's doc comment).
    /// Same optional-filter idiom as [`Self::list_jobs`], and one method
    /// rather than a scoped/unscoped pair that could drift apart.
    pub async fn list_drawers(&self, room: Option<RoomId>) -> Result<Vec<Drawer>> {
        let sql = format!(
            "SELECT {DRAWER_COLUMNS} FROM drawer WHERE $room = NULL OR room = $room \
             ORDER BY created_at DESC"
        );
        let room = room.map(|room| room.to_string());
        let mut response = self
            .db
            .query(sql)
            // `bindable`, not `.bind()` directly: see `list_jobs` on why a
            // native `None` would be `NONE` and never match `= NULL`.
            .bind(("room", super::bindable(&room)?))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// Rewrite the `provenance.requested_by` of drawers written by the old
    /// diary path, which stored the agent identity there instead of the
    /// channel (see [`crate::domain::Provenance`]), to `channel`. Returns how
    /// many were rewritten.
    ///
    /// Only for `crate::migrate`. A legacy diary drawer is one with no
    /// producing job, an agent in `source.agent`, and `requested_by` equal to
    /// that agent; a genuine channel name is never rewritten, and a drawer
    /// already carrying `channel` no longer matches, so re-running is a
    /// no-op.
    pub(crate) async fn rewrite_legacy_diary_requested_by(&self, channel: &str) -> Result<u64> {
        let mut response = self
            .db
            .query(
                // Truthiness (`!x`, `x`) rather than `= NULL`: a `None` may be
                // stored as `NONE` or `NULL` — see `entities`'s module doc.
                // The channel names are literals on purpose, not
                // `domain::channel`: a migration describes the data as it was
                // when it ran, so a later rename of a constant must not change
                // which drawers an already-shipped step rewrites.
                "UPDATE drawer SET provenance.requested_by = $channel \
                 WHERE !provenance.job_id AND source.agent \
                   AND provenance.requested_by = source.agent \
                   AND provenance.requested_by NOT IN ['cli', 'http', 'mcp'] \
                 RETURN record::id(id) AS id",
            )
            .bind(("channel", channel.to_string()))
            .await?
            .check()?;
        let rows: Vec<serde_json::Value> = response.take(0)?;
        Ok(rows.len() as u64)
    }

    /// The total number of drawers in the palace, for status reporting.
    pub async fn count_drawers(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Count {
            count: u64,
        }
        let mut response = self
            .db
            .query("SELECT count() AS count FROM drawer GROUP ALL")
            .await?;
        let counts: Vec<Count> = super::take_rows(&mut response, 0)?;
        Ok(counts.into_iter().next().map_or(0, |c| c.count))
    }

    /// List `agent`'s diary entries filed under `room`, newest first,
    /// capped at `limit`. Unlike `list_drawers` (unfiltered), diary reads
    /// are always scoped to one identity — the whole point of issue #13's
    /// design is that two identities sharing a wing/room never see each
    /// other's entries — so `agent` is a required equality filter, not an
    /// optional scope like `list_drawers_matching`'s wing/room. Being a plain
    /// `&str` (not `Option<&str>`), it binds directly rather than through
    /// `bindable`'s `$x = NULL` idiom.
    pub async fn list_diary_drawers(
        &self,
        room: RoomId,
        agent: &str,
        limit: u32,
    ) -> Result<Vec<Drawer>> {
        let sql = format!(
            "SELECT {DRAWER_COLUMNS} FROM drawer \
             WHERE room = $room AND source.agent = $agent AND !valid_to \
             ORDER BY created_at DESC LIMIT $limit"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("room", room.to_string()))
            .bind(("agent", agent.to_string()))
            .bind(("limit", limit))
            .await?;
        super::take_rows(&mut response, 0)
    }

    /// List up to `limit` of the most recent drawers written by a
    /// `JobKind::Checkpoint` job, optionally scoped to one wing by name —
    /// the store-level primitive behind `AppServices::wake_up`'s
    /// `recent_highlights`. "Checkpoint-originated" is identified via
    /// `provenance.job_id` referencing a job whose `kind.type` is
    /// `"checkpoint"` (see `AppServices::wake_up`'s doc comment for why
    /// this is chosen over a new tag convention: `checkpoint::run`
    /// already sets `provenance.job_id` on every drawer it writes, so
    /// this needs no new write path, only this read-side query). Same
    /// "scope pushed into SurrealQL before `ORDER BY`/`LIMIT`" idiom as
    /// `list_drawers_matching` — see that method's doc comment for why fetching
    /// unscoped and filtering in Rust would be wrong here too.
    pub async fn list_checkpoint_originated_drawers(
        &self,
        wing: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Drawer>> {
        let sql = format!(
            "SELECT {DRAWER_COLUMNS} FROM drawer \
             WHERE provenance.job_id != NULL AND !valid_to \
               AND provenance.job_id IN ( \
                     SELECT VALUE record::id(id) FROM job WHERE kind.type = 'checkpoint') \
               AND ($wing = NULL OR room IN ( \
                     SELECT VALUE record::id(id) FROM room WHERE wing IN ( \
                       SELECT VALUE record::id(id) FROM wing WHERE name = $wing))) \
             ORDER BY created_at DESC LIMIT $limit"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("wing", super::bindable(&wing)?))
            .bind(("limit", limit))
            .await?;
        super::take_rows(&mut response, 0)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::domain::{DrawerId, Provenance, Source, SourceKind};

    use super::*;

    fn drawer(room: RoomId, content: String) -> Drawer {
        Drawer::new(
            DrawerId::new(),
            room,
            content,
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
            },
            vec![],
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        )
    }

    /// The full-text and vector indexes on `drawer` are maintained by a
    /// SurrealDB background task that wakes shortly after every commit and
    /// rewrites index keys; a drawer write that overlaps it loses a write
    /// conflict. Writing steadily for longer than that debounce makes the
    /// overlap certain, so a write path that does not retry fails here.
    #[tokio::test]
    async fn drawer_writes_survive_background_index_compaction() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let wing = store.get_or_create_wing("w", None).await.expect("wing");
        let room = store
            .get_or_create_room(wing.id, "r", None)
            .await
            .expect("room")
            .id;

        let started = Instant::now();
        let mut round = 0_u32;
        while started.elapsed() < Duration::from_millis(2_500) {
            round += 1;
            let first = drawer(room, format!("first drawer of round {round}"));
            let second = drawer(room, format!("second drawer of round {round}"));
            store.create_drawer(&first).await.expect("create first");
            store.create_drawer(&second).await.expect("create second");
            store
                .supersede_drawer(first.id, None, chrono::Utc::now())
                .await
                .expect("supersede");
            store.delete_drawer(first.id).await.expect("delete first");
            store.delete_drawer(second.id).await.expect("delete second");
        }
        assert!(round > 1, "the loop must outlast at least one compaction");
    }

    /// A transaction that loses a conflict fails every statement, and the
    /// first of them is `NotExecuted`, not the conflict. Deleting a room is
    /// such a transaction, so this is what proves its retry can see the cause.
    #[tokio::test]
    async fn room_deletions_survive_background_index_compaction() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let wing = store.get_or_create_wing("w", None).await.expect("wing");

        let started = Instant::now();
        let mut round = 0_u32;
        while started.elapsed() < Duration::from_millis(2_500) {
            round += 1;
            let room = store
                .get_or_create_room(wing.id, &format!("room {round}"), None)
                .await
                .expect("room")
                .id;
            for n in 0..3 {
                let content = format!("drawer {n} of room {round}");
                store
                    .create_drawer(&drawer(room, content))
                    .await
                    .expect("create");
            }
            store.delete_room(room).await.expect("delete room");
        }
        assert!(round > 1, "the loop must outlast at least one compaction");
    }
}
