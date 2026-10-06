//! Retrieval evaluation: a reproducible measurement of how well search finds what it should (docs/retrieval-evaluation.md).
//!
//! The harness is a client of a real daemon over HTTP, like an integration (invariant 8), and lives in the test tree
//! so nothing of it ships in the binary or in a release. The small bundled dataset runs in the everyday suite as a
//! regression floor; the explicit entry points (`mise run eval`, ...) print a full report, rewrite the baseline,
//! compare two reports and run a downloaded LongMemEval file. Those are `#[ignore]`d, so `mise run check` never runs them.
//!
//! What a report establishes is bounded by its dataset and its vectors, which it records: see the documentation page
//! before reading a number as more than "this engine, on this data, with these vectors".

mod dataset;
mod longmemeval;
mod metrics;
mod report;
mod run;
mod seed;
mod vectors;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::common::TestDaemon;
use dataset::{Dataset, RUNS};
use metrics::KS;
use report::{DatasetInfo, Environment, Parameters, Report, SCHEMA_VERSION};
use run::{Context, LIMIT, QueryResult};
use vectors::Embedder;

/// How a run is set up.
pub struct Options {
    /// Timed passes per configuration after the warm-up. Zero skips latency entirely.
    pub repetitions: usize,
    /// Filler documents added to read latency at a larger size.
    pub scale: usize,
}

/// A finished evaluation: the report, and the raw results the report was computed from.
pub struct Evaluation {
    pub report: Report,
    pub dataset: Dataset,
    /// Per configuration name, what each query returned.
    pub results: BTreeMap<String, Vec<QueryResult>>,
}

/// Seed a fresh daemon with the dataset at `path` and measure every configuration.
pub async fn evaluate(path: &Path, options: &Options) -> Evaluation {
    let (dataset, sha256) = Dataset::load(path);
    let embedder = Embedder::new(&dataset.concepts);
    // No provider: the vectors are the harness's own, so the daemon's embedding pipeline is not part of what is measured.
    let daemon = TestDaemon::start().await;
    let base = daemon.base_url.clone();
    let seeded = seed::seed(&base, &dataset, &embedder, options.scale).await;

    let context = Context {
        base: &base,
        seeded: &seeded,
        embedder: Some(&embedder),
    };
    let mut runs = Vec::new();
    let mut results = BTreeMap::new();
    for run in &RUNS {
        let outcome = run::execute(&context, &dataset, run, options.repetitions).await;
        results.insert(run.name.to_string(), outcome.results);
        runs.push(outcome.report);
    }

    let http = seed::client();
    let config: Value = http
        .get(format!("{base}/api/config"))
        .send()
        .await
        .expect("config")
        .json()
        .await
        .expect("config json");
    daemon.shutdown().await;

    let report = Report {
        schema_version: SCHEMA_VERSION,
        memcastle_version: env!("CARGO_PKG_VERSION").to_string(),
        dataset: DatasetInfo {
            name: dataset.name.clone(),
            version: dataset.version,
            sha256,
            documents: seeded.doc_of.len(),
            queries: dataset.queries.len(),
        },
        environment: Environment {
            vectors: "fixture: hashed bag of concepts, 768-d, supplied by the harness".into(),
            embedding_provider: config["embeddings"]["provider"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
            embedding_model: config["embeddings"]["model"].as_str().map(str::to_string),
            backend: config["backend"].as_str().unwrap_or("unknown").to_string(),
        },
        parameters: Parameters {
            ks: KS.to_vec(),
            limit: LIMIT,
            repetitions: options.repetitions,
            scale: options.scale,
            skipped_queries: 0,
        },
        ingest: Some(seeded.ingest),
        runs,
    };
    Evaluation {
        report,
        dataset,
        results,
    }
}

/// The committed baseline for the core dataset.
fn committed_baseline() -> Report {
    let path = dataset::baseline_path();
    let json = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "no baseline at {}: {error}. Create it with `mise run eval:baseline`.",
            path.display()
        )
    });
    Report::from_json(&json).unwrap_or_else(|problem| panic!("{}: {problem}", path.display()))
}

