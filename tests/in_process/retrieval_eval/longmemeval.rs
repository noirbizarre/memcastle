//! A LongMemEval-compatible path: session-level retrieval over a file the user downloaded.
//!
//! LongMemEval (<https://github.com/xiaowu0162/LongMemEval>) is a third-party benchmark of long-term chat memory. Its
//! data is not bundled here and never will be: it is large, and its licence and availability are the benchmark's own, to
//! be checked by whoever downloads it. This module reads the file format and nothing else.
//!
//! Each question comes with its own haystack of chat sessions and the ids of the sessions that hold the answer. The
//! mapping is: one drawer per session (its turns, in order, as `role: content` lines), in a wing of its own per question
//! so that a question searches only its haystack, and the question is the query. A hit is a session, so the metrics are
//! session-level retrieval metrics: `hit@k` is LongMemEval's `recall_any@k` and `all@k` its `recall_all@k`.
//!
//! What this measures is retrieval of the right *session*. It does not answer the questions, so it is not a
//! LongMemEval score and says nothing about end-to-end answer accuracy. Abstention questions (no answer session) have no
//! retrieval metric and are skipped, and the count is reported.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use memcastle::config::{EmbeddingProvider, Secret};
use memcastle::domain::{SearchFilter, SearchQuery};
use serde::Deserialize;
use serde_json::{Value, json};

use super::dataset::RUNS;
use super::metrics::KS;
use super::report::{DatasetInfo, Environment, Ingest, Parameters, Report, SCHEMA_VERSION};
use super::run::{Case, LIMIT, execute_cases};
use super::seed::{client, create};
use crate::common::TestDaemon;

#[derive(Debug, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct Question {
    pub question_id: String,
    #[serde(default)]
    pub question_type: String,
    pub question: String,
    pub haystack_session_ids: Vec<String>,
    pub haystack_sessions: Vec<Vec<Turn>>,
    #[serde(default)]
    pub answer_session_ids: Vec<String>,
}

/// A question mapped onto the palace: the drawers to write and the judgment to score with.
#[derive(Debug)]
pub struct Converted {
    pub wing: String,
    /// `(session id, drawer content)`.
    pub sessions: Vec<(String, String)>,
    pub question_id: String,
    pub category: String,
    pub text: String,
    pub answers: Vec<String>,
}

/// A provider to embed with, for the semantic and hybrid configurations.
#[derive(Debug, Clone)]
pub struct Provider {
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl Provider {
    /// From `EVAL_EMBEDDINGS_URL`, `EVAL_EMBEDDINGS_MODEL` and optionally `EVAL_EMBEDDINGS_API_KEY`.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("EVAL_EMBEDDINGS_URL").ok()?;
        let model = std::env::var("EVAL_EMBEDDINGS_MODEL")
            .expect("EVAL_EMBEDDINGS_MODEL must name the model when EVAL_EMBEDDINGS_URL is set: a report without it cannot be interpreted");
        Some(Self {
            url,
            model,
            api_key: std::env::var("EVAL_EMBEDDINGS_API_KEY").ok(),
        })
    }
}

/// A session as one drawer's text.
pub fn session_text(turns: &[Turn]) -> String {
    turns
        .iter()
        .map(|turn| format!("{}: {}", turn.role, turn.content))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Map `questions` onto the palace, skipping abstention questions. Returns them with how many were skipped.
pub fn convert(questions: &[Question]) -> (Vec<Converted>, usize) {
    let mut converted = Vec::new();
    let mut skipped = 0;
    for question in questions {
        // An abstention question has no answer session, so recall is undefined; LongMemEval marks it with `_abs`.
        if question.answer_session_ids.is_empty() || question.question_id.ends_with("_abs") {
            skipped += 1;
            continue;
        }
        let sessions = question
            .haystack_session_ids
            .iter()
            .zip(&question.haystack_sessions)
            .map(|(id, turns)| (id.clone(), session_text(turns)))
            // A drawer must have content, and an empty session cannot be an answer.
            .filter(|(_, text)| !text.trim().is_empty())
            .collect();
        converted.push(Converted {
            wing: format!("lme-{}", question.question_id),
            sessions,
            question_id: question.question_id.clone(),
            category: if question.question_type.is_empty() {
                "unknown".into()
            } else {
                question.question_type.clone()
            },
            text: question.question.clone(),
            answers: question.answer_session_ids.clone(),
        });
    }
    (converted, skipped)
}

/// Read a LongMemEval file, with the SHA-256 of its bytes.
pub fn load(path: &Path) -> (Vec<Question>, String) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| {
        panic!(
            "cannot read the LongMemEval file {}: {error}",
            path.display()
        )
    });
    let questions = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "{} is not a LongMemEval file (a JSON array of questions with haystack_sessions and answer_session_ids): {error}",
            path.display()
        )
    });
    (questions, memcastle::domain::sha256_hex(&bytes))
}

