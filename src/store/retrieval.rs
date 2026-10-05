//! Retrieval queries: lexical, vector and fused ranking over the `drawer` table.
//!
//! Every leg is one SurrealQL script, and every leg applies the *same* scope
//! predicate ([`SCOPE_WHERE`]) inside the database, before ranking and before
//! the result limit. Fetching an unscoped candidate page and filtering it in
//! Rust would let an out-of-scope but better-scoring drawer crowd a requested
//! scope's matches out of a capped result — the regression
//! `wing_scope_is_applied_by_surrealdb_before_the_result_limit` pins that for
//! lexical, and the vector tests below pin it for KNN.
//!
//! What Rust does here is small and explicit: it binds parameters, breaks
//! ties between equally ranked drawers by id (so an ordering never depends on
//! engine iteration order), and maps rows to the domain's [`SearchHit`].

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use surrealdb::engine::any::Any;
use surrealdb::method::Query;

use crate::domain::{
    Drawer, DrawerId, EMBEDDING_DIMENSION, SearchFilter, SearchHit, Signals, Temporal,
};
use crate::error::{Error, Result};

use super::SurrealStore;
use super::drawers::DRAWER_SEARCH_COLUMNS;

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

/// Resolves the wing/room *names* in the filter to room ids once, so each leg
/// can test `room IN $rooms` (an indexed lookup SurrealDB can use to prefilter
/// a vector search) instead of re-running nested subqueries per candidate.
///
/// `drawer` has no `wing` column and every foreign key is a plain string (see
/// `store::wings`), so the wing scope is two hops: wing name -> wings -> rooms.
pub(super) const SCOPE_PRELUDE: &str = "LET $rooms = (SELECT VALUE record::id(id) FROM room \
       WHERE ($wing = NULL OR wing IN (SELECT VALUE record::id(id) FROM wing WHERE name = $wing)) \
         AND ($room = NULL OR name = $room)); ";

/// The text of [`VALIDITY_WHERE`], as a macro so [`SCOPE_WHERE`] can `concat!`
/// it: a `const` string cannot be spliced into another at compile time, and a
/// second hand-copied predicate is how drawers and edges would drift apart.
macro_rules! validity_where {
    () => {
        "($all_time = true OR (valid_from < <datetime>$win_until \
         AND (!valid_to OR valid_to > $win_from)))"
    };
}

/// The temporal predicate, shared by drawers and `relates_to` edges so both are
/// always judged by the same rule: validity overlaps the window
/// `[$win_from, $win_until)` (see [`Temporal`]), or every record qualifies when
/// `$all_time` is set.
///
/// A point in time is the window `[t, t + 1ns)`, so as-of and interval queries
/// run this one clause and cannot drift apart. `valid_to` is an
/// `option<string>` holding fixed-width UTC text, so comparing it with a string
/// compares instants (ADR-005); truthiness (`!valid_to`) is the "still open"
/// test because `NONE` and `NULL` are both possible. `valid_from` is a native
/// datetime, so its bound is cast. No index backs it: it is a predicate beside
/// the full-text and vector lookups, not a second access path.
pub(super) const VALIDITY_WHERE: &str = validity_where!();

/// The scope every leg applies: room, tags, source kind and temporal validity.
///
/// - **Room**: unscoped when neither wing nor room is given.
/// - **Tags**: the drawer must carry all of them.
/// - **Temporal**: [`VALIDITY_WHERE`].
pub(super) const SCOPE_WHERE: &str = concat!(
    "(($wing = NULL AND $room = NULL) OR room IN $rooms) \
     AND ($tags = [] OR tags CONTAINSALL $tags) \
     AND ($source_kind = NULL OR source.kind = $source_kind) \
     AND ",
    validity_where!()
);

/// The window `[from, until)` a [`Temporal`] stands for, or `None` for all
/// time. `now` is passed in so a query reads the clock once.
fn window(temporal: Temporal, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    // One nanosecond: the finest instant SurrealDB stores, so `[t, t + 1ns)`
    // contains exactly `t`.
    let instant = |at: DateTime<Utc>| (at, at + Duration::nanoseconds(1));
    match temporal {
        Temporal::All => None,
        Temporal::Current => Some(instant(now)),
        Temporal::AsOf(at) => Some(instant(at)),
        Temporal::Between { from, until } => Some((from, until)),
    }
}

