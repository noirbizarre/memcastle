//! Graph-aware retrieval: linking drawers to entities and walking the links.
//!
//! The canonical memory is the drawer; the graph is derived from it. A
//! `mentions` edge says "this drawer talks about that entity", and
//! `relates_to` (see `entities`) says how entities relate. Expansion walks
//! `seed drawer -> mentions -> entity -> relates_to -> entity <- mentions <-
//! other drawer` entirely inside SurrealDB, so no graph engine is rebuilt in
//! Rust: Rust only folds the rows SurrealDB returns into a ranked list.
//!
//! Expansion adds drawers *next to* the direct hits and never replaces one,
//! and it only reads: deleting every edge leaves every drawer's content as it
//! was.

use std::collections::HashMap;

use chrono::Utc;
use serde::Deserialize;

use crate::domain::{Drawer, DrawerId, EntityId, SearchFilter, SearchHit, Signals};
use crate::error::Result;

use super::SurrealStore;

/// How much one entity shared *directly* with a seed counts toward a hit's
/// graph score.
const DIRECT_WEIGHT: f32 = 1.0;
/// How much an entity reached over one `relates_to` hop counts: a looser
/// connection than a shared mention, so it ranks below one.
const NEIGHBOUR_WEIGHT: f32 = 0.5;

/// One candidate row from the expansion query.
#[derive(Deserialize)]
struct Candidate {
    id: String,
    /// How many of its entities the drawer shares with a seed.
    direct: u32,
    /// How many of its entities are one `relates_to` hop from a seed's.
    near: u32,
    /// The entity names that connect it.
    via: Vec<String>,
}

impl SurrealStore {
    /// Record that `drawer` mentions `entity`. Idempotent: the unique
    /// `(in, out)` index means linking twice leaves one edge. Returns whether
    /// this call created the edge.
    pub async fn link_drawer_entity(&self, drawer: DrawerId, entity: EntityId) -> Result<bool> {
        let mut response = self
            .db
            .query(
                "LET $existing = (SELECT VALUE id FROM mentions \
                    WHERE in = type::record('drawer', $drawer) \
                      AND out = type::record('entity', $entity)); \
                 IF array::len($existing) = 0 { \
                    RELATE (type::record('drawer', $drawer))->mentions->(type::record('entity', $entity)) \
                      SET created_at = <datetime>$now; \
                    RETURN true; \
                 } ELSE { RETURN false; };",
            )
            .bind(("drawer", drawer.to_string()))
            .bind(("entity", entity.to_string()))
            .bind(("now", super::stored(Utc::now())))
            .await?
            .check()?;
        let created: Option<bool> = response.take(response.num_statements() - 1)?;
        Ok(created.unwrap_or(false))
    }

    /// The names of the entities `drawer` mentions, sorted.
    pub async fn list_drawer_entities(&self, drawer: DrawerId) -> Result<Vec<String>> {
        let mut response = self
            .db
            .query(
                "SELECT VALUE out.name FROM mentions \
                 WHERE in = type::record('drawer', $drawer)",
            )
            .bind(("drawer", drawer.to_string()))
            .await?;
        let mut names: Vec<String> = super::take_rows(&mut response, 0)?;
        names.sort();
        Ok(names)
    }