/// Ingest the file into a fresh daemon and measure session retrieval under each configuration it can run.
///
/// Lexical always runs. Semantic and hybrid run when `provider` is given, which the daemon then uses to embed every
/// session and every question: that cost is the provider's, and the report records which provider and model it was.
pub async fn evaluate(
    path: &Path,
    limit: Option<usize>,
    provider: Option<Provider>,
    repetitions: usize,
) -> Report {
    let (questions, sha256) = load(path);
    let (mut converted, skipped) = convert(&questions);
    if let Some(limit) = limit {
        converted.truncate(limit);
    }
    assert!(
        !converted.is_empty(),
        "no question in {} has an answer session to retrieve",
        path.display()
    );

    let configured = provider.clone();
    let daemon = TestDaemon::start_configured(move |config| {
        if let Some(provider) = configured {
            config.embeddings.provider = EmbeddingProvider::Http;
            config.embeddings.url = Some(provider.url);
            config.embeddings.model = Some(provider.model);
            config.embeddings.api_key = provider.api_key.map(Secret::new);
            // A long session is a long text and a real model can be slow on it.
            config.embeddings.timeout_secs = 300;
        }
    })
    .await;
    let base = daemon.base_url.clone();
    let http = client();

    let started = std::time::Instant::now();
    let mut doc_of = HashMap::new();
    let mut sessions = 0usize;
    for question in &converted {
        for (id, text) in &question.sessions {
            // A session repeated within a haystack is stored once, and the later id wins: it is the same text either way.
            let (drawer, _) = create(&http, &base, &question.wing, "sessions", text).await;
            doc_of.insert(drawer, id.clone());
            sessions += 1;
        }
    }
    let ingest = Ingest {
        documents: sessions,
        documents_per_second: sessions as f64 / started.elapsed().as_secs_f64().max(f64::EPSILON),
    };
    if provider.is_some() {
        wait_for_embeddings(&http, &base).await;
    }

    let mut runs = Vec::new();
    for run in RUNS
        .iter()
        .filter(|run| !run.expand && (provider.is_some() || run.name == "lexical"))
    {
        let cases: Vec<Case> = converted
            .iter()
            .map(|question| Case {
                id: question.question_id.clone(),
                category: question.category.clone(),
                request: SearchQuery {
                    text: question.text.clone(),
                    ranking: run.ranking,
                    limit: u32::try_from(LIMIT).expect("a small limit"),
                    filter: SearchFilter {
                        wing: Some(question.wing.clone()),
                        ..SearchFilter::default()
                    },
                    // The daemon embeds the question with the provider, like any client that has no model of its own.
                    ..SearchQuery::default()
                },
                grades: question.answers.iter().map(|id| (id.clone(), 2)).collect(),
            })
            .collect();
        runs.push(
            execute_cases(&base, &doc_of, run, &cases, repetitions)
                .await
                .report,
        );
    }

    let config: Value = http
        .get(format!("{base}/api/config"))
        .send()
        .await
        .expect("config")
        .json()
        .await
        .expect("config json");
    daemon.shutdown().await;

    Report {
        schema_version: SCHEMA_VERSION,
        memcastle_version: env!("CARGO_PKG_VERSION").to_string(),
        dataset: DatasetInfo {
            name: format!(
                "longmemeval:{}",
                path.file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
            ),
            version: 0,
            sha256,
            documents: sessions,
            queries: converted.len(),
        },
        environment: Environment {
            vectors: provider.as_ref().map_or_else(
                || "none (lexical only)".to_string(),
                |p| format!("provider {}", p.model),
            ),
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
            repetitions,
            scale: 0,
            skipped_queries: skipped,
        },
        ingest: Some(ingest),
        runs,
    }
}

