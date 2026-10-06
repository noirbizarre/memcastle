//! Retrieval-quality metrics and latency statistics.
//!
//! Every function here is pure, so the definitions the documentation promises are the ones the tests pin down.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// The cut-offs a report measures at.
pub const KS: [usize; 4] = [1, 3, 5, 10];

/// Named metrics: `recall@5`, `precision@5`, `hit@5`, `all@5`, `ndcg@5` and `mrr`.
///
/// A sorted map keeps a report's JSON stable, so a committed baseline diffs line by line.
pub type Metrics = BTreeMap<String, f64>;

/// Score one ranked list of document ids against graded judgments.
///
/// - `recall@k`: the share of relevant documents among the first `k`.
/// - `precision@k`: the share of the first `k` slots that are relevant. It divides by `k`, not by how many were
///   returned, so a search that returns nothing scores 0 rather than being excused.
/// - `hit@k`: 1 when any relevant document is among the first `k` (LongMemEval's `recall_any`).
/// - `all@k`: 1 when every relevant document is (LongMemEval's `recall_all`).
/// - `ndcg@k`: discounted cumulative gain with gain `2^grade - 1`, over the best ordering possible.
/// - `mrr`: the reciprocal of the rank of the first relevant document anywhere in the list.
pub fn score_query(ranked: &[String], grades: &HashMap<String, u32>, ks: &[usize]) -> Metrics {
    // A document that appears twice would otherwise be counted twice by recall and precision.
    let mut seen = HashSet::new();
    let ranked: Vec<&String> = ranked.iter().filter(|doc| seen.insert(*doc)).collect();
    let mut metrics = Metrics::new();
    for &k in ks {
        let top = &ranked[..ranked.len().min(k)];
        let found = top.iter().filter(|doc| grades.contains_key(**doc)).count();
        metrics.insert(format!("recall@{k}"), found as f64 / grades.len() as f64);
        metrics.insert(format!("precision@{k}"), found as f64 / k as f64);
        metrics.insert(format!("hit@{k}"), f64::from(found > 0));
        metrics.insert(format!("all@{k}"), f64::from(found == grades.len()));
        let dcg: f64 = top
            .iter()
            .enumerate()
            .map(|(index, doc)| gain(grades.get(*doc).copied().unwrap_or(0)) / discount(index))
            .sum();
        let mut ideal: Vec<u32> = grades.values().copied().collect();
        ideal.sort_unstable_by(|a, b| b.cmp(a));
        let ideal_dcg: f64 = ideal
            .iter()
            .take(k)
            .enumerate()
            .map(|(index, grade)| gain(*grade) / discount(index))
            .sum();
        metrics.insert(
            format!("ndcg@{k}"),
            if ideal_dcg > 0.0 {
                dcg / ideal_dcg
            } else {
                0.0
            },
        );
    }
    let first = ranked.iter().position(|doc| grades.contains_key(*doc));
    metrics.insert(
        "mrr".into(),
        first.map_or(0.0, |index| 1.0 / (index + 1) as f64),
    );
    metrics
}

fn gain(grade: u32) -> f64 {
    f64::from(2u32.pow(grade) - 1)
}

/// The rank discount `log2(rank + 1)` for a zero-based position.
fn discount(position: usize) -> f64 {
    (position as f64 + 2.0).log2()
}

/// The mean of each metric over `per_query`, which is how a report summarises a set of queries.
pub fn mean(per_query: &[&Metrics]) -> Metrics {
    let mut sums = Metrics::new();
    for metrics in per_query {
        for (name, value) in *metrics {
            *sums.entry(name.clone()).or_default() += value;
        }
    }
    let count = per_query.len().max(1) as f64;
    sums.values_mut().for_each(|sum| *sum /= count);
    sums
}

/// Latency of one retrieval configuration, from the client's side of the HTTP call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Latency {
    pub samples: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    /// Requests completed per second, one at a time (not a concurrency benchmark).
    pub queries_per_second: f64,
}

impl Latency {
    /// Summarise `samples_ms` (any order) taken over `wall_seconds` of back-to-back requests.
    pub fn from_samples(samples_ms: &[f64], wall_seconds: f64) -> Option<Self> {
        if samples_ms.is_empty() {
            return None;
        }
        let mut sorted = samples_ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        Some(Self {
            samples: sorted.len(),
            mean_ms: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p50_ms: percentile(&sorted, 50.0),
            p95_ms: percentile(&sorted, 95.0),
            p99_ms: percentile(&sorted, 99.0),
            queries_per_second: if wall_seconds > 0.0 {
                sorted.len() as f64 / wall_seconds
            } else {
                0.0
            },
        })
    }
}