    /// Drawers related to `seeds` through the knowledge graph, best first,
    /// within `filter`, excluding the seeds themselves.
    ///
    /// A candidate shares an entity with a seed (a `mentions` edge to the same
    /// entity), or mentions an entity one currently-valid `relates_to` hop from
    /// one the seed mentions. Its `graph` signal is the weighted count of such
    /// entities and `via` names them. The same scope as every other leg
    /// (room, tags, source, validity of the *drawer*) is applied in the
    /// database before ranking and truncation, and `relates_to` edges are
    /// filtered to those valid at the filter's point in time.
    pub async fn expand_via_graph(
        &self,
        seeds: &[DrawerId],
        limit: u32,
        filter: &SearchFilter,
    ) -> Result<Vec<SearchHit>> {
        if seeds.is_empty() {
            return Ok(Vec::new());
        }
        let seed_ids: Vec<String> = seeds.iter().map(ToString::to_string).collect();
        // The same `$all_time`/`$at` the scope binds, so a `relates_to` edge is
        // judged at the same instant as the drawers.
        let edge_valid =
            "($all_time = true OR (valid_from <= <datetime>$at AND (!valid_to OR valid_to > $at)))";
        let sql = format!(
            "{prelude} \
             LET $seed_records = (SELECT VALUE id FROM drawer WHERE record::id(id) IN $seeds); \
             LET $direct = array::distinct((SELECT VALUE out FROM mentions WHERE in IN $seed_records)); \
             LET $forward = (SELECT VALUE out FROM relates_to WHERE in IN $direct AND {edge_valid}); \
             LET $backward = (SELECT VALUE in FROM relates_to WHERE out IN $direct AND {edge_valid}); \
             LET $near = array::complement(array::distinct(array::concat($forward, $backward)), $direct); \
             LET $reach = array::concat($direct, $near); \
             LET $candidates = array::complement( \
                array::distinct((SELECT VALUE in FROM mentions WHERE out IN $reach)), $seed_records); \
             SELECT record::id(id) AS id, \
                    array::len(array::intersect(->mentions->entity, $direct)) AS direct, \
                    array::len(array::intersect(->mentions->entity, $near)) AS near, \
                    ->mentions->entity[WHERE id IN $reach].name AS via \
             FROM drawer WHERE id IN $candidates AND {scope};",
            prelude = super::retrieval::SCOPE_PRELUDE,
            scope = super::retrieval::SCOPE_WHERE,
        );
        let mut response = super::retrieval::bind_scope(self.db.query(sql), filter)?
            .bind(("seeds", seed_ids))
            .await?;
        let count = response.num_statements();
        let rows: Vec<Candidate> = super::take_rows(&mut response, count - 1)?;

        let mut ranked: Vec<(String, f32, Vec<String>)> = rows
            .into_iter()
            .filter(|row| row.direct > 0 || row.near > 0)
            .map(|mut row| {
                // The count is by entity, so a drawer sharing two entities with
                // a seed outranks one sharing a single entity.
                let score = f32::from(u16::try_from(row.direct).unwrap_or(u16::MAX))
                    * DIRECT_WEIGHT
                    + f32::from(u16::try_from(row.near).unwrap_or(u16::MAX)) * NEIGHBOUR_WEIGHT;
                row.via.sort();
                row.via.dedup();
                (row.id, score, row.via)
            })
            .collect();
        // Best first; equal scores order by id so the result never depends on
        // the engine's iteration order.
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked.truncate(limit as usize);

        let ids: Vec<String> = ranked.iter().map(|(id, _, _)| id.clone()).collect();
        let mut drawers: HashMap<String, Drawer> = self.fetch_search_drawers(&ids).await?;
        Ok(ranked
            .into_iter()
            .filter_map(|(id, score, via)| {
                let drawer = drawers.remove(&id)?;
                Some(SearchHit {
                    drawer,
                    score,
                    signals: Signals {
                        graph: Some(score),
                        ..Signals::default()
                    },
                    via,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use super::*;
    use crate::domain::{
        NewRelationship, Provenance, RelationshipId, RoomId, Source, SourceKind, Temporal,
    };

    async fn store() -> SurrealStore {
        SurrealStore::connect_memory_for_tests().await
    }

    async fn room(store: &SurrealStore, wing: &str) -> RoomId {
        let wing = store.get_or_create_wing(wing, None).await.expect("wing");
        store
            .get_or_create_room(wing.id, "r", None)
            .await
            .expect("room")
            .id
    }

    async fn drawer(store: &SurrealStore, room: RoomId, content: &str) -> Drawer {
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
        // Valid well before any point in time a test asks about: a drawer
        // written now was not yet valid last week.
        drawer.valid_from = Utc::now() - Duration::days(30);
        store.create_drawer(&drawer).await.expect("create drawer");
        drawer
    }

    async fn entity(store: &SurrealStore, name: &str) -> EntityId {
        store
            .get_or_create_entity(name, "thing", json!({}))
            .await
            .expect("entity")
            .id
    }

    fn ids(hits: &[SearchHit]) -> Vec<DrawerId> {
        hits.iter().map(|h| h.drawer.id).collect()
    }

    #[tokio::test]
    async fn linking_a_drawer_to_an_entity_twice_leaves_one_edge() {
        let store = store().await;
        let r = room(&store, "w").await;
        let d = drawer(&store, r, "the harbour").await;
        let e = entity(&store, "harbour").await;

        assert!(store.link_drawer_entity(d.id, e).await.expect("first"));
        assert!(!store.link_drawer_entity(d.id, e).await.expect("second"));
        assert_eq!(
            store.list_drawer_entities(d.id).await.expect("entities"),
            ["harbour"]
        );
    }

    #[tokio::test]
    async fn drawers_sharing_an_entity_with_a_seed_are_found_and_the_seed_is_not_repeated() {
        let store = store().await;
        let r = room(&store, "w").await;
        let seed = drawer(&store, r, "Alice maintains the parser").await;
        let related = drawer(&store, r, "release notes mention parsing speedups").await;
        let unrelated = drawer(&store, r, "lunch menu").await;
        let alice = entity(&store, "alice").await;
        store.link_drawer_entity(seed.id, alice).await.unwrap();
        store.link_drawer_entity(related.id, alice).await.unwrap();

        let hits = store
            .expand_via_graph(&[seed.id], 10, &SearchFilter::default())
            .await
            .expect("expand");

        assert_eq!(ids(&hits), [related.id]);
        assert!(!ids(&hits).contains(&unrelated.id));
        assert_eq!(hits[0].via, ["alice"]);
        assert_eq!(hits[0].signals.graph, Some(1.0));
    }

    #[tokio::test]
    async fn a_relates_to_neighbour_is_reached_with_a_lower_weight_than_a_shared_entity() {
        let store = store().await;
        let r = room(&store, "w").await;
        let seed = drawer(&store, r, "Alice works on the parser").await;
        let shared = drawer(&store, r, "Alice again").await;
        let neighbour = drawer(&store, r, "the parser project plan").await;
        let alice = entity(&store, "alice").await;
        let parser = entity(&store, "parser").await;
        store.link_drawer_entity(seed.id, alice).await.unwrap();
        store.link_drawer_entity(shared.id, alice).await.unwrap();
        store
            .link_drawer_entity(neighbour.id, parser)
            .await
            .unwrap();
        store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from: alice,
                    to: parser,
                    predicate: "works_on".into(),
                    confidence: 1.0,
                },
                Utc::now() - Duration::days(1),
            )
            .await
            .expect("relationship");

        let hits = store
            .expand_via_graph(&[seed.id], 10, &SearchFilter::default())
            .await
            .expect("expand");

        assert_eq!(
            ids(&hits),
            [shared.id, neighbour.id],
            "direct first, neighbour second"
        );
        assert_eq!(hits[1].signals.graph, Some(0.5));
        assert_eq!(hits[1].via, ["parser"]);
    }

    #[tokio::test]
    async fn a_relationship_that_has_ended_is_not_followed_unless_history_is_requested() {
        let store = store().await;
        let r = room(&store, "w").await;
        let seed = drawer(&store, r, "Alice owned the parser").await;
        let neighbour = drawer(&store, r, "parser backlog").await;
        let alice = entity(&store, "alice").await;
        let parser = entity(&store, "parser").await;
        store.link_drawer_entity(seed.id, alice).await.unwrap();
        store
            .link_drawer_entity(neighbour.id, parser)
            .await
            .unwrap();
        let edge = RelationshipId::new();
        store
            .create_relationship(
                edge,
                NewRelationship {
                    from: alice,
                    to: parser,
                    predicate: "owns".into(),
                    confidence: 1.0,
                },
                Utc::now() - Duration::days(10),
            )
            .await
            .unwrap();
        store
            .invalidate_relationship(edge, Utc::now() - Duration::days(2))
            .await
            .unwrap();

        let current = store
            .expand_via_graph(&[seed.id], 10, &SearchFilter::default())
            .await
            .expect("current");
        assert!(
            current.is_empty(),
            "an ended relationship must not link: {current:?}"
        );

        let past = SearchFilter {
            temporal: Temporal::AsOf(Utc::now() - Duration::days(5)),
            ..SearchFilter::default()
        };
        let then = store
            .expand_via_graph(&[seed.id], 10, &past)
            .await
            .expect("as of");
        assert_eq!(ids(&then), [neighbour.id]);
    }

    #[tokio::test]
    async fn expansion_applies_the_scope_before_ranking() {
        let store = store().await;
        let mine = room(&store, "mine").await;
        let other = room(&store, "other").await;
        let seed = drawer(&store, mine, "seed").await;
        let near_mine = drawer(&store, mine, "in scope").await;
        let alice = entity(&store, "alice").await;
        store.link_drawer_entity(seed.id, alice).await.unwrap();
        store.link_drawer_entity(near_mine.id, alice).await.unwrap();
        // Out-of-scope drawers that would outrank the in-scope one (two shared
        // entities each) and fill a limit of 1.
        let bob = entity(&store, "bob").await;
        store.link_drawer_entity(seed.id, bob).await.unwrap();
        for i in 0..2 {
            let d = drawer(&store, other, &format!("elsewhere {i}")).await;
            store.link_drawer_entity(d.id, alice).await.unwrap();
            store.link_drawer_entity(d.id, bob).await.unwrap();
        }

        let filter = SearchFilter {
            wing: Some("mine".into()),
            ..SearchFilter::default()
        };
        let hits = store
            .expand_via_graph(&[seed.id], 1, &filter)
            .await
            .expect("expand");
        assert_eq!(ids(&hits), [near_mine.id]);
    }

    #[tokio::test]
    async fn expansion_leaves_canonical_drawer_content_untouched() {
        let store = store().await;
        let r = room(&store, "w").await;
        let seed = drawer(&store, r, "verbatim seed").await;
        let related = drawer(&store, r, "verbatim related").await;
        let e = entity(&store, "topic").await;
        store.link_drawer_entity(seed.id, e).await.unwrap();
        store.link_drawer_entity(related.id, e).await.unwrap();

        let hits = store
            .expand_via_graph(&[seed.id], 10, &SearchFilter::default())
            .await
            .expect("expand");

        assert_eq!(hits[0].drawer.content, related.content);
        assert_eq!(hits[0].drawer.content_hash, related.content_hash);
        let stored = store.get_drawer(related.id).await.unwrap().unwrap();
        assert_eq!(stored.content, related.content);
        assert_eq!(stored.updated_at, related.updated_at);
    }

    #[tokio::test]
    async fn a_superseded_drawer_is_not_surfaced_by_expansion() {
        let store = store().await;
        let r = room(&store, "w").await;
        let seed = drawer(&store, r, "seed").await;
        let stale = drawer(&store, r, "stale").await;
        let e = entity(&store, "topic").await;
        store.link_drawer_entity(seed.id, e).await.unwrap();
        store.link_drawer_entity(stale.id, e).await.unwrap();
        store
            .supersede_drawer(stale.id, None, Utc::now())
            .await
            .unwrap();

        let current = store
            .expand_via_graph(&[seed.id], 10, &SearchFilter::default())
            .await
            .expect("expand");
        assert!(current.is_empty(), "{current:?}");
    }

    #[tokio::test]
    async fn deleting_a_drawer_leaves_no_dangling_link_to_surface() {
        let store = store().await;
        let r = room(&store, "w").await;
        let seed = drawer(&store, r, "seed").await;
        let gone = drawer(&store, r, "gone").await;
        let e = entity(&store, "topic").await;
        store.link_drawer_entity(seed.id, e).await.unwrap();
        store.link_drawer_entity(gone.id, e).await.unwrap();
        store.delete_drawer(gone.id).await.unwrap();

        let hits = store
            .expand_via_graph(&[seed.id], 10, &SearchFilter::default())
            .await
            .expect("expand");
        assert!(hits.is_empty(), "{hits:?}");
    }
}
