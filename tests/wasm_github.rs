//! The bundled GitHub source, tested with offline REST and Git stand-ins.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::OnceLock;

mod common;

use common::{TestDaemon, wait_for_job_status};
use memcastle::config::MiningConfig;
use memcastle::domain::{Job, JobStatus, Options};
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::{build::Project, conformance};
use serde_json::json;
use tokio::sync::Mutex;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn project() -> Project {
    Project::open(&root().join("sources/github")).unwrap()
}

fn adapter() -> WasmAdapter {
    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    let component = COMPONENT.get_or_init(|| project().build().unwrap());
    WasmAdapter::load(
        &project().manifest,
        &std::fs::read(component).unwrap(),
        &MiningConfig::default(),
    )
    .unwrap()
}

fn archive() -> &'static [u8] {
    static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
    ARCHIVE.get_or_init(|| {
        adapter();
        let output = tempfile::tempdir().unwrap();
        let (path, _) = project()
            .package(Some(&output.path().join("github.tar.gz")))
            .unwrap();
        std::fs::read(path).unwrap()
    })
}

static FIXTURE: Mutex<()> = Mutex::const_new(());

struct Fixture {
    _guard: tokio::sync::MutexGuard<'static, ()>,
    original_path: std::ffi::OsString,
    old_gh: Option<std::ffi::OsString>,
    old_github: Option<std::ffi::OsString>,
}

impl Fixture {
    async fn new() -> Self {
        let guard = FIXTURE.lock().await;
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let old_gh = std::env::var_os("GH_TOKEN");
        let old_github = std::env::var_os("GITHUB_TOKEN");
        let mut paths = vec![root().join("sources/github/fixtures/bin")];
        paths.extend(std::env::split_paths(&original_path));
        // Every test that changes these process-wide values holds FIXTURE until the adapter finishes.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
            std::env::remove_var("GH_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
        }
        Self {
            _guard: guard,
            original_path,
            old_gh,
            old_github,
        }
    }