/// The everyday regression floor, and the checks that the measurement itself is sound.
mod suite {
    use super::*;

    /// How far a metric may fall below the committed baseline before the suite fails. A margin, not a target: it absorbs
    /// an approximate vector index and the arbitrary order of equal scores without hiding a real regression.
    const TOLERANCE: f64 = 0.02;

    /// One evaluation answers every question below: seeding a daemon is most of the cost, and each check reads the same
    /// result, so they share a test rather than boot three daemons.
    #[tokio::test]
    async fn the_core_evaluation_holds_its_baseline_stays_in_scope_and_still_tells_configurations_apart()
     {
        let evaluation = evaluate(
            &dataset::core_path(),
            &Options {
                repetitions: 0,
                scale: 0,
            },
        )
        .await;
        assert_no_regression_from_the_baseline(&evaluation.report);
        assert_nothing_outside_scope_or_time(&evaluation);
        assert_the_configurations_are_distinguishable(&evaluation.report);
    }

    fn assert_no_regression_from_the_baseline(report: &Report) {
        let comparison = report::compare(&committed_baseline(), report, TOLERANCE);
        assert!(
            comparison.incomparable.is_none(),
            "the baseline was measured on another dataset or with other parameters; refresh it with \
             `mise run eval:baseline`:\n{}",
            comparison.render()
        );
        assert!(
            comparison.unmatched.is_empty(),
            "a configuration was added or removed; refresh the baseline with `mise run eval:baseline`:\n{}",
            comparison.render()
        );
        assert!(
            comparison.regressions().is_empty(),
            "retrieval quality fell below the committed baseline (an intended trade-off is recorded with \
             `mise run eval:baseline`):\n{}",
            comparison.render()
        );
    }

    /// A scoped or temporal query must never return a drawer outside its wing, room or time, whatever the ranking.
    fn assert_nothing_outside_scope_or_time(evaluation: &Evaluation) {
        let index = evaluation.dataset.index();
        for (run, results) in &evaluation.results {
            for result in results {
                let query = evaluation
                    .dataset
                    .queries
                    .iter()
                    .find(|query| query.id == result.query)
                    .expect("a result belongs to a query");
                for (doc, _) in &result.ranked {
                    assert!(
                        !doc.starts_with("unknown:"),
                        "{run}/{}: a hit is not one of the dataset's drawers: {doc}",
                        query.id
                    );
                    assert!(
                        index.in_scope(doc, &query.filter),
                        "{run}/{}: `{doc}` is outside the requested wing and room",
                        query.id
                    );
                    assert!(
                        index.visible(doc, query.temporal.as_ref()),
                        "{run}/{}: `{doc}` was not valid at the time the query asked about",
                        query.id
                    );
                }
            }
        }
    }

    /// These guard the measurement, not the engine's absolute quality: a dataset or harness that stopped separating
    /// lexical from semantic, or plain search from graph expansion, would make every later comparison empty.
    fn assert_the_configurations_are_distinguishable(report: &Report) {
        let recall = |run: &str, category: &str, metric: &str| {
            report.run(run).unwrap().by_category[category][metric]
        };
        assert!(
            recall("semantic", "paraphrase", "recall@5")
                > recall("lexical", "paraphrase", "recall@5"),
            "vector search should find paraphrases that share no words with their source:\n{}",
            report.to_table()
        );
        assert!(
            recall("lexical+expand", "graph", "recall@10")
                > recall("lexical", "graph", "recall@10"),
            "graph expansion should reach the drawers that share an entity with the hit:\n{}",
            report.to_table()
        );
        assert!(
            recall("lexical", "temporal", "recall@5") > 0.9,
            "the temporal filter is what separates a drawer from its correction, so it must find the right version:\n{}",
            report.to_table()
        );
    }
}

