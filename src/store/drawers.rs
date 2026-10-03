//! Drawer repository methods: writes, listing, and lexical search.
//!
//! Semantic/vector search is a deliberate gap here (see `search` module docs
//! and the architecture doc's non-goals list) — `embedding` is written when
//! present but nothing yet reads it back for ranking.

use serde::Deserialize;

use crate::domain::{Drawer, DrawerId, RoomId};
use crate::error::Result;

use super::SurrealStore;

/// One lexical search result: the drawer plus its BM25 relevance score.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct SearchHit {
    /// The matching drawer.
    #[serde(flatten)]
    pub drawer: Drawer,
    /// The BM25 score `search::score(1)` assigned this match — higher is
    /// more relevant. Not comparable across different queries.
    pub score: f32,
}

/// How a multi-word query's terms combine in the full-text match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// Every query term must appear in the drawer (SurrealDB's default `@1@`).
    All,
    /// A single matching term is enough; BM25 still ranks drawers with more
    /// matching terms higher.
    Any,
}

impl MatchMode {
    /// The full-text operator for this mode. A closed set of literals,
    /// because SurrealDB cannot bind an operator as a parameter and
    /// interpolating anything caller-supplied into the query would be an
    /// injection hole.
    const fn operator(self) -> &'static str {
        match self {
            Self::All => "@1@",
            Self::Any => "@1,OR@",
        }
    }
}

/// The column list every drawer read projects, so a native `id`/`room`
/// `RecordId` never has to be handled on the Rust side (see `store::mod`'s
/// module doc) and every datetime round-trips through a plain RFC3339
/// string that `chrono`'s default `serde` support parses directly.
pub(super) const DRAWER_COLUMNS: &str = "record::id(id) AS id, room, name, content, content_hash, source, tags, \
     embedding, provenance, <string>valid_from AS valid_from, valid_to, \
     <string>created_at AS created_at, <string>updated_at AS updated_at";

impl SurrealStore {
    /// Persist a new drawer. Drawers are never updated in place (`content`
    /// is immutable — see [`Drawer`]'s doc comment), so this is always a
    /// fresh `CREATE`, never an upsert.
    pub async fn create_drawer(&self, drawer: &Drawer) -> Result<()> {
        self.db
            .query(
                "CREATE type::record('drawer', $id) SET \
                 room = $room, name = $name, content = $content, content_hash = $content_hash, \
                 source = $source, tags = $tags, embedding = $embedding, provenance = $provenance, \
                 valid_from = <datetime>$valid_from, valid_to = $valid_to, \
                 created_at = <datetime>$created_at, updated_at = <datetime>$updated_at",
            )
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
            .bind(("updated_at", super::stored(drawer.updated_at)))
            .await?
            // `.await` alone only reports transport-level failures; a
            // rejected `SET` (e.g. a schema mismatch) would otherwise fail
            // silently and leave `create_drawer` reporting success for a
            // drawer that was never written. See `store::mod`'s doc comment.
            .check()?;
        Ok(())
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
        self.db
            .query("DELETE type::record('drawer', $id)")
            .bind(("id", id.to_string()))
            .await?
            // Same reasoning as `create_drawer`'s `.check()?` — a rejected
            // `DELETE` must not silently report success.
            .check()?;
        Ok(())
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

    /// List drawers whose content matches `query`, best first: lexical (BM25
    /// full-text) search over drawer content — the "basic
    /// working search path" this bootstrap establishes. Semantic and hybrid
    /// ranking are later phases layered on top of the same `drawer` table.
    ///
    /// `wing`/`room` optionally scope results by name. `drawer` has no
    /// `wing` column (only `room`; `room.wing` is one hop up — see
    /// `store::wings`'s module doc on why every FK here is a plain string,
    /// not a record link), so the wing scope resolves matching room ids via
    /// a nested subquery rather than a direct column comparison. Both
    /// scopes are expressed as `($param = NULL OR ...)` predicates — the
    /// same "optional filter" idiom as `list_jobs` (see its comment) — so
    /// SurrealDB applies them before `ORDER BY`/`LIMIT`, instead of this
    /// method fetching an unscoped page and filtering it in Rust, which
    /// would let an out-of-scope but higher-scoring hit crowd a requested
    /// scope's matches out of a capped result set.
    ///
    /// `mode` picks whether every query term must match or any one will
    /// (see [`MatchMode`]); the `search` module decides which to try.
    pub async fn list_drawers_matching(
        &self,
        query: &str,
        limit: u32,
        wing: Option<&str>,
        room: Option<&str>,
        mode: MatchMode,
    ) -> Result<Vec<SearchHit>> {
        let operator = mode.operator();
        let sql = format!(
            "SELECT {DRAWER_COLUMNS}, search::score(1) AS score FROM drawer \
             WHERE content {operator} $query \
               AND ($wing = NULL OR room IN ( \
                     SELECT VALUE record::id(id) FROM room WHERE wing IN ( \
                       SELECT VALUE record::id(id) FROM wing WHERE name = $wing))) \
               AND ($room = NULL OR room IN ( \
                     SELECT VALUE record::id(id) FROM room WHERE name = $room)) \
             ORDER BY score DESC LIMIT $limit"
        );
        let mut response = self
            .db
            .query(sql)
            .bind(("query", query.to_string()))
            // Bound through `bindable` (`serde_json::Value`), not `.bind()`
            // directly: binding `Option::None` the native way produces
            // SurrealDB's `NONE` (absence), which `$wing = NULL` never
            // matches — see `store::mod`'s regression test on `list_jobs`
            // for the exact failure mode this sidesteps.
            .bind(("wing", super::bindable(&wing)?))
            .bind(("room", super::bindable(&room)?))
            .bind(("limit", limit))
            .await?;
        super::take_rows(&mut response, 0)
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
             WHERE room = $room AND source.agent = $agent \
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
             WHERE provenance.job_id != NULL \
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
