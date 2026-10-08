//! The extract job handler: read every mined or noted drawer that has not been read, and add what it says to the graph.
//!
//! Like the embed sweep, this keeps no cursor of its own: each pass asks the store for the next drawers *without an
//! extraction marker*, so the database is the checkpoint and a crash, a pause or a retry simply asks again.
//!
//! **Extraction only adds.** It never writes a drawer: it creates entities, `mentions` links and `relates_to` edges
//! (each carrying the provenance of the drawer it was read from), and then a marker that says the drawer was read.
//! Every write before the marker is idempotent — entities by `(name, kind)`, links by `(drawer, entity)`, edges by an
//! id derived from the drawer and the fact — so a crash between any two of them replays the drawer harmlessly
//! (ADR-008). The marker comes last for exactly that reason.
//!
//! A provider failure fails the job rather than skipping the drawer, as embedding does: a silent hole in the graph
//! would be worse than a failed job visible in `memcastle job list`.

use std::collections::HashMap;

use chrono::Utc;
use serde_json::json;

use crate::domain::{
    Drawer, EntityId, EntityKind, ExtractedGraph, FactProvenance, Job, JobProgress,
    NewRelationship, Observation, RelationshipId,
};
use crate::error::{Error, Result};
use crate::events::Event;
use crate::jobs::{JobContext, JobOutcome};
use crate::store::SurrealStore;

/// What an `Extract` job needs, gathered from its [`crate::domain::JobKind`].
pub struct ExtractParams {
    /// Restrict the sweep to one wing by name.
    pub wing: Option<String>,
}

/// What a run has written so far, kept in the job's checkpoint so a resumed job continues its count.
#[derive(Default, Clone, Copy)]
struct Totals {
    drawers: u64,
    entities: u64,
    relations: u64,
    closed: u64,
}

impl Totals {
    fn resume(job: &Job) -> Self {
        let read = |key: &str| JobContext::resume_index(job, key) as u64;
        Self {
            drawers: read("drawers"),
            entities: read("entities"),
            relations: read("relations"),
            closed: read("closed"),
        }
    }

    fn to_json(self) -> serde_json::Value {
        json!({
            "drawers": self.drawers,
            "entities": self.entities,
            "relations": self.relations,
            "closed": self.closed,
        })
    }
}

/// Extract from drawers until none are left unread, or the job is stopped.
///
/// # Errors
///
/// [`Error::ExtractionNotConfigured`] without a provider, [`Error::ExtractionFailed`] when it fails, or a store
/// error.
pub async fn run(ctx: &JobContext, job: &mut Job, params: ExtractParams) -> Result<JobOutcome> {
    let ExtractParams { wing } = params;
    let extraction = ctx.extraction();
    let Some(extractor) = extraction.name() else {
        return Err(Error::ExtractionNotConfigured);
    };
    let store = ctx.store();
    let mut totals = Totals::resume(job);
    tracing::info!(already_read = totals.drawers, "extraction sweep started");

    // First, so a drawer replaced since the last sweep stops vouching for its facts before its replacement is
    // read: the facts of a superseded drawer are history, not current knowledge.
    totals.closed += store.close_facts_of_retired_drawers().await?;

    // Spelling variants (case, punctuation, aliases) always converge; typos only when deduplication allows it.
    let fuzzy = ctx.dedup().enabled && ctx.dedup().entity_fuzzy;
    let pass = u32::try_from(extraction.batch_size()).unwrap_or(u32::MAX);
    loop {
        if let Some(stop) = ctx.stop_requested() {
            return Ok(stop);
        }
        let drawers = store
            .list_drawers_pending_extraction(wing.as_deref(), pass)
            .await?;
        if drawers.is_empty() {
            break;
        }
        let texts: Vec<String> = drawers.iter().map(|d| d.content.clone()).collect();
        let graphs = extraction.extract(&texts).await?;
        let (entities_before, relations_before) = (totals.entities, totals.relations);
        for (drawer, graph) in drawers.iter().zip(graphs) {
            let (entities, relations) =
                write_graph(store, job, extractor, drawer, graph, fuzzy).await?;
            totals.drawers += 1;
            totals.entities += entities;
            totals.relations += relations;
        }
        let progress = JobProgress {
            current: u32::try_from(totals.drawers).unwrap_or(u32::MAX),
            total: None,
            message: Some(format!(
                "read {} drawer(s): {} entities, {} relationships",
                totals.drawers, totals.entities, totals.relations
            )),
        };
        // One event per pass, after the entities are saved: the graph view re-reads once, however many were found.
        if entities_before != totals.entities || relations_before != totals.relations {
            ctx.events().publish(Event::entity_graph_changed());
        }
        ctx.checkpoint(job, progress, totals.to_json()).await?;
    }
    job.result = Some(totals.to_json());
    tracing::info!(
        drawers = totals.drawers,
        entities = totals.entities,
        relations = totals.relations,
        closed = totals.closed,
        "extraction sweep finished"
    );
    Ok(JobOutcome::Completed)
}