/// The nearest-rank percentile of an ascending list: the smallest value that at least `p` percent of samples do not
/// exceed. Nearest rank always returns a value that was measured, which is what a latency figure should be.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grades(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs
            .iter()
            .map(|(doc, grade)| ((*doc).to_string(), *grade))
            .collect()
    }

    fn ranked(docs: &[&str]) -> Vec<String> {
        docs.iter().map(|doc| (*doc).to_string()).collect()
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn recall_counts_the_relevant_documents_found_in_the_first_k() {
        let metrics = score_query(
            &ranked(&["x", "a", "y", "b"]),
            &grades(&[("a", 2), ("b", 2), ("c", 2)]),
            &KS,
        );
        close(metrics["recall@1"], 0.0);
        close(metrics["recall@3"], 1.0 / 3.0);
        close(metrics["recall@5"], 2.0 / 3.0);
    }

    #[test]
    fn precision_divides_by_k_even_when_fewer_documents_came_back() {
        let metrics = score_query(&ranked(&["a"]), &grades(&[("a", 2)]), &KS);
        close(metrics["precision@1"], 1.0);
        close(metrics["precision@5"], 0.2);
    }

    #[test]
    fn hit_and_all_tell_any_relevant_from_every_relevant() {
        let metrics = score_query(
            &ranked(&["a", "x", "b"]),
            &grades(&[("a", 2), ("b", 2)]),
            &KS,
        );
        close(metrics["hit@1"], 1.0);
        close(metrics["all@1"], 0.0);
        close(metrics["all@3"], 1.0);
    }

    #[test]
    fn mrr_is_the_reciprocal_rank_of_the_first_relevant_document() {
        let metrics = score_query(&ranked(&["x", "y", "a"]), &grades(&[("a", 1)]), &KS);
        close(metrics["mrr"], 1.0 / 3.0);
        let none = score_query(&ranked(&["x"]), &grades(&[("a", 1)]), &KS);
        close(none["mrr"], 0.0);
    }

    #[test]
    fn ndcg_rewards_a_relevant_document_ranked_first_over_ranked_third() {
        let relevant = grades(&[("a", 2)]);
        let first = score_query(&ranked(&["a", "x", "y"]), &relevant, &KS);
        let third = score_query(&ranked(&["x", "y", "a"]), &relevant, &KS);
        close(first["ndcg@3"], 1.0);
        // gain 3 at rank 3 is discounted by log2(4) = 2, against 1 for the ideal rank.
        close(third["ndcg@3"], 0.5);
    }

    #[test]
    fn ndcg_prefers_the_answer_above_a_merely_related_document() {
        let relevant = grades(&[("answer", 2), ("related", 1)]);
        let good = score_query(&ranked(&["answer", "related"]), &relevant, &KS);
        let bad = score_query(&ranked(&["related", "answer"]), &relevant, &KS);
        close(good["ndcg@3"], 1.0);
        assert!(bad["ndcg@3"] < 1.0);
    }

    #[test]
    fn a_document_returned_twice_is_counted_once() {
        let metrics = score_query(
            &ranked(&["a", "a", "a"]),
            &grades(&[("a", 2), ("b", 2)]),
            &KS,
        );
        close(metrics["recall@3"], 0.5);
        close(metrics["precision@3"], 1.0 / 3.0);
    }

    #[test]
    fn the_mean_averages_each_metric_over_the_queries() {
        let one = score_query(&ranked(&["a"]), &grades(&[("a", 2)]), &KS);
        let two = score_query(&ranked(&["x"]), &grades(&[("a", 2)]), &KS);
        close(mean(&[&one, &two])["recall@1"], 0.5);
    }

    #[test]
    fn latency_percentiles_are_nearest_rank_values_that_were_measured() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        let latency = Latency::from_samples(&samples, 2.0).expect("samples");
        close(latency.p50_ms, 50.0);
        close(latency.p95_ms, 95.0);
        close(latency.p99_ms, 99.0);
        close(latency.mean_ms, 50.5);
        close(latency.queries_per_second, 50.0);
        assert!(Latency::from_samples(&[], 1.0).is_none());
    }
}