/// Bind every parameter [`SCOPE_PRELUDE`] and [`SCOPE_WHERE`] mention.
pub(super) fn bind_scope<'r>(
    query: Query<'r, Any>,
    filter: &SearchFilter,
) -> Result<Query<'r, Any>> {
    let now = Utc::now();
    let bounds = window(filter.temporal, now);
    // Under `All` the window is never read (`$all_time` short-circuits), but
    // every name must still be bound or the statement fails.
    let (from, until) = bounds.unwrap_or((now, now));
    Ok(query
        // `bindable`, not a native `Option`: a native `None` binds as `NONE`,
        // which `$wing = NULL` never matches (see `list_jobs`).
        .bind(("wing", super::bindable(&filter.wing)?))
        .bind(("room", super::bindable(&filter.room)?))
        .bind(("tags", filter.tags.clone()))
        .bind(("source_kind", super::bindable(&filter.source_kind)?))
        .bind(("all_time", bounds.is_none()))
        // Both as canonical text: `valid_to` is compared as a string, and the
        // `valid_from` side casts `$win_until` back to a datetime.
        .bind(("win_from", super::stored(from)))
        .bind(("win_until", super::stored(until))))
}

/// A drawer row plus the one ranking column a leg selected.
#[derive(Deserialize)]
struct RankedRow {
    #[serde(flatten)]
    drawer: Drawer,
    /// BM25 score or KNN distance, depending on the leg.
    rank: f32,
}

/// What `search::rrf` returns per fused candidate. A leg that did not match
/// leaves its field absent.
#[derive(Deserialize)]
struct FusedRow {
    id: String,
    rrf_score: f32,
    #[serde(default)]
    distance: Option<f32>,
    #[serde(default)]
    ft_score: Option<f32>,
}

/// The k-nearest-neighbour operator for a `limit`-sized result.
///
/// `k` and `ef` are interpolated because the operator takes literals, not
/// parameters; both are integers computed here, so nothing caller-supplied
/// reaches the query text. `ef` (the search beam) is wider than `k` so the
/// approximate search stays accurate when the scope discards candidates.
fn knn_operator(k: u32) -> String {
    let ef = k.saturating_mul(4).max(64);
    format!("<|{k},{ef}|>")
}

/// Reject a vector the index would refuse, with a diagnostic that names the
/// expected length instead of the engine's internal message.
pub(crate) fn check_dimension(embedding: &[f32]) -> Result<()> {
    if embedding.len() == EMBEDDING_DIMENSION {
        Ok(())
    } else {
        Err(Error::EmbeddingDimension {
            expected: EMBEDDING_DIMENSION,
            actual: embedding.len(),
        })
    }
}

impl SurrealStore {
    /// Lexical (BM25 full-text) search over drawer content, best first.
    ///
    /// `mode` picks whether every query term must match or any one will (see
    /// [`MatchMode`]); the `search` module decides which to try. Ties break
    /// on drawer id.
    pub async fn search_lexical(
        &self,
        query: &str,
        limit: u32,
        filter: &SearchFilter,
        mode: MatchMode,
    ) -> Result<Vec<SearchHit>> {
        let operator = mode.operator();
        let sql = format!(
            "{SCOPE_PRELUDE} \
             SELECT {DRAWER_SEARCH_COLUMNS}, search::score(1) AS rank FROM drawer \
             WHERE content {operator} $query AND {SCOPE_WHERE} \
             ORDER BY rank DESC, id ASC LIMIT $limit"
        );
        let mut response = bind_scope(self.db.query(sql), filter)?
            .bind(("query", query.to_string()))
            .bind(("limit", limit))
            .await?;
        let rows: Vec<RankedRow> = super::take_rows(&mut response, 1)?;
        Ok(rows
            .into_iter()
            .map(|row| SearchHit {
                drawer: row.drawer,
                score: row.rank,
                signals: Signals {
                    lexical: Some(row.rank),
                    ..Signals::default()
                },
                via: Vec::new(),
            })
            .collect())
    }