/// Write one drawer's graph and then mark the drawer read. Returns how many entities were linked and relationships
/// written.
async fn write_graph(
    store: &SurrealStore,
    job: &Job,
    extractor: &'static str,
    drawer: &Drawer,
    graph: ExtractedGraph,
    fuzzy: bool,
) -> Result<(u64, u64)> {
    let origin = drawer.source.origin.clone();
    let provenance = FactProvenance {
        drawer: drawer.id,
        origin: origin.clone(),
        job_id: Some(job.id),
        extractor: extractor.to_string(),
        extracted_at: Utc::now(),
    };

    // A fact holds from when the document it was read from says it did, else from when the drawer began to hold;
    // never from the future, whatever a source claims.
    let mut valid_from = drawer.valid_from;
    if let Some(origin) = &origin
        && let Some(document) = store
            .get_source_document(origin.source_id, &origin.document)
            .await?
        && let Some(occurred_at) = document.occurred_at
    {
        valid_from = occurred_at;
    }
    let valid_from = valid_from.min(Utc::now());

    let mut ids: HashMap<String, EntityId> = HashMap::new();
    let mut linked = 0u64;
    for entity in &graph.entities {
        let (id, observation) = resolve_entity(store, &entity.name, entity.kind, fuzzy).await?;
        // The mention keeps the drawer's own spelling and how it was resolved, so converging two spellings on
        // one entity loses neither.
        if store
            .link_drawer_entity_observed(drawer.id, id, Some(&provenance), Some(&observation))
            .await?
        {
            linked += 1;
        }
        ids.insert(entity.name.clone(), id);
    }

    let mut written = 0u64;
    for relation in &graph.relations {
        let (Some(&from), Some(&to)) = (ids.get(&relation.subject), ids.get(&relation.object))
        else {
            // `ExtractedGraph::normalise` guarantees both endpoints; skipping beats trusting it blindly.
            continue;
        };
        // From the drawer and the fact alone, so a replay of this drawer names the edge it already wrote and a
        // second job never duplicates it.
        let id = RelationshipId::derive(
            drawer.id.0,
            &format!("extract-edge:{from}:{}:{to}", relation.predicate.as_str()),
        );
        let fact = store
            .create_relationship_with(
                id,
                NewRelationship {
                    from,
                    to,
                    predicate: relation.predicate.as_str().to_string(),
                    confidence: relation.confidence,
                },
                valid_from,
                Some(provenance.clone()),
            )
            .await?;
        // Link after the edge exists but before the marker: a crash here replays stable link IDs.
        store.reconcile_extracted_fact(&fact).await?;
        if let Some(predecessor) = drawer.supersedes {
            store.link_revised_extraction(&fact, predecessor).await?;
        }
        written += 1;
    }

    // Last: see the module doc.
    store
        .mark_drawer_extracted(
            drawer.id,
            extractor,
            u32::try_from(graph.entities.len()).unwrap_or(u32::MAX),
            u32::try_from(written).unwrap_or(u32::MAX),
            Some(job.id),
            Utc::now(),
        )
        .await?;
    Ok((linked, written))
}

