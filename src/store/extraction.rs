//! Repository methods for the extraction job: which drawers still need reading, and closing the facts whose
//! evidence has gone.
//!
//! Extraction is derived data (docs/adr/024). Nothing here writes the `drawer` table: the marker lives in
//! `drawer_extraction`, beside it.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::domain::{Drawer, DrawerId, JobId};
use crate::error::Result;

use super::SurrealStore;
use super::drawers::DRAWER_SEARCH_COLUMNS;

impl SurrealStore {
    /// Up to `limit` drawers the extraction job has not read yet, oldest first.
    ///
    /// Only drawers that are still current and that came through a mining source (they carry `source.origin`) or were
    /// captured as a note (`source.kind = 'note'`): extraction consumes what the unified Source model filed and what a
    /// person wrote down on purpose, and a superseded drawer is history, not something to learn new facts from.
    /// A note has no origin because there is no document to cut it from, and without this clause a note would be
    /// searchable but never enriched. Oldest first so an interrupted sweep resumes where it stopped.
    pub async fn list_drawers_pending_extraction(
        &self,
        wing: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Drawer>> {
        let sql = format!(
            "SELECT {DRAWER_SEARCH_COLUMNS} FROM drawer \
             WHERE !valid_to AND (source.origin != NONE OR source.kind = 'note') \
               AND record::id(id) NOT IN (SELECT VALUE drawer FROM drawer_extraction) \
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

    /// Record that `drawer` has been read, so no later sweep sends it again.
    ///
    /// Written last, after the entities, links and edges it produced: a crash before it replays the drawer, which is
    /// safe because every one of those writes is idempotent. A drawer that yielded nothing is marked too.
    pub async fn mark_drawer_extracted(
        &self,
        drawer: DrawerId,
        extractor: &str,
        entities: u32,
        relations: u32,
        job: Option<JobId>,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.db
            .query(
                "UPSERT type::record('drawer_extraction', $drawer) SET \
                 drawer = $drawer, extractor = $extractor, entities = $entities, relations = $relations, \
                 job = $job, extracted_at = <datetime>$at",
            )
            .bind(("drawer", drawer.to_string()))
            .bind(("extractor", extractor.to_string()))
            .bind(("entities", entities))
            .bind(("relations", relations))
            .bind(("job", job.map(|job| job.to_string())))
            .bind(("at", super::stored(at)))
            .await?
            .check()?;
        Ok(())
    }

    /// Whether the extraction job has read `drawer`.
    pub async fn drawer_extracted(&self, drawer: DrawerId) -> Result<bool> {
        let mut response = self
            .db
            .query("SELECT VALUE drawer FROM drawer_extraction WHERE id = type::record('drawer_extraction', $drawer)")
            .bind(("drawer", drawer.to_string()))
            .await?;
        let rows: Vec<String> = super::take_rows(&mut response, 0)?;
        Ok(!rows.is_empty())
    }

    /// Close every open extracted fact whose evidence drawer has been superseded or retired, at the instant the
    /// drawer stopped being current. Returns how many edges were closed.
    ///
    /// A fact read from a drawer is only as true as that drawer: when a re-mine replaces the drawer, the fact stops
    /// being current (its history stays queryable) and the replacement is extracted afresh. Facts somebody asserted
    /// directly carry no provenance and are never touched. Idempotent: an edge already closed is not an open one.
    pub async fn close_facts_of_retired_drawers(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Retired {
            id: String,
            valid_to: String,
        }
        // Retired drawers that still have an open extracted edge, found through the provenance index.
        let mut response = self
            .db
            .query(
                "SELECT record::id(id) AS id, valid_to FROM drawer \
                 WHERE valid_to AND record::id(id) IN ( \
                     SELECT VALUE provenance.drawer FROM relates_to WHERE !valid_to AND provenance != NONE)",
            )
            .await?;
        let retired: Vec<Retired> = super::take_rows(&mut response, 0)?;

        let mut closed = 0u64;
        for drawer in retired {
            let mut response = self
                .db
                .query(
                    "UPDATE relates_to SET valid_to = $valid_to \
                     WHERE !valid_to AND provenance.drawer = $drawer RETURN VALUE record::id(id)",
                )
                .bind(("valid_to", drawer.valid_to))
                .bind(("drawer", drawer.id))
                .await?;
            let ids: Vec<String> = super::take_rows(&mut response, 0)?;
            closed += ids.len() as u64;
        }
        Ok(closed)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use super::*;
    use crate::domain::{
        FactProvenance, NewRelationship, Origin, Provenance, RelationshipId, RoomId, Source,
        SourceId, SourceKind,
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

    fn origin() -> Origin {
        Origin {
            source_id: SourceId::new(),
            source: "directory".into(),
            document: "a.md".into(),
            chunk: 0,
            revision: "rev1".into(),
        }
    }

    async fn drawer(store: &SurrealStore, room: RoomId, content: &str, mined: bool) -> Drawer {
        let mut source = Source::new(SourceKind::File, None, None);
        if mined {
            source.origin = Some(origin());
        }
        let mut drawer = Drawer::new(
            DrawerId::new(),
            room,
            content.to_string(),
            source,
            vec![],
            Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        );
        drawer.valid_from = Utc::now() - Duration::days(30);
        store.create_drawer(&drawer).await.expect("create drawer");
        drawer
    }

    fn provenance(drawer: &Drawer) -> FactProvenance {
        FactProvenance {
            drawer: drawer.id,
            origin: drawer.source.origin.clone(),
            job_id: None,
            extractor: "test".into(),
            extracted_at: Utc::now(),
        }
    }

    async fn edge(store: &SurrealStore, evidence: &Drawer) -> RelationshipId {
        let a = store
            .get_or_create_entity("A", "thing", json!({}))
            .await
            .unwrap();
        let b = store
            .get_or_create_entity("B", "thing", json!({}))
            .await
            .unwrap();
        let id = RelationshipId::new();
        store
            .create_relationship_with(
                id,
                NewRelationship {
                    from: a.id,
                    to: b.id,
                    predicate: "uses".into(),
                    confidence: 0.5,
                },
                Utc::now() - Duration::days(1),
                Some(provenance(evidence)),
            )
            .await
            .expect("edge");
        id
    }

    #[tokio::test]
    async fn only_current_mined_drawers_that_were_not_read_are_pending() {
        let store = store().await;
        let r = room(&store, "w").await;
        let pending = drawer(&store, r, "mined", true).await;
        let read = drawer(&store, r, "already read", true).await;
        let _manual = drawer(&store, r, "written by hand", false).await;
        let old = drawer(&store, r, "superseded", true).await;
        store
            .supersede_drawer(old.id, None, Utc::now())
            .await
            .expect("retire");
        store
            .mark_drawer_extracted(read.id, "test", 0, 0, None, Utc::now())
            .await
            .expect("mark");

        let listed = store
            .list_drawers_pending_extraction(None, 10)
            .await
            .expect("pending");
        let ids: Vec<_> = listed.iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![pending.id]);
        assert!(store.drawer_extracted(read.id).await.unwrap());
        assert!(!store.drawer_extracted(pending.id).await.unwrap());
    }

    #[tokio::test]
    async fn a_note_without_an_origin_is_pending_extraction_but_a_manual_drawer_is_not() {
        let store = store().await;
        let r = room(&store, "w").await;
        let provenance = || Provenance {
            requested_by: "cli".into(),
            job_id: None,
        };
        let mut note = Drawer::new(
            DrawerId::new(),
            r,
            "Alice works at Acme".to_string(),
            Source::new(SourceKind::Note, None, None),
            vec![],
            provenance(),
        );
        note.valid_from = Utc::now() - Duration::days(1);
        store.create_drawer(&note).await.expect("create note");
        let manual = Drawer::new(
            DrawerId::new(),
            r,
            "written by hand".to_string(),
            Source::new(SourceKind::Manual, None, None),
            vec![],
            provenance(),
        );
        store.create_drawer(&manual).await.expect("create manual");

        let listed = store
            .list_drawers_pending_extraction(None, 10)
            .await
            .expect("pending");
        assert_eq!(
            listed.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![note.id]
        );
    }

    #[tokio::test]
    async fn marking_a_drawer_twice_leaves_it_marked_once() {
        let store = store().await;
        let r = room(&store, "w").await;
        let d = drawer(&store, r, "x", true).await;
        for _ in 0..2 {
            store
                .mark_drawer_extracted(d.id, "test", 1, 1, None, Utc::now())
                .await
                .expect("mark");
        }
        assert!(
            store
                .list_drawers_pending_extraction(None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pending_drawers_can_be_narrowed_to_a_wing() {
        let store = store().await;
        let here = room(&store, "here").await;
        let there = room(&store, "there").await;
        let wanted = drawer(&store, here, "in the wing", true).await;
        let _other = drawer(&store, there, "elsewhere", true).await;
        let listed = store
            .list_drawers_pending_extraction(Some("here"), 10)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|d| d.id).collect::<Vec<_>>(),
            vec![wanted.id]
        );
    }

    #[tokio::test]
    async fn a_relationship_keeps_the_provenance_it_was_extracted_with() {
        let store = store().await;
        let r = room(&store, "w").await;
        let d = drawer(&store, r, "A uses B", true).await;
        let id = edge(&store, &d).await;
        let a = store.find_entity_by_name("A").await.unwrap().expect("A");

        let edges = store.list_relationships(a.id, false).await.unwrap();
        let found = edges.iter().find(|e| e.id == id).expect("edge");
        let provenance = found.provenance.as_ref().expect("provenance");
        assert_eq!(provenance.drawer, d.id);
        assert_eq!(provenance.origin, d.source.origin);
        assert_eq!(provenance.extractor, "test");
    }

    #[tokio::test]
    async fn a_directly_asserted_relationship_has_no_provenance() {
        let store = store().await;
        let a = store
            .get_or_create_entity("A", "thing", json!({}))
            .await
            .unwrap();
        let b = store
            .get_or_create_entity("B", "thing", json!({}))
            .await
            .unwrap();
        store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from: a.id,
                    to: b.id,
                    predicate: "uses".into(),
                    confidence: 1.0,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        let edges = store.list_relationships(a.id, false).await.unwrap();
        assert!(edges[0].provenance.is_none());
    }

    #[tokio::test]
    async fn a_mention_keeps_the_provenance_of_the_link() {
        let store = store().await;
        let r = room(&store, "w").await;
        let d = drawer(&store, r, "A", true).await;
        let a = store
            .get_or_create_entity("A", "thing", json!({}))
            .await
            .unwrap();
        store
            .link_drawer_entity_with(d.id, a.id, Some(&provenance(&d)))
            .await
            .unwrap();
        let mentions = store.list_entity_mentions(a.id).await.unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].drawer, d.id);
        assert_eq!(mentions[0].provenance.as_ref().unwrap().drawer, d.id);
    }

    #[tokio::test]
    async fn facts_of_a_superseded_drawer_are_closed_but_stay_in_history() {
        let store = store().await;
        let r = room(&store, "w").await;
        let evidence = drawer(&store, r, "A uses B", true).await;
        let id = edge(&store, &evidence).await;
        let a = store.find_entity_by_name("A").await.unwrap().unwrap();

        // Nothing is closed while the evidence is current.
        assert_eq!(store.close_facts_of_retired_drawers().await.unwrap(), 0);
        assert_eq!(
            store.list_relationships(a.id, false).await.unwrap().len(),
            1
        );

        store
            .supersede_drawer(evidence.id, None, Utc::now())
            .await
            .unwrap();
        assert_eq!(store.close_facts_of_retired_drawers().await.unwrap(), 1);
        assert!(
            store
                .list_relationships(a.id, false)
                .await
                .unwrap()
                .is_empty()
        );
        let history = store.list_relationships(a.id, true).await.unwrap();
        assert!(history.iter().any(|e| e.id == id && e.valid_to.is_some()));
        // A second pass has nothing left to close.
        assert_eq!(store.close_facts_of_retired_drawers().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn closing_retired_facts_never_touches_a_directly_asserted_one() {
        let store = store().await;
        let r = room(&store, "w").await;
        let evidence = drawer(&store, r, "x", true).await;
        let a = store
            .get_or_create_entity("A", "thing", json!({}))
            .await
            .unwrap();
        let b = store
            .get_or_create_entity("B", "thing", json!({}))
            .await
            .unwrap();
        store
            .create_relationship(
                RelationshipId::new(),
                NewRelationship {
                    from: a.id,
                    to: b.id,
                    predicate: "uses".into(),
                    confidence: 1.0,
                },
                Utc::now(),
            )
            .await
            .unwrap();
        store
            .supersede_drawer(evidence.id, None, Utc::now())
            .await
            .unwrap();
        assert_eq!(store.close_facts_of_retired_drawers().await.unwrap(), 0);
        assert_eq!(
            store.list_relationships(a.id, false).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn an_other_kind_mention_resolves_to_a_better_typed_entity_of_the_same_name() {
        let store = store().await;
        store
            .get_or_create_entity("Ada", "other", json!({}))
            .await
            .unwrap();
        let person = store
            .get_or_create_entity("Ada", "person", json!({}))
            .await
            .unwrap();
        assert_eq!(
            store.find_entity_by_name("Ada").await.unwrap().unwrap().id,
            person.id
        );
        assert!(store.find_entity_by_name("Nobody").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn entities_can_be_listed_by_kind_and_name() {
        let store = store().await;
        store
            .get_or_create_entity("Ada Lovelace", "person", json!({}))
            .await
            .unwrap();
        store
            .get_or_create_entity("Adafruit", "organization", json!({}))
            .await
            .unwrap();
        store
            .get_or_create_entity("Bob", "person", json!({}))
            .await
            .unwrap();
        let people = store.list_entities(None, Some("Person"), 10).await.unwrap();
        assert_eq!(people.len(), 2);
        let ada = store.list_entities(Some("ADA"), None, 10).await.unwrap();
        assert_eq!(ada.len(), 2);
        assert_eq!(store.list_entities(None, None, 1).await.unwrap().len(), 1);
    }
}