    /// Vector search: the drawers nearest `embedding` by cosine similarity,
    /// most similar first, within `filter`.
    ///
    /// The scope sits in the same `WHERE` as the KNN operator, so SurrealDB
    /// filters *during* the index traversal and still returns `limit`
    /// in-scope neighbours; a post-filter would return fewer, or none, when
    /// the globally nearest drawers are out of scope. Drawers without an
    /// embedding are not in the index and never appear.
    pub async fn search_vector(
        &self,
        embedding: &[f32],
        limit: u32,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchHit>> {
        check_dimension(embedding)?;
        let knn = knn_operator(limit);
        let sql = format!(
            "{SCOPE_PRELUDE} \
             SELECT {DRAWER_SEARCH_COLUMNS}, vector::distance::knn() AS rank FROM drawer \
             WHERE embedding {knn} $embedding AND {SCOPE_WHERE} \
             ORDER BY rank ASC, id ASC LIMIT $limit"
        );
        let mut response = bind_scope(self.db.query(sql), filter)?
            .bind(("embedding", embedding.to_vec()))
            .bind(("limit", limit))
            .await?;
        let rows: Vec<RankedRow> = super::take_rows(&mut response, 1)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                // Cosine *distance* is 0 for identical vectors; a similarity
                // is the form where bigger means better, like every other score.
                let similarity = 1.0 - row.rank;
                SearchHit {
                    drawer: row.drawer,
                    score: similarity,
                    signals: Signals {
                        semantic: Some(similarity),
                        ..Signals::default()
                    },
                    via: Vec::new(),
                }
            })
            .collect())
    }

    /// Hybrid search: the lexical and vector legs fused by reciprocal rank.
    ///
    /// Both legs run, scoped identically, in one SurrealQL script and are
    /// merged by SurrealDB's own `search::rrf` (constant 60), so the fusion
    /// is the database's rather than a Rust re-implementation. Reciprocal
    /// rank fusion uses ranks, not raw scores, which is what makes a BM25
    /// score and a cosine distance combinable at all. Rust then orders by
    /// `(fused score desc, id asc)` — deterministic even when two drawers tie
    /// — and fetches only the winners' rows.
    pub async fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        limit: u32,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchHit>> {
        check_dimension(embedding)?;
        // Each leg contributes more candidates than the result needs, so a
        // drawer ranked mid-table by one leg but high by the other can still
        // surface.
        let pool = limit.saturating_mul(2).max(20);
        let knn = knn_operator(pool);
        let sql = format!(
            "{SCOPE_PRELUDE} \
             LET $semantic = (SELECT record::id(id) AS id, vector::distance::knn() AS distance \
                FROM drawer WHERE embedding {knn} $embedding AND {SCOPE_WHERE} \
                ORDER BY distance ASC, id ASC LIMIT $pool); \
             LET $lexical = (SELECT record::id(id) AS id, search::score(1) AS ft_score \
                FROM drawer WHERE content @1,OR@ $query AND {SCOPE_WHERE} \
                ORDER BY ft_score DESC, id ASC LIMIT $pool); \
             RETURN search::rrf([$semantic, $lexical], $fuse_limit, 60);"
        );
        let mut response = bind_scope(self.db.query(sql), filter)?
            .bind(("embedding", embedding.to_vec()))
            .bind(("query", query.to_string()))
            .bind(("pool", pool))
            // Large enough to keep every candidate: truncating inside
            // `rrf` would cut ties in an order Rust cannot control.
            .bind(("fuse_limit", pool.saturating_mul(2)))
            .await?;
        let mut fused: Vec<FusedRow> = super::take_rows(&mut response, 3)?;
        // The deterministic tie-break: equal fused scores order by id.
        fused.sort_by(|a, b| {
            b.rrf_score
                .total_cmp(&a.rrf_score)
                .then_with(|| a.id.cmp(&b.id))
        });
        fused.truncate(limit as usize);

        let ids: Vec<String> = fused.iter().map(|row| row.id.clone()).collect();
        let mut drawers = self.fetch_search_drawers(&ids).await?;
        Ok(fused
            .into_iter()
            .filter_map(|row| {
                // A drawer deleted between the two queries simply drops out.
                let drawer = drawers.remove(&row.id)?;
                Some(SearchHit {
                    drawer,
                    score: row.rrf_score,
                    signals: Signals {
                        lexical: row.ft_score,
                        semantic: row.distance.map(|d| 1.0 - d),
                        graph: None,
                    },
                    via: Vec::new(),
                })
            })
            .collect())
    }

    /// The drawers with these ids, keyed by id, without their embeddings.
    pub(crate) async fn fetch_search_drawers(
        &self,
        ids: &[String],
    ) -> Result<HashMap<String, Drawer>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let sql =
            format!("SELECT {DRAWER_SEARCH_COLUMNS} FROM drawer WHERE record::id(id) IN $ids");
        let mut response = self.db.query(sql).bind(("ids", ids.to_vec())).await?;
        let drawers: Vec<Drawer> = super::take_rows(&mut response, 0)?;
        Ok(drawers
            .into_iter()
            .map(|drawer| (drawer.id.to_string(), drawer))
            .collect())
    }

    /// Store `embedding` for an existing drawer, touching nothing else.
    ///
    /// The embedding is derived data: this updates that one field and leaves
    /// `content`, `content_hash` and every timestamp as written, so the
    /// canonical memory is byte-identical before and after. Returns whether
    /// the drawer existed.
    pub async fn set_drawer_embedding(&self, id: DrawerId, embedding: &[f32]) -> Result<bool> {
        check_dimension(embedding)?;
        // Retried: the HNSW index entry is rewritten by this update, and a
        // background task maintains that index too (see `retrying_on_conflict`).
        let mut response = super::retrying_on_conflict(|| async {
            super::checked(
                self.db
                    .query(
                        // `WHERE`, not `UPDATE type::record(..)`: updating a record id
                        // directly *creates* it when absent, which would conjure a
                        // half-formed drawer for one deleted since it was listed.
                        "UPDATE drawer SET embedding = $embedding \
                         WHERE id = type::record('drawer', $id) RETURN record::id(id) AS id",
                    )
                    .bind(("id", id.to_string()))
                    .bind(("embedding", embedding.to_vec()))
                    .await?,
            )
        })
        .await?;
        let rows: Vec<serde_json::Value> = response.take(0)?;
        Ok(!rows.is_empty())
    }

    /// Up to `limit` drawers that have no embedding yet, oldest first, for the
    /// embedding sweep. Oldest first so a sweep interrupted partway resumes
    /// where it stopped rather than starving the backlog.
    pub async fn list_drawers_without_embedding(
        &self,
        wing: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Drawer>> {
        let sql = format!(
            "SELECT {DRAWER_SEARCH_COLUMNS} FROM drawer \
             WHERE embedding = NONE \
               AND ($wing = NULL OR room IN ( \
                     SELECT VALUE record::id(id) FROM room WHERE wing IN ( \
                       SELECT VALUE record::id(id) FROM wing WHERE name = $wing))) \
             ORDER BY created_at ASC, id ASC LIMIT $limit"
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
    use chrono::{Duration, Utc};

    use super::*;
    use crate::domain::{Provenance, RoomId, SearchFilter, Source, SourceKind, Temporal};

    /// A unit vector along `axis`: two of them are orthogonal (distance 1),
    /// the same axis is identical (distance 0), so KNN geometry is exact.
    fn axis(axis: usize) -> Vec<f32> {
        let mut v = vec![0.0; EMBEDDING_DIMENSION];
        v[axis] = 1.0;
        v
    }

    /// A vector leaning `weight` towards axis 0 and the rest towards `other`.
    fn mix(weight: f32, other: usize) -> Vec<f32> {
        let mut v = vec![0.0; EMBEDDING_DIMENSION];
        v[0] = weight;
        v[other] = 1.0 - weight;
        v
    }

    struct Spec<'a> {
        content: &'a str,
        tags: &'a [&'a str],
        kind: SourceKind,
        embedding: Option<Vec<f32>>,
    }

    fn spec<'a>(content: &'a str, embedding: Option<Vec<f32>>) -> Spec<'a> {
        Spec {
            content,
            tags: &[],
            kind: SourceKind::Manual,
            embedding,
        }
    }

    async fn store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    async fn room(store: &SurrealStore, wing: &str, room: &str) -> RoomId {
        let wing = store.get_or_create_wing(wing, None).await.expect("wing");
        store
            .get_or_create_room(wing.id, room, None)
            .await
            .expect("room")
            .id
    }

    async fn add(store: &SurrealStore, room: RoomId, spec: Spec<'_>) -> Drawer {
        let mut drawer = Drawer::new(
            DrawerId::new(),
            room,
            spec.content.to_string(),
            Source {
                kind: spec.kind,
                uri: None,
                agent: None,
                origin: None,
            },
            spec.tags.iter().map(|t| (*t).to_string()).collect(),
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        );
        drawer.embedding = spec.embedding;
        store.create_drawer(&drawer).await.expect("create drawer");
        drawer
    }

    fn contents(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.drawer.content.as_str()).collect()
    }

    #[tokio::test]
    async fn vector_search_returns_the_nearest_drawers_most_similar_first() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        add(&store, r, spec("exact", Some(axis(0)))).await;
        add(&store, r, spec("close", Some(mix(0.9, 1)))).await;
        add(&store, r, spec("far", Some(axis(5)))).await;

        let hits = store
            .search_vector(&axis(0), 3, &SearchFilter::default())
            .await
            .expect("vector search");

        assert_eq!(contents(&hits), ["exact", "close", "far"]);
        assert!(
            (hits[0].score - 1.0).abs() < 1e-4,
            "identical vector has similarity 1"
        );
        assert_eq!(hits[0].signals.semantic, Some(hits[0].score));
        assert!(hits[1].score > hits[2].score);
    }

    #[tokio::test]
    async fn a_drawer_without_an_embedding_is_never_a_vector_hit() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        add(&store, r, spec("embedded", Some(axis(0)))).await;
        add(&store, r, spec("bare", None)).await;

        let hits = store
            .search_vector(&axis(0), 10, &SearchFilter::default())
            .await
            .expect("vector search");

        assert_eq!(contents(&hits), ["embedded"]);
    }

    #[tokio::test]
    async fn a_search_hit_never_carries_the_embedding_vector() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        add(&store, r, spec("x", Some(axis(0)))).await;

        let hits = store
            .search_vector(&axis(0), 1, &SearchFilter::default())
            .await
            .expect("vector search");

        assert!(
            hits[0].drawer.embedding.is_none(),
            "vectors must not go over the wire"
        );
    }

    #[tokio::test]
    async fn a_vector_of_the_wrong_length_is_refused_with_the_expected_length() {
        let store = store().await;
        let error = store
            .search_vector(&[1.0, 2.0], 3, &SearchFilter::default())
            .await
            .expect_err("wrong length");
        assert!(
            matches!(
                error,
                Error::EmbeddingDimension {
                    expected: 768,
                    actual: 2
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn the_wing_scope_is_applied_before_the_nearest_neighbour_limit() {
        // The three globally nearest drawers are all in "other"; a post-filter
        // over the top 2 would leave "mine" with nothing.
        let store = store().await;
        let other = room(&store, "other", "r").await;
        let mine = room(&store, "mine", "r").await;
        for i in 0..3 {
            add(&store, other, spec(&format!("other {i}"), Some(axis(0)))).await;
        }
        add(&store, mine, spec("mine near", Some(mix(0.6, 1)))).await;
        add(&store, mine, spec("mine far", Some(axis(7)))).await;

        let filter = SearchFilter {
            wing: Some("mine".into()),
            ..SearchFilter::default()
        };
        let hits = store
            .search_vector(&axis(0), 2, &filter)
            .await
            .expect("scoped vector search");

        assert_eq!(contents(&hits), ["mine near", "mine far"]);
    }

    #[tokio::test]
    async fn the_room_scope_restricts_a_vector_search_to_that_room() {
        let store = store().await;
        let a = room(&store, "w", "a").await;
        let b = room(&store, "w", "b").await;
        add(&store, a, spec("in a", Some(axis(0)))).await;
        add(&store, b, spec("in b", Some(axis(0)))).await;

        let filter = SearchFilter {
            room: Some("b".into()),
            ..SearchFilter::default()
        };
        let hits = store
            .search_vector(&axis(0), 5, &filter)
            .await
            .expect("search");
        assert_eq!(contents(&hits), ["in b"]);
    }

    #[tokio::test]
    async fn tag_and_source_kind_filters_are_applied_before_the_nearest_neighbour_limit() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        // Nearer drawers lack the tag or have the wrong source; the wanted
        // one is the farthest, so only a database-side prefilter finds it at k=1.
        add(&store, r, spec("untagged near", Some(axis(0)))).await;
        add(
            &store,
            r,
            Spec {
                content: "tagged file",
                tags: &["keep", "extra"],
                kind: SourceKind::File,
                embedding: Some(axis(9)),
            },
        )
        .await;
        add(
            &store,
            r,
            Spec {
                content: "tagged manual",
                tags: &["keep"],
                kind: SourceKind::Manual,
                embedding: Some(mix(0.5, 3)),
            },
        )
        .await;

        let tagged = SearchFilter {
            tags: vec!["keep".into(), "extra".into()],
            ..SearchFilter::default()
        };
        let hits = store
            .search_vector(&axis(0), 1, &tagged)
            .await
            .expect("tags");
        assert_eq!(
            contents(&hits),
            ["tagged file"],
            "every listed tag is required"
        );

        let manual_tagged = SearchFilter {
            tags: vec!["keep".into()],
            source_kind: Some(SourceKind::Manual),
            ..SearchFilter::default()
        };
        let hits = store
            .search_vector(&axis(0), 1, &manual_tagged)
            .await
            .expect("tags and source");
        assert_eq!(contents(&hits), ["tagged manual"]);
    }

    /// A drawer valid from `from` until `to` (open-ended when `None`).
    async fn add_valid(
        store: &SurrealStore,
        room: RoomId,
        content: &str,
        embedding: Option<Vec<f32>>,
        from: chrono::DateTime<Utc>,
        to: Option<chrono::DateTime<Utc>>,
    ) {
        // Built with its validity from the start and written once: creating,
        // deleting and re-creating one id loses a write conflict against index
        // maintenance on some platforms (seen on Windows CI).
        let mut drawer = Drawer::new(
            DrawerId::new(),
            room,
            content.to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            vec![],
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        );
        drawer.valid_from = from;
        drawer.valid_to = to;
        drawer.embedding = embedding;
        store.create_drawer(&drawer).await.expect("create drawer");
    }

    #[tokio::test]
    async fn current_search_excludes_superseded_and_future_drawers_and_all_time_includes_them() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let now = Utc::now();
        add_valid(
            &store,
            r,
            "current memory",
            Some(axis(0)),
            now - Duration::days(10),
            None,
        )
        .await;
        add_valid(
            &store,
            r,
            "superseded memory",
            Some(axis(0)),
            now - Duration::days(30),
            Some(now - Duration::days(5)),
        )
        .await;
        add_valid(
            &store,
            r,
            "future memory",
            Some(axis(0)),
            now + Duration::days(5),
            None,
        )
        .await;

        let current = store
            .search_vector(&axis(0), 10, &SearchFilter::default())
            .await
            .expect("current");
        assert_eq!(contents(&current), ["current memory"]);

        let all = SearchFilter {
            temporal: Temporal::All,
            ..SearchFilter::default()
        };
        let mut every = contents(&store.search_vector(&axis(0), 10, &all).await.expect("all"))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        every.sort();
        assert_eq!(
            every,
            ["current memory", "future memory", "superseded memory"]
        );
    }

    #[tokio::test]
    async fn a_point_in_time_search_sees_the_memory_valid_then_with_an_exclusive_end() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let now = Utc::now();
        let boundary = now - Duration::days(5);
        add_valid(
            &store,
            r,
            "old truth",
            Some(axis(0)),
            now - Duration::days(30),
            Some(boundary),
        )
        .await;
        add_valid(&store, r, "new truth", Some(axis(0)), boundary, None).await;

        let at = |t| SearchFilter {
            temporal: Temporal::AsOf(t),
            ..SearchFilter::default()
        };
        let before = store
            .search_vector(&axis(0), 10, &at(boundary - Duration::seconds(1)))
            .await
            .expect("before");
        assert_eq!(contents(&before), ["old truth"]);

        // At the boundary the old record is gone and the new one has begun:
        // exactly one of them, never both and never neither.
        let exactly = store
            .search_vector(&axis(0), 10, &at(boundary))
            .await
            .expect("at boundary");
        assert_eq!(contents(&exactly), ["new truth"]);
    }

    #[tokio::test]
    async fn lexical_search_honours_the_same_temporal_and_tag_scope() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let now = Utc::now();
        add_valid(
            &store,
            r,
            "castle ledger live",
            None,
            now - Duration::days(1),
            None,
        )
        .await;
        add_valid(
            &store,
            r,
            "castle ledger retired",
            None,
            now - Duration::days(9),
            Some(now - Duration::days(2)),
        )
        .await;
        for i in 0..6 {
            add(&store, r, spec(&format!("unrelated filler {i}"), None)).await;
        }

        let current = store
            .search_lexical(
                "castle ledger",
                10,
                &SearchFilter::default(),
                MatchMode::All,
            )
            .await
            .expect("current");
        assert_eq!(contents(&current), ["castle ledger live"]);

        let all = SearchFilter {
            temporal: Temporal::All,
            ..SearchFilter::default()
        };
        let both = store
            .search_lexical("castle ledger", 10, &all, MatchMode::All)
            .await
            .expect("all");
        assert_eq!(both.len(), 2);

        let tagged = SearchFilter {
            tags: vec!["nope".into()],
            ..SearchFilter::default()
        };
        assert!(
            store
                .search_lexical("castle ledger", 10, &tagged, MatchMode::All)
                .await
                .expect("tagged")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn hybrid_search_fuses_both_legs_and_a_drawer_strong_in_both_wins() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        // "both" matches the words and the vector; the others match one leg only.
        add(&store, r, spec("moat drawbridge both", Some(axis(0)))).await;
        add(&store, r, spec("moat only words", Some(axis(40)))).await;
        add(&store, r, spec("nothing alike", Some(mix(0.9, 1)))).await;
        for i in 0..6 {
            add(
                &store,
                r,
                spec(&format!("unrelated filler {i}"), Some(axis(100 + i))),
            )
            .await;
        }

        let hits = store
            .search_hybrid("moat drawbridge", &axis(0), 3, &SearchFilter::default())
            .await
            .expect("hybrid");

        assert_eq!(hits[0].drawer.content, "moat drawbridge both");
        assert!(hits[0].signals.lexical.is_some() && hits[0].signals.semantic.is_some());
        let names = contents(&hits);
        assert!(
            names.contains(&"moat only words"),
            "a lexical-only hit still surfaces: {names:?}"
        );
        assert!(
            names.contains(&"nothing alike"),
            "a semantic-only hit still surfaces: {names:?}"
        );
    }

    #[tokio::test]
    async fn hybrid_ordering_is_deterministic_even_when_scores_tie() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        // Identical vectors and identical text: every score ties, so only the
        // id tie-break can order them.
        for _ in 0..6 {
            add(&store, r, spec("tie word", Some(axis(0)))).await;
        }
        for i in 0..6 {
            add(
                &store,
                r,
                spec(&format!("unrelated filler {i}"), Some(axis(50 + i))),
            )
            .await;
        }

        let run = || async {
            store
                .search_hybrid("tie", &axis(0), 6, &SearchFilter::default())
                .await
                .expect("hybrid")
                .into_iter()
                .map(|h| h.drawer.id.to_string())
                .collect::<Vec<_>>()
        };
        let first = run().await;
        assert_eq!(first.len(), 6);
        for _ in 0..4 {
            assert_eq!(
                run().await,
                first,
                "the same query must rank identically every time"
            );
        }
        let mut sorted = first.clone();
        sorted.sort();
        assert_eq!(first, sorted, "equal scores fall back to ascending id");
    }

    #[tokio::test]
    async fn hybrid_search_respects_the_scope_in_both_legs() {
        let store = store().await;
        let other = room(&store, "other", "r").await;
        let mine = room(&store, "mine", "r").await;
        add(&store, other, spec("moat outside", Some(axis(0)))).await;
        add(&store, mine, spec("moat inside", Some(axis(0)))).await;
        for i in 0..6 {
            add(
                &store,
                mine,
                spec(&format!("unrelated filler {i}"), Some(axis(60 + i))),
            )
            .await;
        }

        let filter = SearchFilter {
            wing: Some("mine".into()),
            ..SearchFilter::default()
        };
        let hits = store
            .search_hybrid("moat", &axis(0), 5, &filter)
            .await
            .expect("hybrid");
        assert!(
            hits.iter().all(|h| h.drawer.room == mine),
            "no hit may come from outside the wing: {:?}",
            contents(&hits)
        );
        assert_eq!(hits[0].drawer.content, "moat inside");
    }

    #[tokio::test]
    async fn storing_an_embedding_changes_nothing_but_the_embedding() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let drawer = add(&store, r, spec("canonical words, verbatim.", None)).await;

        let stored = store
            .set_drawer_embedding(drawer.id, &axis(3))
            .await
            .expect("set embedding");
        assert!(stored);

        let after = store
            .list_drawers(Some(r))
            .await
            .expect("list")
            .into_iter()
            .next()
            .expect("the drawer");
        assert_eq!(after.content, drawer.content);
        assert_eq!(after.content_hash, drawer.content_hash);
        assert_eq!(after.created_at, drawer.created_at);
        assert_eq!(after.updated_at, drawer.updated_at);
        assert_eq!(after.valid_from, drawer.valid_from);
        assert_eq!(after.embedding.as_deref(), Some(axis(3).as_slice()));
    }

    #[tokio::test]
    async fn setting_the_embedding_of_a_missing_drawer_creates_nothing() {
        let store = store().await;
        let stored = store
            .set_drawer_embedding(DrawerId::new(), &axis(1))
            .await
            .expect("no error for a missing drawer");
        assert!(!stored);
        assert_eq!(store.count_drawers().await.expect("count"), 0);
    }

    #[tokio::test]
    async fn the_embedding_sweep_lists_only_unembedded_drawers_oldest_first() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let first = add(&store, r, spec("first", None)).await;
        add(&store, r, spec("done", Some(axis(0)))).await;
        let last = add(&store, r, spec("last", None)).await;

        let pending = store
            .list_drawers_without_embedding(None, 10)
            .await
            .expect("pending");
        let ids: Vec<_> = pending.iter().map(|d| d.id).collect();
        assert_eq!(ids, [first.id, last.id]);
    }

    #[tokio::test]
    async fn superseding_a_drawer_closes_it_opens_the_replacement_and_keeps_history() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let mut old = Drawer::new(
            DrawerId::new(),
            r,
            "the deploy target is staging".to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            vec![],
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        )
        .with_name(Some("deploy".to_string()));
        old.valid_from = Utc::now() - Duration::days(3);
        store.create_drawer(&old).await.expect("create");

        let at = Utc::now() - Duration::days(1);
        let mut replacement = Drawer::new(
            DrawerId::new(),
            r,
            "the deploy target is production".to_string(),
            Source {
                kind: SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            vec![],
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        )
        .with_name(Some("deploy".to_string()));
        replacement.valid_from = at;

        let closed = store
            .supersede_drawer(old.id, Some(&replacement), at)
            .await
            .expect("supersede");
        assert!(closed);

        // The old drawer is untouched except for its validity and its name.
        let kept = store
            .get_drawer(old.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(kept.content, old.content);
        assert_eq!(kept.content_hash, old.content_hash);
        assert_eq!(kept.valid_to, Some(at));
        assert_eq!(kept.name, None, "the replacement takes the unique name");
        let named = store
            .get_drawer_by_name(r, "deploy")
            .await
            .expect("by name");
        assert_eq!(named.map(|d| d.id), Some(replacement.id));

        let filter = |temporal| SearchFilter {
            temporal,
            ..SearchFilter::default()
        };
        let now = store
            .search_lexical(
                "deploy target",
                10,
                &filter(Temporal::Current),
                MatchMode::All,
            )
            .await
            .expect("current");
        assert_eq!(contents(&now), ["the deploy target is production"]);
        let then = store
            .search_lexical(
                "deploy target",
                10,
                &filter(Temporal::AsOf(at - Duration::hours(1))),
                MatchMode::All,
            )
            .await
            .expect("as of");
        assert_eq!(contents(&then), ["the deploy target is staging"]);
    }

    #[tokio::test]
    async fn superseding_without_a_replacement_only_closes_the_drawer() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let drawer = add(&store, r, spec("a belief later retracted", None)).await;

        let closed = store
            .supersede_drawer(drawer.id, None, Utc::now())
            .await
            .expect("invalidate");
        assert!(closed);
        assert_eq!(store.count_drawers().await.expect("count"), 1);
        let kept = store
            .get_drawer(drawer.id)
            .await
            .expect("get")
            .expect("exists");
        assert!(kept.valid_to.is_some());
        assert_eq!(kept.content, "a belief later retracted");
    }

    #[tokio::test]
    async fn superseding_a_closed_or_missing_drawer_writes_nothing() {
        let store = store().await;
        let r = room(&store, "w", "r").await;
        let drawer = add(&store, r, spec("once", None)).await;
        assert!(
            store
                .supersede_drawer(drawer.id, None, Utc::now())
                .await
                .expect("first")
        );

        let replacement = add(&store, r, spec("placeholder", None)).await;
        store.delete_drawer(replacement.id).await.expect("delete");
        let again = store
            .supersede_drawer(drawer.id, Some(&replacement), Utc::now())
            .await
            .expect("second");
        assert!(!again, "an already closed drawer cannot be closed again");
        assert!(
            store
                .get_drawer(replacement.id)
                .await
                .expect("get")
                .is_none(),
            "a refused supersession must not create its replacement"
        );

        let missing = store
            .supersede_drawer(DrawerId::new(), None, Utc::now())
            .await
            .expect("missing");
        assert!(!missing);
    }

    #[test]
    fn the_declared_dimension_matches_the_schema_index() {
        // `DIMENSION` lives in the schema file because Rust may not carry DDL;
        // this keeps the constant callers validate against equal to it.
        let schema = include_str!("../../database/schema/palace.surql");
        assert!(
            schema.contains(&format!("HNSW DIMENSION {EMBEDDING_DIMENSION} ")),
            "palace.surql must declare an HNSW index of {EMBEDDING_DIMENSION} dimensions"
        );
    }
}
