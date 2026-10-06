//! The evaluation dataset: documents, the edits that supersede some of them, entity links and judged queries.
//!
//! A dataset is plain JSON under `tests/fixtures/retrieval/`, hashed byte for byte so a report says which one it measured.
//! Document ids are the dataset's own: the daemon assigns its own drawer ids, and the harness translates between the two.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use memcastle::domain::RankingMode;
use serde::Deserialize;

/// The bundled dataset the committed baseline was measured on.
pub fn core_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/retrieval/core.json")
}

/// The committed baseline for [`core_path`].
pub fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/retrieval/baseline.json")
}

/// One retrieval configuration a query can be run under.
#[derive(Debug, Clone, Copy)]
pub struct RunConfig {
    /// The name a report and a comparison key on. Renaming one orphans its baseline.
    pub name: &'static str,
    pub ranking: RankingMode,
    /// Whether the knowledge graph adds related drawers after the direct hits.
    ///
    /// Expansion appends up to `limit` related drawers *after* the direct hits. A vector search always fills its page,
    /// so with a vector leg the graph's drawers would sit beyond every cut-off (and the junk that fills the page would
    /// be the seeds). Expansion is therefore measured on top of lexical search, whose direct hits are only drawers
    /// that match, which is also what makes the measurement repeatable.
    pub expand: bool,
}

/// Every configuration, in the order a report lists them.
pub const RUNS: [RunConfig; 4] = [
    RunConfig {
        name: "lexical",
        ranking: RankingMode::Lexical,
        expand: false,
    },
    RunConfig {
        name: "semantic",
        ranking: RankingMode::Semantic,
        expand: false,
    },
    RunConfig {
        name: "hybrid",
        ranking: RankingMode::Hybrid,
        expand: false,
    },
    RunConfig {
        name: "lexical+expand",
        ranking: RankingMode::Lexical,
        expand: true,
    },
];

#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    pub id: String,
    pub wing: String,
    pub room: String,
    pub content: String,
}

/// A correction: `doc` stops being valid and `new_id` takes its place, in the same room, during `epoch`.
#[derive(Debug, Clone, Deserialize)]
pub struct Supersession {
    pub epoch: usize,
    pub doc: String,
    pub new_id: String,
    pub content: String,
}

/// `doc` mentions the entity `entity`, which is what the graph-aware configuration walks.
#[derive(Debug, Clone, Deserialize)]
pub struct Mention {
    pub doc: String,
    pub entity: String,
    pub kind: String,
}