/// The entry points a person runs on purpose (`mise run eval`, ...). All `#[ignore]`d: they print, write files, take
/// longer, and some need input.
mod explicit {
    use super::*;

    fn env_usize(name: &str, default: usize) -> usize {
        std::env::var(name).map_or(default, |raw| {
            raw.parse()
                .unwrap_or_else(|_| panic!("{name} must be a non-negative integer, not `{raw}`"))
        })
    }

    fn env_flag(name: &str) -> bool {
        std::env::var(name).is_ok_and(|value| value == "1" || value == "true")
    }

    fn output_path() -> std::path::PathBuf {
        std::env::var("EVAL_OUT").map_or_else(
            |_| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/eval/report.json"),
            Into::into,
        )
    }

    fn write(path: &Path, report: &Report) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create the output directory");
        }
        std::fs::write(path, report.to_json()).expect("write the report");
        println!("report written to {}", path.display());
    }

    #[tokio::test]
    #[ignore = "an explicit run: `mise run eval`"]
    async fn an_explicit_run_prints_a_report_and_writes_it_as_json() {
        let path = std::env::var("EVAL_DATASET").map_or_else(|_| dataset::core_path(), Into::into);
        let options = Options {
            repetitions: env_usize("EVAL_REPETITIONS", 5),
            scale: env_usize("EVAL_SCALE", 0),
        };
        let evaluation = evaluate(&path, &options).await;
        println!("\n{}", evaluation.report.to_table());
        write(&output_path(), &evaluation.report);

        if env_flag("EVAL_UPDATE_BASELINE") {
            // A baseline measured on filler or on another dataset would make every later comparison meaningless.
            assert!(
                path == dataset::core_path() && options.scale == 0,
                "the baseline is for the bundled dataset at scale 0: unset EVAL_DATASET and EVAL_SCALE"
            );
            write(
                &dataset::baseline_path(),
                &evaluation.report.without_timings(),
            );
        }
    }

    #[test]
    #[ignore = "an explicit run: `mise run eval:compare -- <base> <new>`"]
    fn an_explicit_compare_prints_what_changed_between_two_reports() {
        let read = |variable: &str| {
            let path = std::env::var(variable)
                .unwrap_or_else(|_| panic!("set {variable} to a report file"));
            let json = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {path}: {error}"));
            Report::from_json(&json).unwrap_or_else(|problem| panic!("{path}: {problem}"))
        };
        let threshold: f64 = std::env::var("EVAL_THRESHOLD").map_or(0.02, |raw| {
            raw.parse()
                .unwrap_or_else(|_| panic!("EVAL_THRESHOLD must be a number, not `{raw}`"))
        });
        let comparison = report::compare(&read("EVAL_BASE"), &read("EVAL_NEW"), threshold);
        println!("\n{}", comparison.render());
        if env_flag("EVAL_FAIL_ON_REGRESSION") {
            assert!(
                comparison.incomparable.is_none() && comparison.regressions().is_empty(),
                "the new report is not at least as good as the base"
            );
        }
    }

    #[tokio::test]
    #[ignore = "an explicit run: `mise run eval:longmemeval -- <file>`"]
    async fn an_explicit_longmemeval_run_measures_session_retrieval_over_a_downloaded_file() {
        let path = std::env::var("EVAL_LONGMEMEVAL")
            .expect("set EVAL_LONGMEMEVAL to a LongMemEval JSON file");
        let limit = std::env::var("EVAL_LONGMEMEVAL_LIMIT").ok().map(|raw| {
            raw.parse().unwrap_or_else(|_| {
                panic!("EVAL_LONGMEMEVAL_LIMIT must be an integer, not `{raw}`")
            })
        });
        let report = longmemeval::evaluate(
            Path::new(&path),
            limit,
            longmemeval::Provider::from_env(),
            env_usize("EVAL_REPETITIONS", 1),
        )
        .await;
        println!("\n{}", report.to_table());
        write(&output_path(), &report);
    }
}
