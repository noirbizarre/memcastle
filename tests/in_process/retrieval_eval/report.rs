//! The evaluation report: what was measured, on what, and how two reports compare.
//!
//! A report is JSON so a run can be saved, committed (the baseline) and diffed against another. It records the
//! configuration alongside the numbers, because a number without its dataset, vectors and parameters cannot be
//! interpreted a month later.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::metrics::{Latency, Metrics};

/// Bumped when a report's shape changes in a way an older reader would misread.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub name: String,
    pub version: u32,
    /// SHA-256 of the dataset file's bytes: two reports are only comparable when this matches.
    pub sha256: String,
    pub documents: usize,
    pub queries: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    /// Where the vectors came from: the harness's fixed stand-in model, or a provider and its model.
    pub vectors: String,
    pub embedding_provider: String,
    pub embedding_model: Option<String>,
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameters {
    pub ks: Vec<usize>,
    /// The most drawers any configuration returns.
    pub limit: usize,
    /// Timed passes over the query set per configuration, after one untimed warm-up pass.
    pub repetitions: usize,
    /// Synthetic filler documents added so latency can be read at a larger size.
    pub scale: usize,
    /// Questions left out because no metric is defined for them (LongMemEval's abstention questions).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped_queries: usize,
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ingest {
    pub documents: usize,
    pub documents_per_second: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub name: String,
    pub ranking: String,
    pub expand: bool,
    pub queries: usize,
    pub metrics: Metrics,
    pub by_category: BTreeMap<String, Metrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency: Option<Latency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub memcastle_version: String,
    pub dataset: DatasetInfo,
    pub environment: Environment,
    pub parameters: Parameters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest: Option<Ingest>,
    pub runs: Vec<RunReport>,
}

impl Report {
    /// This report without anything that depends on the machine it ran on.
    ///
    /// Quality metrics are a property of the engine and the dataset; latency and throughput are a property of the
    /// hardware too. A committed baseline holds only the first kind, so it never goes stale on a faster laptop.
    pub fn without_timings(mut self) -> Self {
        self.ingest = None;
        // Repetitions only exist to time requests, so a baseline that records them would differ with the run that made it.
        self.parameters.repetitions = 0;
        for run in &mut self.runs {
            run.latency = None;
        }
        self
    }

    pub fn run(&self, name: &str) -> Option<&RunReport> {
        self.runs.iter().find(|run| run.name == name)
    }

    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("a report serialises");
        json.push('\n');
        json
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        let report: Self =
            serde_json::from_str(json).map_err(|error| format!("not a report: {error}"))?;
        if report.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "report schema {} is not the one this harness reads ({SCHEMA_VERSION})",
                report.schema_version
            ));
        }
        Ok(report)
    }

    /// A human-readable table: quality per configuration, then per category, then latency.
    pub fn to_table(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "dataset {} v{} ({} documents, {} queries) sha256 {}",
            self.dataset.name,
            self.dataset.version,
            self.dataset.documents,
            self.dataset.queries,
            &self.dataset.sha256[..12.min(self.dataset.sha256.len())]
        );
        let _ = writeln!(
            out,
            "memcastle {} | backend {} | vectors: {} | scale {} | repetitions {}",
            self.memcastle_version,
            self.environment.backend,
            self.environment.vectors,
            self.parameters.scale,
            self.parameters.repetitions
        );
        let columns = [
            "recall@5".to_string(),
            "recall@10".to_string(),
            "precision@5".to_string(),
            "ndcg@10".to_string(),
            "hit@5".to_string(),
            "mrr".to_string(),
        ];

        let _ = writeln!(out, "\nquality, every query of each configuration");
        header(&mut out, "configuration", &columns);
        for run in &self.runs {
            row(
                &mut out,
                &format!("{} ({})", run.name, run.queries),
                &run.metrics,
                &columns,
            );
        }
        let categories: Vec<&String> = self
            .runs
            .iter()
            .flat_map(|run| run.by_category.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for category in categories {
            let _ = writeln!(out, "\nquality, {category} queries");
            header(&mut out, "configuration", &columns);
            for run in &self.runs {
                if let Some(metrics) = run.by_category.get(category) {
                    row(&mut out, &run.name, metrics, &columns);
                }
            }
        }
        if self.runs.iter().any(|run| run.latency.is_some()) {
            let _ = writeln!(
                out,
                "\nlatency per search request (ms) and sequential throughput"
            );
            let _ = writeln!(
                out,
                "{:<18}{:>9}{:>9}{:>9}{:>9}{:>10}",
                "configuration", "mean", "p50", "p95", "p99", "queries/s"
            );
            for run in &self.runs {
                if let Some(l) = &run.latency {
                    let _ = writeln!(
                        out,
                        "{:<18}{:>9.2}{:>9.2}{:>9.2}{:>9.2}{:>10.1}",
                        run.name, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.queries_per_second
                    );
                }
            }
        }
        if let Some(ingest) = &self.ingest {
            let _ = writeln!(
                out,
                "\ningest: {} documents at {:.1} documents/s (write plus vector attach, over HTTP)",
                ingest.documents, ingest.documents_per_second
            );
        }
        out
    }
}