    fn token(name: &str, value: &str) {
        unsafe { std::env::set_var(name, value) };
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            std::env::set_var("PATH", &self.original_path);
            for (name, previous) in [
                ("GH_TOKEN", &self.old_gh),
                ("GITHUB_TOKEN", &self.old_github),
            ] {
                if let Some(value) = previous {
                    std::env::set_var(name, value);
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
}

fn options(pairs: &[(&str, &str)]) -> Options {
    pairs
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

async fn ids(adapter: &WasmAdapter, options: &Options) -> Vec<String> {
    let source = adapter.identify(None, options).unwrap();
    let mut cursor = json!(null);
    let mut ids = Vec::new();
    for _ in 0..10 {
        let page = adapter.discover(&source, &cursor, 2).await.unwrap();
        for candidate in page.candidates {
            cursor = candidate.cursor_after;
            ids.push(candidate.external_id);
        }
        if page.exhausted {
            assert!(
                adapter
                    .discover(&source, &cursor, 2)
                    .await
                    .unwrap()
                    .candidates
                    .is_empty()
            );
            return ids;
        }
    }
    panic!("source did not exhaust its cursor");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_github_source_passes_its_offline_contract() {
    let _fixture = Fixture::new().await;
    let report = conformance::run_all(&adapter(), &root().join("sources/github/fixtures"))
        .await
        .unwrap();
    assert!(report.passed(), "{report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn inclusion_exclusion_and_content_switches_never_expand_a_repository_scope() {
    let _fixture = Fixture::new().await;
    let adapter = adapter();
    let selected = options(&[("include", "acme/*"), ("exclude", "acme/other")]);
    let documents = ids(&adapter, &selected).await;
    assert_eq!(documents.len(), 3);
    assert!(documents.iter().all(|id| id.starts_with("acme/alpha/")));
    let wiki_only = options(&[
        ("include", "acme/alpha"),
        ("wiki_include", "acme/alpha"),
        ("issues", "false"),
        ("pulls", "false"),
        ("metadata", "false"),
    ]);
    assert_eq!(
        ids(&adapter, &wiki_only).await,
        [
            format!("acme/alpha/wiki/Home.md@{}", "a".repeat(40)),
            format!("acme/alpha/wiki/Home.md@{}", "b".repeat(40)),
            format!("acme/alpha/wiki/Guide.md@{}", "b".repeat(40)),
        ]
    );
    assert!(adapter.identify(None, &Options::default()).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn wiki_paths_and_history_boundary_filter_page_revisions_without_enabling_other_content() {
    let _fixture = Fixture::new().await;
    let adapter = adapter();
    let scope = options(&[
        ("include", "acme/alpha"),
        ("wiki_include", "acme/alpha"),
        ("issues", "false"),
        ("pulls", "false"),
        ("metadata", "false"),
        ("paths", "Guide.md"),
        ("since", "2026-01-04"),
    ]);
    assert_eq!(
        ids(&adapter, &scope).await,
        [format!("acme/alpha/wiki/Guide.md@{}", "b".repeat(40))]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn comments_reviews_and_each_wiki_revision_are_independent_documents() {
    let _fixture = Fixture::new().await;
    let adapter = adapter();
    let scoped = options(&[
        ("include", "acme/alpha"),
        ("wiki_include", "acme/alpha"),
        ("comments", "true"),
        ("reviews", "true"),
    ]);
    let source = adapter.identify(None, &scoped).unwrap();
    let page = adapter.discover(&source, &json!(null), 30).await.unwrap();
    assert!(page.exhausted);
    assert_eq!(page.candidates.len(), 9);
    let wiki: Vec<_> = page
        .candidates
        .iter()
        .filter(|candidate| candidate.external_id.contains("/wiki/"))
        .collect();
    assert_eq!(wiki.len(), 3);
    let first = adapter.read(&source, wiki[0]).await.unwrap().unwrap();
    let second = adapter.read(&source, wiki[1]).await.unwrap().unwrap();
    assert_eq!(first.metadata["repo"], "acme/alpha");
    assert_eq!(first.metadata["path"], "Home.md");
    assert_eq!(
        first.occurred_at.unwrap().to_rfc3339(),
        "2026-01-03T00:00:00+00:00"
    );
    assert_ne!(first.external_id, second.external_id);
    assert_ne!(first.revision, second.revision);
    assert_eq!(
        adapter.normalize(&second).unwrap().room.as_deref(),
        Some("wikis")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn either_standard_token_works_and_errors_do_not_disclose_it() {
    let _fixture = Fixture::new().await;
    let adapter = adapter();
    let scoped = options(&[("include", "acme/alpha")]);
    Fixture::token("GITHUB_TOKEN", "rejected-token");
    let source = adapter.identify(None, &scoped).unwrap();
    let error = adapter
        .discover(&source, &json!(null), 2)
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("rejected-token"), "{error}");
    Fixture::token("GH_TOKEN", "fixture-token");
    assert!(!ids(&adapter, &scoped).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_configured_github_miner_passes_its_scope_to_a_durable_job_and_remining_is_idempotent() {
    let _fixture = Fixture::new().await;
    let daemon = TestDaemon::start().await;
    let client = reqwest::Client::new();
    let url = |path: &str| format!("{}{path}", daemon.base_url);
    // An ordinary installed package exercises the same component and manifest the release bundles.
    let refusal: serde_json::Value = client
        .post(url("/api/source-packages"))
        .body(archive().to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let consent = refusal["help"]
        .as_str()
        .unwrap()
        .split("--consent ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`');
    let installed = client
        .post(url("/api/source-packages"))
        .query(&[("consent", consent), ("enable", "true")])
        .body(archive().to_vec())
        .send()
        .await
        .unwrap();
    assert!(
        installed.status().is_success(),
        "{}",
        installed.text().await.unwrap()
    );

    let configured: serde_json::Value = client
        .put(url("/api/miners/acme-github"))
        .json(&json!({"source":"github", "scope": {
            "include":["acme/*"], "exclude":["acme/other"]},
            "config": {"comments":true, "reviews":true}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(configured["miner"]["state"], "ready", "{configured}");
    for expected in [6, 0] {
        let submitted = client
            .post(url("/api/miners/acme-github/run"))
            .send()
            .await
            .unwrap();
        assert!(
            submitted.status().is_success(),
            "{}",
            submitted.text().await.unwrap()
        );
        let job: Job = submitted.json().await.unwrap();
        let complete =
            wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await;
        assert_eq!(complete.result.unwrap()["documents"], expected);
    }
    let hits: Vec<serde_json::Value> = client
        .get(url("/api/search"))
        .query(&[("q", "Inline review")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["source"]["origin"]["source"], "github");
    assert!(
        hits[0]["source"]["origin"]["document"]
            .as_str()
            .unwrap()
            .starts_with("acme/alpha/review-comment/")
    );
    daemon.shutdown().await;
}
