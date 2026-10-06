//! Running a dataset's queries under each retrieval configuration and scoring what came back.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use memcastle::domain::{SearchFilter, SearchQuery, Temporal};
use serde_json::Value;

use super::dataset::{Dataset, Query, RunConfig, TemporalSpec};
use super::metrics::{KS, Latency, Metrics, mean, score_query};
use super::report::RunReport;
use super::seed::{Seeded, client};
use super::vectors::Embedder;

/// The most drawers any configuration returns: the largest cut-off a report measures at.
pub const LIMIT: usize = 10;

/// What one query returned under one configuration.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub query: String,
    /// The dataset's document ids, best first, with the score the daemon gave each.
    pub ranked: Vec<(String, f32)>,
}

/// A configuration's report and the raw results behind it, for tests that inspect more than the means.
pub struct RunOutcome {
    pub report: RunReport,
    pub results: Vec<QueryResult>,
}

/// How the harness reaches the daemon and what it gives it.
pub struct Context<'a> {
    pub base: &'a str,
    pub seeded: &'a Seeded,
    /// Caller-computed vectors for each query. `None` leaves embedding to the daemon's provider.
    pub embedder: Option<&'a Embedder>,
}

/// Translate a dataset query into the daemon's search request.
pub fn request_for(
    query: &Query,
    run: &RunConfig,
    seeded: &Seeded,
    embedder: Option<&Embedder>,
) -> SearchQuery {
    let temporal = match &query.temporal {
        None => Temporal::Current,
        Some(TemporalSpec::Historical) => Temporal::All,
        Some(TemporalSpec::AsOfEpoch(epoch)) => Temporal::AsOf(seeded.epoch_end[*epoch]),
        Some(TemporalSpec::BetweenEpochs(from, until)) => Temporal::Between {
            from: seeded.epoch_end[*from],
            until: seeded.epoch_end[*until],
        },
    };
    SearchQuery {
        text: query.text.clone(),
        ranking: run.ranking,
        limit: u32::try_from(LIMIT).expect("a small limit"),
        filter: SearchFilter {
            wing: query.filter.wing.clone(),
            room: query.filter.room.clone(),
            temporal,
            ..SearchFilter::default()
        },
        expand: run.expand,
        query_embedding: embedder.map(|embedder| embedder.embed(&query.text)),
    }
}

/// Send one search and return the hits and how long the round trip took.
pub async fn search(
    http: &reqwest::Client,
    base: &str,
    request: &SearchQuery,
) -> (Vec<Value>, f64) {
    let started = Instant::now();
    let response = http
        .post(format!("{base}/api/search"))
        .json(request)
        .send()
        .await
        .expect("search request");
    let status = response.status();
    let body: Value = response.json().await.expect("search json");
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    assert!(
        status.is_success(),
        "searching for `{}` failed with {status}: {body}",
        request.text
    );
    (
        body.as_array().expect("a JSON array of hits").clone(),
        elapsed,
    )
}

/// Break ties by document id, so the arbitrary order the daemon gives equal scores cannot move a metric.
///
/// Only neighbours with exactly the same score are reordered: a ranking is never changed, and the direct hits of an
/// expanded search keep their place ahead of the drawers the graph added.
pub fn settle_ties(mut ranked: Vec<(String, f32)>) -> Vec<(String, f32)> {
    let mut start = 0;
    while start < ranked.len() {
        let mut end = start + 1;
        while end < ranked.len() && ranked[end].1.to_bits() == ranked[start].1.to_bits() {
            end += 1;
        }
        ranked[start..end].sort_by(|a, b| a.0.cmp(&b.0));
        start = end;
    }
    ranked
}

/// One search to run, and what counts as a good answer to it.
pub struct Case {
    pub id: String,
    pub category: String,
    pub request: SearchQuery,
    pub grades: HashMap<String, u32>,
}

