//! Drawer repository methods: writes, listing, and lexical search.
//!
//! Semantic/vector search is a deliberate gap here (see `search` module docs
//! and the architecture doc's non-goals list) — `embedding` is written when
//! present but nothing yet reads it back for ranking.

use serde::Deserialize;

use crate::domain::{Drawer, RoomId};
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

/// The column list every drawer read projects, so a native `id`/`room`
/// `RecordId` never has to be handled on the Rust side (see `store::mod`'s
/// module doc) and every datetime round-trips through a plain RFC3339
/// string that `chrono`'s default `serde` support parses directly.
const DRAWER_COLUMNS: &str = "record::id(id) AS id, room, content, content_hash, source, tags, \
     embedding, provenance, <string>valid_from AS valid_from, valid_to, \
     <string>created_at AS created_at, <string>updated_at AS updated_at";

impl SurrealStore {
    /// Persist a new drawer. Drawers are never updated in place (`content`
    /// is immutable — see [`Drawer`]'s doc comment), so this is always a
    /// fresh `CREATE`, never an upsert.
    pub async fn create_drawer(&self, drawer: &Drawer) -> Result<()> {
        self.db
            .query(
                "CREATE type::thing('drawer', $id) SET \
                 room = $room, content = $content, content_hash = $content_hash, \
                 source = $source, tags = $tags, embedding = $embedding, provenance = $provenance, \
                 valid_from = <datetime>$valid_from, valid_to = $valid_to, \
                 created_at = <datetime>$created_at, updated_at = <datetime>$updated_at",
            )
            .bind(("id", drawer.id.to_string()))
            .bind(("room", drawer.room.to_string()))
            .bind(("content", drawer.content.clone()))
            .bind(("content_hash", drawer.content_hash.clone()))
            .bind(("source", super::bindable(&drawer.source)?))
            .bind(("tags", drawer.tags.clone()))
            .bind(("embedding", drawer.embedding.clone()))
            .bind(("provenance", super::bindable(&drawer.provenance)?))
            .bind(("valid_from", drawer.valid_from.to_rfc3339()))
            .bind(("valid_to", drawer.valid_to.map(|dt| dt.to_rfc3339())))
            .bind(("created_at", drawer.created_at.to_rfc3339()))
            .bind(("updated_at", drawer.updated_at.to_rfc3339()))
            .await?
            // `.await` alone only reports transport-level failures; a
            // rejected `SET` (e.g. a schema mismatch) would otherwise fail
            // silently and leave `create_drawer` reporting success for a
            // drawer that was never written. See `store::mod`'s doc comment.
            .check()?;
        Ok(())
    }

    /// List every drawer filed under `room`, newest first.
    pub async fn list_drawers(&self, room: RoomId) -> Result<Vec<Drawer>> {
        let sql = format!(
            "SELECT {DRAWER_COLUMNS} FROM drawer WHERE room = $room ORDER BY created_at DESC"
        );
        Ok(self
            .db
            .query(sql)
            .bind(("room", room.to_string()))
            .await?
            .take(0)?)
    }

    /// The total number of drawers in the palace, for status reporting.
    pub async fn count_drawers(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Count {
            count: u64,
        }
        let counts: Vec<Count> = self
            .db
            .query("SELECT count() AS count FROM drawer GROUP ALL")
            .await?
            .take(0)?;
        Ok(counts.into_iter().next().map_or(0, |c| c.count))
    }

    /// Lexical (BM25 full-text) search over drawer content — the "basic
    /// working search path" this bootstrap establishes. Semantic and hybrid
    /// ranking are later phases layered on top of the same `drawer` table.
    pub async fn lexical_search(&self, query: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let sql = format!(
            "SELECT {DRAWER_COLUMNS}, search::score(1) AS score FROM drawer \
             WHERE content @1@ $query ORDER BY score DESC LIMIT $limit"
        );
        Ok(self
            .db
            .query(sql)
            .bind(("query", query.to_string()))
            .bind(("limit", limit))
            .await?
            .take(0)?)
    }
}