fn header(out: &mut String, first: &str, columns: &[String]) {
    let _ = write!(out, "{first:<22}");
    for column in columns {
        let _ = write!(out, "{column:>13}");
    }
    out.push('\n');
}

fn row(out: &mut String, label: &str, metrics: &Metrics, columns: &[String]) {
    let _ = write!(out, "{label:<22}");
    for column in columns {
        match metrics.get(column) {
            Some(value) => {
                let _ = write!(out, "{value:>13.3}");
            }
            None => {
                let _ = write!(out, "{:>13}", "-");
            }
        }
    }
    out.push('\n');
}

/// One metric of one configuration in one scope, before and after.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub run: String,
    /// `overall` or a category name.
    pub scope: String,
    pub metric: String,
    pub base: f64,
    pub new: f64,
}

impl Delta {
    pub fn change(&self) -> f64 {
        self.new - self.base
    }
}

/// The result of comparing a new report against a base one.
#[derive(Debug)]
pub struct Comparison {
    pub deltas: Vec<Delta>,
    /// Why the two cannot be compared, when they cannot: other dataset, other parameters.
    pub incomparable: Option<String>,
    /// Configurations present in only one report. They are listed, never silently dropped.
    pub unmatched: Vec<String>,
    pub threshold: f64,
}

impl Comparison {
    /// The metrics that fell by more than the threshold.
    pub fn regressions(&self) -> Vec<&Delta> {
        self.deltas
            .iter()
            .filter(|delta| delta.change() < -self.threshold)
            .collect()
    }

    /// Only the metrics that changed, worst first, as text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(why) = &self.incomparable {
            let _ = writeln!(out, "NOT COMPARABLE: {why}");
        }
        for name in &self.unmatched {
            let _ = writeln!(out, "only in one report: {name}");
        }
        let mut changed: Vec<&Delta> = self
            .deltas
            .iter()
            .filter(|delta| delta.change().abs() > 1e-9)
            .collect();
        changed.sort_by(|a, b| a.change().total_cmp(&b.change()));
        if changed.is_empty() {
            let _ = writeln!(out, "no metric changed ({} compared)", self.deltas.len());
        }
        for delta in changed {
            let flag = if delta.change() < -self.threshold {
                "REGRESSION"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{:<14}{:<12}{:<16}{:>8.3} -> {:<8.3}{:>+8.3}  {flag}",
                delta.run,
                delta.scope,
                delta.metric,
                delta.base,
                delta.new,
                delta.change()
            );
        }
        let _ = writeln!(
            out,
            "{} metric(s) compared, {} regression(s) beyond {:.3}",
            self.deltas.len(),
            self.regressions().len(),
            self.threshold
        );
        out
    }
}

/// Compare `new` with `base`, metric by metric, for the configurations both contain.
///
/// Reports of different datasets (by hash) or different limits are flagged as incomparable rather than compared:
/// the difference would be the data, not the engine.
pub fn compare(base: &Report, new: &Report, threshold: f64) -> Comparison {
    let mut comparison = Comparison {
        deltas: Vec::new(),
        incomparable: None,
        unmatched: Vec::new(),
        threshold,
    };
    if base.dataset.sha256 != new.dataset.sha256 {
        comparison.incomparable = Some(format!(
            "the datasets differ ({} v{} against {} v{}), so a difference may be the data",
            base.dataset.name, base.dataset.version, new.dataset.name, new.dataset.version
        ));
    } else if base.parameters.limit != new.parameters.limit
        || base.parameters.scale != new.parameters.scale
    {
        comparison.incomparable = Some(format!(
            "the parameters differ (limit {} scale {} against limit {} scale {})",
            base.parameters.limit,
            base.parameters.scale,
            new.parameters.limit,
            new.parameters.scale
        ));
    }
    for run in &base.runs {
        let Some(other) = new.run(&run.name) else {
            comparison
                .unmatched
                .push(format!("{} (base only)", run.name));
            continue;
        };
        collect(
            &mut comparison.deltas,
            &run.name,
            "overall",
            &run.metrics,
            &other.metrics,
        );
        for (category, metrics) in &run.by_category {
            if let Some(other_metrics) = other.by_category.get(category) {
                collect(
                    &mut comparison.deltas,
                    &run.name,
                    category,
                    metrics,
                    other_metrics,
                );
            }
        }
    }
    for run in &new.runs {
        if base.run(&run.name).is_none() {
            comparison
                .unmatched
                .push(format!("{} (new only)", run.name));
        }
    }
    comparison
}