/// The entity `name` of `kind` refers to, created if need be, and how that was decided.
///
/// A spelling variant of an entity the graph knows converges on it (docs/adr/025); an entity an extractor could
/// not classify (`other`) joins an existing entity of the same name rather than splitting it from a better-typed
/// one. Only graph records are read and written: no drawer is touched.
async fn resolve_entity(
    store: &SurrealStore,
    name: &str,
    kind: EntityKind,
    fuzzy: bool,
) -> Result<(EntityId, Observation)> {
    // Check-then-create races with another daemon on a shared palace: the loser hits the unique index, and a
    // second try finds the winner's entity.
    let resolve = || store.resolve_or_create_entity(name, kind.as_str(), fuzzy);
    let (entity, observation) = match resolve().await {
        Ok(resolved) => resolved,
        Err(_) => resolve().await?,
    };
    Ok((entity.id, observation))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use serde_json::json;

    use super::*;
    use crate::config::{ExtractionConfig, MiningConfig};
    use crate::domain::{JobEvent, JobKind, MiningSource, Predicate, Priority, Relationship};
    use crate::extract::{Extraction, Extractor, fake, heuristic::HeuristicExtractor};
    use crate::jobs::JobControl;

    fn heuristic() -> Extraction {
        Extraction::new(HeuristicExtractor, &ExtractionConfig::default())
    }

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    /// Push `name`'s modification time `seconds` into the future, so a re-mine always sees it as newer.
    fn touch_ahead(dir: &Path, name: &str, seconds: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(seconds))
            .unwrap();
    }

    /// Mine `dir` through the Source/SourceAdapter path, exactly as `memcastle mine` would.
    async fn mine(store: &SurrealStore, dir: &Path) {
        let mut job = Job::new(
            JobKind::Mine {
                source: MiningSource::Directory {
                    path: dir.to_path_buf(),
                },
                wing: Some("docs".to_string()),
                full: false,
                options: Default::default(),
            },
            Priority::Background,
            "test",
        );
        job.apply(JobEvent::Claim).unwrap();
        let ctx = JobContext::new(job.id, JobControl::default(), store.clone())
            .with_mining(MiningConfig::default());
        let outcome = crate::mining::run(
            &ctx,
            &mut job,
            crate::mining::MiningParams {
                source: MiningSource::Directory {
                    path: dir.to_path_buf(),
                },
                wing: Some("docs".to_string()),
                full: false,
                options: Default::default(),
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome, JobOutcome::Completed);
    }

    fn extract_job() -> Job {
        let mut job = Job::new(
            JobKind::Extract { wing: None },
            Priority::Background,
            "test",
        );
        job.apply(JobEvent::Claim).unwrap();
        job
    }

    async fn extract_with(
        store: &SurrealStore,
        extraction: Extraction,
        control: JobControl,
    ) -> (Job, Result<JobOutcome>) {
        let mut job = extract_job();
        let ctx = JobContext::new(job.id, control, store.clone()).with_extraction(extraction);
        let outcome = run(&ctx, &mut job, ExtractParams { wing: None }).await;
        (job, outcome)
    }

    async fn extract(store: &SurrealStore) -> Job {
        let (job, outcome) = extract_with(store, heuristic(), JobControl::default()).await;
        assert_eq!(outcome.unwrap(), JobOutcome::Completed);
        job
    }

    async fn edges_of(store: &SurrealStore, name: &str) -> Vec<Relationship> {
        let entity = store
            .find_entity_by_name(name)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no entity {name}"));
        store.list_relationships(entity.id, true).await.unwrap()
    }

    #[tokio::test]
    async fn mined_content_yields_entities_and_relationships_with_provenance_and_validity() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "team.md", "Ada Lovelace works on MemCastle.");
        mine(&store, dir.path()).await;

        let job = extract(&store).await;
        assert_eq!(job.result.as_ref().unwrap()["drawers"], 1);

        let edges = edges_of(&store, "Ada Lovelace").await;
        assert_eq!(edges.len(), 1);
        let edge = &edges[0];
        assert_eq!(edge.predicate, Predicate::WorksOn.as_str());
        assert!(edge.valid_to.is_none(), "a fresh fact is current");
        assert!(edge.valid_from <= Utc::now());

        // Provenance names the drawer, its source origin, the job and the extractor.
        let provenance = edge
            .provenance
            .as_ref()
            .expect("extracted facts have provenance");
        let drawer = store
            .list_drawers(None)
            .await
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(provenance.drawer, drawer.id);
        assert_eq!(provenance.origin, drawer.source.origin);
        assert_eq!(provenance.origin.as_ref().unwrap().document, "team.md");
        assert_eq!(provenance.job_id, Some(job.id));
        assert_eq!(provenance.extractor, "heuristic");

        // The mention carries it too, and is reachable from the entity.
        let ada = store
            .find_entity_by_name("Ada Lovelace")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ada.kind, "person");
        let mentions = store.list_entity_mentions(ada.id).await.unwrap();
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].provenance.as_ref().unwrap().drawer, drawer.id);
    }

    #[tokio::test]
    async fn extraction_never_changes_the_drawers_it_reads() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.md",
            "Ada works on MemCastle and uses `surrealdb`.",
        );
        write(dir.path(), "b.md", "Bob maintains Parser.");
        mine(&store, dir.path()).await;
        let before = serde_json::to_value(store.list_drawers(None).await.unwrap()).unwrap();

        extract(&store).await;

        let after = serde_json::to_value(store.list_drawers(None).await.unwrap()).unwrap();
        assert_eq!(
            before, after,
            "content, hash, validity and timestamps are all canonical"
        );
    }

    #[tokio::test]
    async fn spelling_variants_from_different_documents_converge_and_keep_each_sources_spelling() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "Ada works on MemCastle.");
        write(dir.path(), "b.md", "ADA works on MemCastle.");
        mine(&store, dir.path()).await;

        extract(&store).await;

        let people = store.list_entities(None, Some("person"), 10).await.unwrap();
        assert_eq!(
            people.len(),
            1,
            "casing alone must not split an entity: {people:?}"
        );
        let ada = &people[0];
        let mentions = store.list_entity_mentions(ada.id).await.unwrap();
        assert_eq!(
            mentions.len(),
            2,
            "both documents still vouch for the entity"
        );
        let mut spellings: Vec<String> = mentions
            .iter()
            .map(|m| m.observation.as_ref().expect("an observation").name.clone())
            .collect();
        spellings.sort();
        assert_eq!(
            spellings,
            ["ADA", "Ada"],
            "the source-specific names are not lost"
        );
        assert!(mentions.iter().all(|m| m.provenance.is_some()));
        assert_eq!(
            ada.aliases.len(),
            1,
            "the spelling that is not the canonical one is an alias"
        );
    }

    #[tokio::test]
    async fn independent_extractions_conflict_without_overwriting_either_drawer() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        let graph = |place: &str| {
            let wire: crate::extract::WireGraph = serde_json::from_value(json!({
                "entities": [
                    {"name": "Ada", "kind": "person"},
                    {"name": place, "kind": "place"}
                ],
                "relations": [{"subject": "Ada", "predicate": "located_in", "object": place, "confidence": 0.9}]
            })).unwrap();
            Extraction::new(
                fake::Canned(wire.into_graph()),
                &ExtractionConfig::default(),
            )
        };
        write(dir.path(), "a.md", "Ada is in Paris.");
        mine(&store, dir.path()).await;
        let (_, result) = extract_with(&store, graph("Paris"), JobControl::default()).await;
        assert_eq!(result.unwrap(), JobOutcome::Completed);

        write(dir.path(), "b.md", "Ada is in London.");
        mine(&store, dir.path()).await;
        let (_, result) = extract_with(&store, graph("London"), JobControl::default()).await;
        assert_eq!(result.unwrap(), JobOutcome::Completed);

        let ada = store.find_entity_by_name("Ada").await.unwrap().unwrap();
        let current = store.list_relationships(ada.id, false).await.unwrap();
        assert_eq!(current.len(), 2);
        assert!(
            current
                .iter()
                .all(|f| f.lifecycle.as_ref().unwrap().state
                    == crate::domain::FactState::Conflicting)
        );
        assert_eq!(
            store.list_drawers(None).await.unwrap().len(),
            2,
            "both pieces of evidence remain canonical"
        );
        let again = extract(&store).await;
        assert_eq!(again.result.as_ref().unwrap()["drawers"], 0);
        assert_eq!(
            store.fact_history(current[0].id, None).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn a_second_sweep_finds_nothing_to_read() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "Ada works on MemCastle.");
        mine(&store, dir.path()).await;
        extract(&store).await;

        let again = extract(&store).await;
        assert_eq!(again.result.as_ref().unwrap()["drawers"], 0);
        assert_eq!(edges_of(&store, "Ada").await.len(), 1);
    }

    #[tokio::test]
    async fn replaying_a_drawer_after_a_lost_marker_adds_nothing() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "Ada works on MemCastle.");
        mine(&store, dir.path()).await;
        let drawer = store.list_drawers(None).await.unwrap().remove(0);
        let graph = HeuristicExtractor
            .extract(std::slice::from_ref(&drawer.content))
            .await
            .unwrap()
            .remove(0);

        // The same drawer written twice, as a crash before the marker would make a replay do.
        let job = extract_job();
        for _ in 0..2 {
            write_graph(&store, &job, "heuristic", &drawer, graph.clone(), true)
                .await
                .unwrap();
        }
        assert_eq!(edges_of(&store, "Ada").await.len(), 1);
        let ada = store.find_entity_by_name("Ada").await.unwrap().unwrap();
        assert_eq!(store.list_entity_mentions(ada.id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_changed_document_closes_the_old_fact_and_opens_the_new_one() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "Ada works on MemCastle.");
        mine(&store, dir.path()).await;
        extract(&store).await;

        write(dir.path(), "a.md", "Ada works on Parser.");
        touch_ahead(dir.path(), "a.md", 3_600);
        mine(&store, dir.path()).await;
        let job = extract(&store).await;
        assert_eq!(job.result.as_ref().unwrap()["closed"], 1);
        assert_eq!(job.result.as_ref().unwrap()["drawers"], 1);

        let edges = edges_of(&store, "Ada").await;
        assert_eq!(edges.len(), 2, "history is kept");
        let old = edges
            .iter()
            .find(|e| e.valid_to.is_some())
            .expect("old fact closed");
        let new = edges
            .iter()
            .find(|e| e.valid_to.is_none())
            .expect("new fact open");
        let parser = store.find_entity_by_name("Parser").await.unwrap().unwrap();
        assert_eq!(new.to, parser.id);
        assert_ne!(old.to, parser.id);
        assert_eq!(
            old.lifecycle.as_ref().unwrap().state,
            crate::domain::FactState::Superseded
        );
        assert!(
            old.lifecycle
                .as_ref()
                .unwrap()
                .links
                .iter()
                .any(|link| link.kind == crate::domain::FactLinkKind::Supersedes
                    && link.from == new.id)
        );
        // A search of the present sees only the new fact.
        let ada = store.find_entity_by_name("Ada").await.unwrap().unwrap();
        let current = store.list_relationships(ada.id, false).await.unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].to, parser.id);
    }

    #[tokio::test]
    async fn drawers_that_did_not_come_through_a_source_are_not_read() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let wing = store.get_or_create_wing("w", None).await.unwrap();
        let room = store.get_or_create_room(wing.id, "r", None).await.unwrap();
        let drawer = crate::domain::Drawer::new(
            crate::domain::DrawerId::new(),
            room.id,
            "Ada works on MemCastle.".to_string(),
            crate::domain::Source::new(crate::domain::SourceKind::Manual, None, None),
            vec![],
            crate::domain::Provenance {
                requested_by: "test".into(),
                job_id: None,
            },
        );
        store.create_drawer(&drawer).await.unwrap();

        let job = extract(&store).await;
        assert_eq!(job.result.as_ref().unwrap()["drawers"], 0);
        assert!(store.find_entity_by_name("Ada").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_drawer_that_names_nothing_is_marked_so_it_is_not_sent_again() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "nothing worth naming here");
        mine(&store, dir.path()).await;

        assert_eq!(extract(&store).await.result.as_ref().unwrap()["drawers"], 1);
        assert_eq!(extract(&store).await.result.as_ref().unwrap()["drawers"], 0);
    }

    #[tokio::test]
    async fn a_provider_failure_fails_the_job_and_leaves_the_drawer_unread() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "Ada works on MemCastle.");
        mine(&store, dir.path()).await;

        let broken = Extraction::new(fake::Broken, &ExtractionConfig::default());
        let (_, outcome) = extract_with(&store, broken, JobControl::default()).await;
        assert!(matches!(outcome, Err(Error::ExtractionFailed { .. })));

        // Nothing was lost: the next sweep reads it.
        assert_eq!(extract(&store).await.result.as_ref().unwrap()["drawers"], 1);
    }

    #[tokio::test]
    async fn extracting_without_a_provider_is_a_typed_error() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let (_, outcome) =
            extract_with(&store, Extraction::disabled(), JobControl::default()).await;
        assert!(matches!(outcome, Err(Error::ExtractionNotConfigured)));
    }

    #[tokio::test]
    async fn a_pause_request_stops_the_sweep_before_it_reads_anything() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "Ada works on MemCastle.");
        mine(&store, dir.path()).await;

        let control = JobControl::default();
        control.request_pause();
        let (_, outcome) = extract_with(&store, heuristic(), control).await;
        assert_eq!(outcome.unwrap(), JobOutcome::Paused);
        assert!(store.find_entity_by_name("Ada").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_entity_the_provider_could_not_classify_joins_a_better_typed_one() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store
            .get_or_create_entity("Ada", "person", json!({}))
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "We thank Ada.");
        mine(&store, dir.path()).await;

        extract(&store).await;
        let all = store.list_entities(Some("Ada"), None, 10).await.unwrap();
        assert_eq!(all.len(), 1, "no second, vaguer Ada");
        assert_eq!(all[0].kind, "person");
    }

    #[tokio::test]
    async fn the_vocabulary_is_enforced_whatever_the_provider_says() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.md", "anything");
        mine(&store, dir.path()).await;

        // A provider answering in its own words; the `Extractor` contract already reads them into the vocabulary,
        // so what reaches the store can only be a member of it.
        let wire: crate::extract::WireGraph = serde_json::from_value(json!({
            "entities": [{"name": "Ada", "kind": "Human Being"}, {"name": "Rust", "kind": "language"}],
            "relations": [{"subject": "Ada", "predicate": "is fond of", "object": "Rust", "confidence": 0.9}]
        }))
        .unwrap();
        let extraction = Extraction::new(
            fake::Canned(wire.into_graph()),
            &ExtractionConfig::default(),
        );
        let (_, outcome) = extract_with(&store, extraction, JobControl::default()).await;
        assert_eq!(outcome.unwrap(), JobOutcome::Completed);

        assert_eq!(
            store
                .find_entity_by_name("Ada")
                .await
                .unwrap()
                .unwrap()
                .kind,
            "other"
        );
        let edges = edges_of(&store, "Ada").await;
        assert_eq!(edges[0].predicate, "related_to");
    }
}