/// Run an embed job and wait until the daemon has nothing left to do, so every session is searchable by meaning.
async fn wait_for_embeddings(http: &reqwest::Client, base: &str) {
    let submitted = http
        .post(format!("{base}/api/jobs"))
        .json(&json!({ "type": "embed" }))
        .send()
        .await
        .expect("submit embed job");
    assert!(
        submitted.status().is_success(),
        "the embed job was refused: {}",
        submitted.status()
    );
    // A real provider embeds thousands of sessions: this is minutes to hours, not the seconds the stub takes.
    for _ in 0..(4 * 3600) {
        let jobs: Vec<Value> = http
            .get(format!("{base}/api/jobs"))
            .send()
            .await
            .expect("list jobs")
            .json()
            .await
            .expect("jobs json");
        assert!(
            !jobs.iter().any(|job| job["status"] == "failed"),
            "an embedding job failed, so semantic results would silently cover only part of the corpus: {jobs:?}"
        );
        let busy = |job: &&Value| {
            matches!(
                job["status"].as_str(),
                Some("queued" | "running" | "paused")
            )
        };
        if jobs.iter().filter(busy).count() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("the embedding jobs did not finish within four hours");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/retrieval/longmemeval-synthetic.json")
    }

    #[test]
    fn the_converter_files_each_session_as_a_drawer_in_a_wing_of_its_question() {
        let (questions, _) = load(&synthetic());
        let (converted, skipped) = convert(&questions);
        assert_eq!(
            skipped, 1,
            "the abstention question has nothing to retrieve"
        );
        assert_eq!(converted.len(), 2);
        let first = &converted[0];
        assert_eq!(first.wing, "lme-syn001");
        assert_eq!(first.sessions.len(), 3);
        assert_eq!(first.answers, ["syn001_b"]);
        assert_eq!(first.category, "single-session-user");
    }

    #[test]
    fn a_session_keeps_its_turns_in_order_with_their_roles() {
        let turns = vec![
            Turn {
                role: "user".into(),
                content: "hello".into(),
            },
            Turn {
                role: "assistant".into(),
                content: "hi".into(),
            },
        ];
        assert_eq!(session_text(&turns), "user: hello\nassistant: hi");
    }

    #[test]
    fn a_question_with_several_answer_sessions_judges_each_of_them() {
        let (questions, _) = load(&synthetic());
        let (converted, _) = convert(&questions);
        assert_eq!(converted[1].answers, ["syn002_a", "syn002_c"]);
    }

    #[tokio::test]
    async fn a_longmemeval_file_can_be_ingested_and_searched_lexically_end_to_end() {
        let report = evaluate(&synthetic(), None, None, 0).await;
        assert_eq!(report.dataset.queries, 2);
        assert_eq!(report.dataset.documents, 6);
        assert_eq!(
            report.runs.len(),
            1,
            "no provider, so only the lexical configuration runs"
        );
        let lexical = &report.runs[0];
        assert_eq!(lexical.name, "lexical");
        // Three sessions per question, so every answer session is within the first five: a lower hit rate means
        // the question was never searched in its own wing.
        assert!(
            (lexical.metrics["hit@5"] - 1.0).abs() < 1e-9,
            "{:?}",
            lexical.metrics
        );
        assert!(
            (lexical.metrics["all@5"] - 1.0).abs() < 1e-9,
            "{:?}",
            lexical.metrics
        );
        assert!(lexical.by_category.contains_key("multi-session"));
    }

    /// Serve `/v1/embeddings` on an ephemeral port with the harness's own vectors: a provider with no model behind it.
    async fn embedding_stub() -> String {
        use axum::{Json, Router, routing::post};
        let embedder = super::super::vectors::Embedder::new(&std::collections::BTreeMap::new());
        let app = Router::new().route(
            "/v1/embeddings",
            post(move |Json(body): Json<Value>| {
                let embedder = embedder.clone();
                async move {
                    let data: Vec<Value> = body["input"]
                        .as_array()
                        .expect("input array")
                        .iter()
                        .enumerate()
                        .map(|(index, text)| {
                            json!({ "index": index, "embedding": embedder.embed(text.as_str().unwrap_or_default()) })
                        })
                        .collect();
                    Json(json!({ "data": data }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://127.0.0.1:{port}/v1")
    }

    #[tokio::test]
    async fn with_a_provider_the_semantic_and_hybrid_configurations_run_and_the_report_names_the_model()
     {
        let provider = Provider {
            url: embedding_stub().await,
            model: "stub-model".into(),
            api_key: None,
        };
        let report = evaluate(&synthetic(), None, Some(provider), 0).await;
        let names: Vec<&str> = report.runs.iter().map(|run| run.name.as_str()).collect();
        assert_eq!(names, ["lexical", "semantic", "hybrid"]);
        assert_eq!(report.environment.embedding_provider, "http");
        assert_eq!(
            report.environment.embedding_model.as_deref(),
            Some("stub-model")
        );
        assert!(
            report.environment.vectors.contains("stub-model"),
            "{}",
            report.environment.vectors
        );
        // Every session was embedded before the first search, or a semantic search would find fewer than three.
        let semantic = report.run("semantic").expect("semantic ran");
        assert!(
            (semantic.metrics["hit@5"] - 1.0).abs() < 1e-9,
            "{:?}",
            semantic.metrics
        );
    }
}