fn collect(deltas: &mut Vec<Delta>, run: &str, scope: &str, base: &Metrics, new: &Metrics) {
    for (metric, base_value) in base {
        if let Some(new_value) = new.get(metric) {
            deltas.push(Delta {
                run: run.to_string(),
                scope: scope.to_string(),
                metric: metric.clone(),
                base: *base_value,
                new: *new_value,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(recall: f64) -> Report {
        let metrics = Metrics::from([("recall@5".to_string(), recall)]);
        Report {
            schema_version: SCHEMA_VERSION,
            memcastle_version: "0".into(),
            dataset: DatasetInfo {
                name: "d".into(),
                version: 1,
                sha256: "abc".into(),
                documents: 1,
                queries: 1,
            },
            environment: Environment {
                vectors: "v".into(),
                embedding_provider: "none".into(),
                embedding_model: None,
                backend: "embedded".into(),
            },
            parameters: Parameters {
                ks: vec![5],
                limit: 10,
                repetitions: 0,
                scale: 0,
                skipped_queries: 0,
            },
            ingest: Some(Ingest {
                documents: 1,
                documents_per_second: 1.0,
            }),
            runs: vec![RunReport {
                name: "lexical".into(),
                ranking: "lexical".into(),
                expand: false,
                queries: 1,
                metrics: metrics.clone(),
                by_category: BTreeMap::from([("lexical".to_string(), metrics)]),
                latency: Some(Latency {
                    samples: 1,
                    mean_ms: 1.0,
                    p50_ms: 1.0,
                    p95_ms: 1.0,
                    p99_ms: 1.0,
                    queries_per_second: 1.0,
                }),
            }],
        }
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let original = report(0.5);
        let back = Report::from_json(&original.to_json()).expect("parses");
        assert_eq!(back.runs[0].metrics, original.runs[0].metrics);
        assert_eq!(back.dataset.sha256, "abc");
    }

    #[test]
    fn a_baseline_carries_no_machine_dependent_numbers() {
        let stripped = report(0.5).without_timings();
        assert!(stripped.ingest.is_none());
        assert_eq!(stripped.parameters.repetitions, 0);
        assert!(stripped.runs[0].latency.is_none());
        assert!(!stripped.to_json().contains("latency"));
    }

    #[test]
    fn a_report_of_another_schema_is_refused_with_both_versions() {
        let mut other = report(0.5);
        other.schema_version = 99;
        let problem = Report::from_json(&other.to_json()).expect_err("refused");
        assert!(problem.contains("99") && problem.contains('1'), "{problem}");
    }

    #[test]
    fn a_drop_beyond_the_threshold_is_a_regression_and_a_smaller_one_is_not() {
        let comparison = compare(&report(0.80), &report(0.70), 0.02);
        assert_eq!(
            comparison.regressions().len(),
            2,
            "overall and the category"
        );
        assert!(comparison.render().contains("REGRESSION"));
        assert!(
            compare(&report(0.80), &report(0.79), 0.02)
                .regressions()
                .is_empty()
        );
        assert!(
            compare(&report(0.80), &report(0.95), 0.02)
                .regressions()
                .is_empty(),
            "an improvement is not one"
        );
    }

    #[test]
    fn reports_of_different_datasets_are_flagged_instead_of_compared_silently() {
        let mut other = report(0.8);
        other.dataset.sha256 = "different".into();
        let comparison = compare(&report(0.8), &other, 0.02);
        assert!(comparison.incomparable.is_some());
        assert!(comparison.render().contains("NOT COMPARABLE"));
    }

    #[test]
    fn a_configuration_in_only_one_report_is_listed() {
        let mut other = report(0.8);
        other.runs[0].name = "semantic".into();
        let comparison = compare(&report(0.8), &other, 0.02);
        assert_eq!(comparison.unmatched.len(), 2);
        assert!(comparison.deltas.is_empty());
    }

    #[test]
    fn the_table_names_each_configuration_and_its_metrics() {
        let table = report(0.5).to_table();
        assert!(table.contains("lexical"), "{table}");
        assert!(table.contains("recall@5"), "{table}");
        assert!(table.contains("queries/s"), "{table}");
    }
}