/// Run every query of `dataset` that applies to `run`, then `repetitions` timed passes, and summarise.
pub async fn execute(
    context: &Context<'_>,
    dataset: &Dataset,
    run: &RunConfig,
    repetitions: usize,
) -> RunOutcome {
    let cases: Vec<Case> = dataset
        .queries
        .iter()
        .filter(|query| query.run_names().iter().any(|name| name == run.name))
        .map(|query| Case {
            id: query.id.clone(),
            category: query.category.clone(),
            request: request_for(query, run, context.seeded, context.embedder),
            grades: query.grades(),
        })
        .collect();
    execute_cases(
        context.base,
        &context.seeded.doc_of,
        run,
        &cases,
        repetitions,
    )
    .await
}

/// Run `cases` once untimed (and score that pass), then `repetitions` timed passes.
///
/// `doc_of` translates the drawer ids the daemon returns into the ids the cases' judgments use.
pub async fn execute_cases(
    base: &str,
    doc_of: &HashMap<String, String>,
    run: &RunConfig,
    cases: &[Case],
    repetitions: usize,
) -> RunOutcome {
    let http = client();
    // The first pass is the warm-up (caches, the HNSW graph, connection set-up) and is also the one that is scored:
    // retrieval is deterministic, so any pass would give the same answer.
    let mut results = Vec::new();
    let mut scored: Vec<(&Case, Metrics)> = Vec::new();
    for case in cases {
        let (hits, _) = search(&http, base, &case.request).await;
        let ranked = settle_ties(
            hits.iter()
                .map(|hit| {
                    let id = hit["id"].as_str().expect("a hit id");
                    let doc = doc_of
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| format!("unknown:{id}"));
                    (doc, hit["score"].as_f64().unwrap_or_default() as f32)
                })
                .collect(),
        );
        let docs: Vec<String> = ranked.iter().map(|(doc, _)| doc.clone()).collect();
        scored.push((case, score_query(&docs, &case.grades, &KS)));
        results.push(QueryResult {
            query: case.id.clone(),
            ranked,
        });
    }

    let mut samples = Vec::new();
    let timed = Instant::now();
    for _ in 0..repetitions {
        for case in cases {
            samples.push(search(&http, base, &case.request).await.1);
        }
    }
    let latency = Latency::from_samples(&samples, timed.elapsed().as_secs_f64());

    let all: Vec<&Metrics> = scored.iter().map(|(_, metrics)| metrics).collect();
    let mut by_category: BTreeMap<String, Vec<&Metrics>> = BTreeMap::new();
    for (case, metrics) in &scored {
        by_category
            .entry(case.category.clone())
            .or_default()
            .push(metrics);
    }
    RunOutcome {
        report: RunReport {
            name: run.name.to_string(),
            ranking: format!("{:?}", run.ranking).to_lowercase(),
            expand: run.expand,
            queries: cases.len(),
            metrics: mean(&all),
            by_category: by_category
                .into_iter()
                .map(|(category, metrics)| (category, mean(&metrics)))
                .collect(),
            latency,
        },
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked(entries: &[(&str, f32)]) -> Vec<(String, f32)> {
        entries
            .iter()
            .map(|(doc, score)| ((*doc).to_string(), *score))
            .collect()
    }

    #[test]
    fn equal_scores_are_ordered_by_document_id_and_nothing_else_moves() {
        let settled = settle_ties(ranked(&[("z", 0.9), ("b", 0.5), ("a", 0.5), ("c", 0.1)]));
        let docs: Vec<&str> = settled.iter().map(|(doc, _)| doc.as_str()).collect();
        assert_eq!(docs, ["z", "a", "b", "c"]);
    }

    #[test]
    fn ties_are_settled_per_run_of_equal_scores_not_across_the_list() {
        // The graph-reached drawers of an expanded search carry their own scores and must stay behind the direct hits.
        let settled = settle_ties(ranked(&[("m", 1.0), ("a", 0.5), ("n", 1.0)]));
        let docs: Vec<&str> = settled.iter().map(|(doc, _)| doc.as_str()).collect();
        assert_eq!(docs, ["m", "a", "n"]);
    }
}