/// The point or window in time a query asks about, in the dataset's own epochs.
///
/// Epoch 0 is the state after every document was written, epoch `n` the state after the supersessions of epoch `n`.
/// A query with none of these asks about the present.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalSpec {
    AsOfEpoch(usize),
    BetweenEpochs(usize, usize),
    Historical,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QueryFilter {
    pub wing: Option<String>,
    pub room: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Judgment {
    pub doc: String,
    /// Graded relevance: 2 is the answer, 1 is related, anything not listed is 0.
    pub grade: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Query {
    pub id: String,
    pub category: String,
    pub text: String,
    #[serde(default)]
    pub filter: QueryFilter,
    #[serde(default)]
    pub temporal: Option<TemporalSpec>,
    /// The configurations this query is measured under. Defaults to every one that is meaningful for its category.
    #[serde(default)]
    pub runs: Option<Vec<String>>,
    pub relevant: Vec<Judgment>,
}

impl Query {
    /// The names of the configurations this query is run under.
    ///
    /// Graph expansion only changes a query that has something to expand to, so only the `graph` category is also
    /// run with it: running every query with expansion would measure the extra tail it appends, not the graph.
    pub fn run_names(&self) -> Vec<String> {
        if let Some(runs) = &self.runs {
            return runs.clone();
        }
        RUNS.iter()
            .filter(|run| !run.expand || self.category == "graph")
            .map(|run| run.name.to_string())
            .collect()
    }

    /// The judgments as a grade per document.
    pub fn grades(&self) -> HashMap<String, u32> {
        self.relevant
            .iter()
            .map(|judgment| (judgment.doc.clone(), judgment.grade))
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dataset {
    pub name: String,
    pub version: u32,
    #[allow(dead_code)]
    pub description: String,
    /// Words that mean the same thing to the stand-in embedding model, so a paraphrase lands near its source.
    pub concepts: BTreeMap<String, String>,
    pub documents: Vec<Document>,
    #[serde(default)]
    pub supersessions: Vec<Supersession>,
    #[serde(default)]
    pub mentions: Vec<Mention>,
    pub queries: Vec<Query>,
}

impl Dataset {
    /// Read and validate the dataset at `path`, with the SHA-256 of its bytes.
    pub fn load(path: &Path) -> (Self, String) {
        let bytes = std::fs::read(path)
            .unwrap_or_else(|error| panic!("cannot read the dataset {}: {error}", path.display()));
        let dataset: Self = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("{} is not a valid dataset: {error}", path.display()));
        if let Err(problem) = dataset.validate() {
            panic!("{} is not a valid dataset: {problem}", path.display());
        }
        (dataset, memcastle::domain::sha256_hex(&bytes))
    }

    /// The last epoch any supersession happens in.
    pub fn last_epoch(&self) -> usize {
        self.supersessions
            .iter()
            .map(|supersession| supersession.epoch)
            .max()
            .unwrap_or(0)
    }

    /// Where each document lives, and when it began and stopped being valid.
    pub fn index(&self) -> Index {
        let mut index = Index::default();
        for document in &self.documents {
            index.placement.insert(
                document.id.clone(),
                (document.wing.clone(), document.room.clone()),
            );
        }
        for supersession in &self.supersessions {
            // A replacement is filed in the room of the drawer it replaces.
            if let Some(place) = index.placement.get(&supersession.doc).cloned() {
                index.placement.insert(supersession.new_id.clone(), place);
            }
            index
                .created_epoch
                .insert(supersession.new_id.clone(), supersession.epoch);
            index
                .ended_epoch
                .insert(supersession.doc.clone(), supersession.epoch);
        }
        index
    }

    /// Why this dataset cannot be run, or `Ok`.
    pub fn validate(&self) -> Result<(), String> {
        let mut known: HashSet<&str> = HashSet::new();
        for document in &self.documents {
            if !known.insert(&document.id) {
                return Err(format!("document id `{}` is used twice", document.id));
            }
        }
        let mut superseded: HashSet<&str> = HashSet::new();
        for supersession in &self.supersessions {
            if supersession.epoch == 0 {
                return Err(format!(
                    "`{}` supersedes in epoch 0, which is the state before any edit",
                    supersession.doc
                ));
            }
            if !self.documents.iter().any(|d| d.id == supersession.doc) {
                return Err(format!(
                    "`{}` supersedes unknown document `{}`",
                    supersession.new_id, supersession.doc
                ));
            }
            if !superseded.insert(&supersession.doc) {
                return Err(format!("`{}` is superseded twice", supersession.doc));
            }
            if !known.insert(&supersession.new_id) {
                return Err(format!(
                    "document id `{}` is used twice",
                    supersession.new_id
                ));
            }
        }
        for mention in &self.mentions {
            if !known.contains(mention.doc.as_str()) {
                return Err(format!(
                    "a mention names unknown document `{}`",
                    mention.doc
                ));
            }
        }
        let last = self.last_epoch();
        let mut queries: HashSet<&str> = HashSet::new();
        for query in &self.queries {
            if !queries.insert(&query.id) {
                return Err(format!("query id `{}` is used twice", query.id));
            }
            if query.relevant.is_empty() {
                return Err(format!(
                    "query `{}` has no relevant document, so no metric is defined for it",
                    query.id
                ));
            }
            for judgment in &query.relevant {
                if !known.contains(judgment.doc.as_str()) {
                    return Err(format!(
                        "query `{}` judges unknown document `{}`",
                        query.id, judgment.doc
                    ));
                }
                if judgment.grade == 0 {
                    return Err(format!(
                        "query `{}` lists `{}` with grade 0: leave it out instead",
                        query.id, judgment.doc
                    ));
                }
            }
            match &query.temporal {
                Some(TemporalSpec::AsOfEpoch(epoch)) if *epoch > last => {
                    return Err(format!(
                        "query `{}` asks about epoch {epoch}, after the last ({last})",
                        query.id
                    ));
                }
                Some(TemporalSpec::BetweenEpochs(from, until))
                    if from >= until || *until > last =>
                {
                    return Err(format!(
                        "query `{}` has the empty or unknown epoch window {from}..{until}",
                        query.id
                    ));
                }
                _ => {}
            }
            for name in query.run_names() {
                if !RUNS.iter().any(|run| run.name == name) {
                    return Err(format!(
                        "query `{}` names the unknown configuration `{name}`",
                        query.id
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Placement and lifetime of each document id, for checking what a search was allowed to return.
#[derive(Debug, Default)]
pub struct Index {
    placement: HashMap<String, (String, String)>,
    created_epoch: HashMap<String, usize>,
    ended_epoch: HashMap<String, usize>,
}

impl Index {
    /// Whether `doc` was inside the wing and room the query asked for.
    pub fn in_scope(&self, doc: &str, filter: &QueryFilter) -> bool {
        // A document the dataset does not describe (bulk filler) lives in no wing a query can name.
        let Some((wing, room)) = self.placement.get(doc) else {
            return filter.wing.is_none() && filter.room.is_none();
        };
        filter.wing.as_ref().is_none_or(|w| w == wing)
            && filter.room.as_ref().is_none_or(|r| r == room)
    }

    /// Whether `doc` was valid at the time `temporal` asks about.
    ///
    /// A document is written during `created` and corrected during `ended`; epoch `n` is the instant after epoch `n`'s
    /// edits. So it is valid at epoch `e` when it was written by then and not yet corrected, and it overlaps the window
    /// from epoch `a` to epoch `b` when it was written by `b` and was still valid after `a`.
    pub fn visible(&self, doc: &str, temporal: Option<&TemporalSpec>) -> bool {
        let created = self.created_epoch.get(doc).copied().unwrap_or(0);
        let ended = self.ended_epoch.get(doc).copied();
        match temporal {
            None => ended.is_none(),
            Some(TemporalSpec::Historical) => true,
            Some(TemporalSpec::AsOfEpoch(epoch)) => {
                created <= *epoch && ended.is_none_or(|end| end > *epoch)
            }
            Some(TemporalSpec::BetweenEpochs(from, until)) => {
                created <= *until && ended.is_none_or(|end| end > *from)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // The datasets live in `tests/`, so their own checks do too: this module is part of the `in_process` binary.
    use super::*;

    #[test]
    fn the_core_dataset_is_well_formed() {
        let (dataset, hash) = Dataset::load(&core_path());
        assert_eq!(hash.len(), 64, "a SHA-256 in hex");
        assert!(
            dataset.documents.len() >= 50,
            "small is fine, but not trivially small"
        );
        assert!(dataset.queries.len() >= 30);
    }

    #[test]
    fn a_dataset_that_judges_an_unknown_document_is_refused() {
        let (mut dataset, _) = Dataset::load(&core_path());
        dataset.queries[0].relevant[0].doc = "nope".into();
        let problem = dataset
            .validate()
            .expect_err("an unknown judgment is invalid");
        assert!(problem.contains("nope"), "{problem}");
    }

    #[test]
    fn a_dataset_that_reuses_a_document_id_is_refused() {
        let (mut dataset, _) = Dataset::load(&core_path());
        let copy = dataset.documents[0].clone();
        dataset.documents.push(copy);
        assert!(dataset.validate().is_err());
    }

    #[test]
    fn a_dataset_with_an_empty_epoch_window_is_refused() {
        let (mut dataset, _) = Dataset::load(&core_path());
        dataset.queries[0].temporal = Some(TemporalSpec::BetweenEpochs(1, 1));
        assert!(dataset.validate().is_err());
    }

    #[test]
    fn visibility_follows_the_epoch_a_document_was_written_and_corrected_in() {
        let (dataset, _) = Dataset::load(&core_path());
        let index = dataset.index();
        // `e07` was corrected in epoch 1 by `e07b`.
        assert!(
            !index.visible("e07", None),
            "the present sees the correction only"
        );
        assert!(index.visible("e07b", None));
        assert!(index.visible("e07", Some(&TemporalSpec::AsOfEpoch(0))));
        assert!(!index.visible("e07b", Some(&TemporalSpec::AsOfEpoch(0))));
        assert!(!index.visible("e07", Some(&TemporalSpec::AsOfEpoch(1))));
        assert!(index.visible("e07", Some(&TemporalSpec::BetweenEpochs(0, 1))));
        assert!(index.visible("e07b", Some(&TemporalSpec::BetweenEpochs(0, 1))));
        assert!(index.visible("e07", Some(&TemporalSpec::Historical)));
    }

    #[test]
    fn a_replacement_is_filed_where_the_document_it_replaces_was() {
        let (dataset, _) = Dataset::load(&core_path());
        let index = dataset.index();
        let engineering = QueryFilter {
            wing: Some("work".into()),
            room: Some("engineering".into()),
        };
        assert!(index.in_scope("e07b", &engineering));
        let elsewhere = QueryFilter {
            wing: Some("home".into()),
            room: None,
        };
        assert!(!index.in_scope("e07b", &elsewhere));
    }

    #[test]
    fn only_graph_queries_are_also_run_with_expansion() {
        let (dataset, _) = Dataset::load(&core_path());
        for query in &dataset.queries {
            let expanded = query
                .run_names()
                .iter()
                .any(|name| name == "lexical+expand");
            assert_eq!(expanded, query.category == "graph", "{}", query.id);
        }
    }
}
