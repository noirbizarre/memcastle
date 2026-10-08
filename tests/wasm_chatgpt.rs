//! ChatGPT is exercised through its installed component and the same adapter contract as other sources.
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;

use common::{TestDaemon, wait_for_job_status};
use memcastle::config::MiningConfig;
use memcastle::domain::{AccessTokens, Job, JobStatus, OAuthRequirement, Secret};
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::{build::Project, conformance};
use serde_json::json;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn project() -> Project {
    Project::open(&root().join("sources/chatgpt")).unwrap()
}
fn component() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| project().build().expect("the ChatGPT component builds"))
}
fn adapter() -> WasmAdapter {
    WasmAdapter::load(
        &project().manifest,
        &std::fs::read(component()).unwrap(),
        &MiningConfig::default(),
    )
    .unwrap()
}

fn archive() -> &'static [u8] {
    static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
    ARCHIVE.get_or_init(|| {
        component();
        let output = tempfile::tempdir().unwrap();
        let (path, _) = project()
            .package(Some(&output.path().join("chatgpt.tar.gz")))
            .unwrap();
        std::fs::read(path).unwrap()
    })
}

fn project_options(projects: &str) -> memcastle::domain::Options {
    let mut options = memcastle::domain::Options::new();
    options.insert("account".into(), "fixture".into());
    options.insert("mode".into(), "web".into());
    options.insert("projects".into(), projects.into());
    options
}

struct SyntheticTokens;

impl AccessTokens for SyntheticTokens {
    fn access_token(
        &self,
        source: &str,
        _requirement: &OAuthRequirement,
    ) -> memcastle::Result<Secret> {
        assert_eq!(source, "chatgpt");
        Ok(Secret::new("fixture-oauth"))
    }

    fn is_signed_in(&self, _source: &str, _requirement: &OAuthRequirement) -> bool {
        true
    }
}

async fn web_fixture() -> (
    tokio::sync::MutexGuard<'static, ()>,
    WasmAdapter,
    memcastle::domain::SourceRef,
) {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let lock = LOCK.lock().await;
    let bin = root().join("sources/chatgpt/fixtures/bin");
    let path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        std::env::set_var("MEMCASTLE_CHATGPT_BEARER", "fixture-session");
        std::env::set_var("MEMCASTLE_CHATGPT_COOKIE", "fixture-cookie");
    }
    let adapter = adapter();
    let mut options = memcastle::domain::Options::new();
    options.insert("account".into(), "fixture".into());
    options.insert("mode".into(), "web".into());
    let source = adapter.identify(None, &options).unwrap();
    (lock, adapter, source)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_synthetic_export_passes_the_source_contract() {
    let report = conformance::run_all(&adapter(), &root().join("sources/chatgpt/fixtures"))
        .await
        .unwrap();
    assert!(report.passed(), "{report:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_official_zip_format_is_imported_inside_the_wasm_sandbox() {
    let adapter = adapter();
    let path = root().join("sources/chatgpt/fixtures/export/synthetic-export.zip");
    let source = adapter
        .identify(path.to_str(), &Default::default())
        .unwrap();
    let page = adapter.discover(&source, &json!(null), 3).await.unwrap();
    assert!(page.exhausted);
    assert_eq!(page.candidates.len(), 2);
    let raw = adapter
        .read(&source, &page.candidates[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.external_id, "conversation-a");
    assert_eq!(adapter.normalize(&raw).unwrap().segments.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completed_sweep_revisits_unchanged_conversations_on_the_next_run() {
    let adapter = adapter();
    let path = root().join("sources/chatgpt/fixtures/export/conversations.json");
    let source = adapter
        .identify(path.to_str(), &Default::default())
        .unwrap();
    let first = adapter.discover(&source, &json!(null), 3).await.unwrap();
    assert!(first.exhausted);
    let last = &first.candidates.last().unwrap().cursor_after;
    let next = adapter.discover(&source, last, 3).await.unwrap();
    assert_eq!(next.candidates.len(), first.candidates.len());
    let one = adapter
        .read(&source, &first.candidates[0])
        .await
        .unwrap()
        .unwrap();
    let again = adapter
        .read(&source, &next.candidates[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(one.revision, again.revision);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_experimental_backend_fetches_every_message_page_without_a_network() {
    // The stand-in is an exact-name process grant, like OpenCode's fixtures; no private endpoint is contacted.
    let (_lock, adapter, source) = web_fixture().await;
    let page = adapter.discover(&source, &json!(null), 5).await.unwrap();
    assert_eq!(page.candidates.len(), 4);
    let raw = adapter
        .read(&source, &page.candidates[0])
        .await
        .unwrap()
        .unwrap();
    let normalized = adapter.normalize(&raw).unwrap();
    assert_eq!(normalized.segments.len(), 2);
    assert!(normalized.segments[0].text.contains("Before"));
    assert!(normalized.segments[1].text.contains("After"));
    assert!(!raw.body.contains("fixture-session"));
    assert_eq!(raw.metadata["project_id"], "g-p-alpha");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_web_source_reads_renewable_host_tokens_without_a_cookie_and_export_stays_offline() {
    let (_lock, _adapter, _) = web_fixture().await;
    unsafe {
        std::env::remove_var("MEMCASTLE_CHATGPT_BEARER");
        std::env::remove_var("MEMCASTLE_CHATGPT_COOKIE");
    }
    let bytes = std::fs::read(component()).unwrap();
    let signed_in = WasmAdapter::load_with(
        &project().manifest,
        &bytes,
        &MiningConfig::default(),
        Some(Arc::new(SyntheticTokens)),
    )
    .unwrap();
    let options = project_options("id:g-p-alpha");
    let source = signed_in.identify(None, &options).unwrap();
    let page = signed_in.discover(&source, &json!(null), 5).await.unwrap();
    assert_eq!(page.candidates.len(), 2);
    let raw = signed_in
        .read(&source, &page.candidates[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.metadata["project_id"], "g-p-alpha");
    assert!(!raw.body.contains("fixture-oauth"));

    let signed_out = adapter();
    let export = root().join("sources/chatgpt/fixtures/export/conversations.json");
    let offline = signed_out
        .identify(export.to_str(), &Default::default())
        .unwrap();
    assert_eq!(
        signed_out
            .discover(&offline, &json!(null), 5)
            .await
            .unwrap()
            .candidates
            .len(),
        2
    );
    let error = signed_out.identify(None, &options).unwrap_err().to_string();
    assert!(
        error.contains("credential") || error.contains("access token"),
        "{error}"
    );
    assert!(!error.contains("fixture-oauth"));
}

#[tokio::test(flavor = "multi_thread")]
async fn names_and_ids_select_the_same_project_but_other_projects_have_separate_cursors() {
    let (_lock, adapter, all) = web_fixture().await;
    let alpha = adapter
        .identify(None, &project_options("name:Alpha"))
        .unwrap();
    let alpha_by_id = adapter
        .identify(None, &project_options("id:g-p-alpha"))
        .unwrap();
    let beta = adapter
        .identify(None, &project_options("name:Beta"))
        .unwrap();
    let empty = adapter
        .identify(None, &project_options("id:g-p-empty"))
        .unwrap();
    assert_eq!(alpha.id(), alpha_by_id.id());
    assert_ne!(alpha.id(), beta.id());
    assert_ne!(all.id(), alpha.id());
    assert_eq!(
        adapter
            .discover(&alpha, &json!(null), 5)
            .await
            .unwrap()
            .candidates
            .len(),
        2
    );
    let beta_page = adapter.discover(&beta, &json!(null), 5).await.unwrap();
    assert_eq!(beta_page.candidates.len(), 1);
    let raw = adapter
        .read(&beta, &beta_page.candidates[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.metadata["project_id"], "g-p-beta");
    assert_eq!(raw.metadata["project_name"], "Beta");
    assert!(
        raw.body.contains("\"gizmo_id\":null"),
        "detail cannot supply membership"
    );
    assert!(
        adapter
            .discover(&empty, &json!(null), 5)
            .await
            .unwrap()
            .candidates
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn several_project_names_are_canonicalized_into_the_same_identity_as_sorted_ids() {
    let (_lock, adapter, _) = web_fixture().await;
    let names = adapter
        .identify(None, &project_options("name:Beta,name:Alpha"))
        .unwrap();
    let ids = adapter
        .identify(None, &project_options("id:g-p-alpha,id:g-p-beta"))
        .unwrap();
    assert_eq!(names.id(), ids.id());
    assert_eq!(
        adapter
            .discover(&names, &json!(null), 5)
            .await
            .unwrap()
            .candidates
            .len(),
        3
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ambiguous_or_missing_names_fail_instead_of_mining_another_project() {
    let (_lock, adapter, _) = web_fixture().await;
    unsafe {
        std::env::set_var("MEMCASTLE_CHATGPT_BEARER", "duplicate-name-session");
    }
    let error = adapter
        .identify(None, &project_options("name:Shared"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("more than one"), "{error}");
    unsafe {
        std::env::set_var("MEMCASTLE_CHATGPT_BEARER", "fixture-session");
    }
    let error = adapter
        .identify(None, &project_options("name:Renamed"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not found"), "{error}");
    let export = root().join("sources/chatgpt/fixtures/export/conversations.json");
    assert!(
        adapter
            .identify(export.to_str(), &project_options("id:g-p-alpha"))
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_membership_or_repeated_project_cursors_are_not_mined_partially() {
    let (_lock, adapter, _) = web_fixture().await;
    let source = adapter
        .identify(None, &project_options("id:g-p-alpha"))
        .unwrap();
    for (session, expected) in [
        ("wrong-project-session", "another project"),
        ("repeated-project-cursor", "repeated a cursor"),
    ] {
        unsafe {
            std::env::set_var("MEMCASTLE_CHATGPT_BEARER", session);
        }
        let error = adapter
            .discover(&source, &json!(null), 5)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains(session));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_private_backend_reports_expiry_rate_limits_and_a_shifted_list_without_credentials() {
    let (_lock, adapter, source) = web_fixture().await;
    for (session, expected) in [
        ("expired-session", "replace the daemon's session"),
        ("throttled-session", "retry later"),
        ("shifted-session", "shifted during paging"),
    ] {
        unsafe {
            std::env::set_var("MEMCASTLE_CHATGPT_BEARER", session);
        }
        let error = adapter
            .discover(&source, &json!(null), 5)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(
            !error.contains(session),
            "a session must not appear in an error: {error}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_previous_message_page_is_rejected_instead_of_filing_a_partial_transcript() {
    let (_lock, adapter, source) = web_fixture().await;
    let page = adapter.discover(&source, &json!(null), 5).await.unwrap();
    unsafe {
        std::env::set_var("MEMCASTLE_CHATGPT_BEARER", "partial-session");
    }
    let error = adapter
        .read(&source, &page.candidates[0])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("empty page"), "{error}");
    assert!(!error.contains("partial-session"));
}

#[tokio::test(flavor = "multi_thread")]
async fn branches_and_source_metadata_remain_available_in_the_raw_conversation() {
    let adapter = adapter();
    let path = root().join("sources/chatgpt/fixtures/export/conversations.json");
    let source = adapter
        .identify(path.to_str(), &Default::default())
        .unwrap();
    let page = adapter.discover(&source, &json!(null), 2).await.unwrap();
    let raw = adapter
        .read(&source, &page.candidates[0])
        .await
        .unwrap()
        .unwrap();
    let conversation: serde_json::Value = serde_json::from_str(&raw.body).unwrap();
    assert_eq!(
        conversation["mapping"]["question"]["children"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(raw.body.contains("references"));
    assert_eq!(raw.metadata["backend"], "export");
    let normalized = adapter.normalize(&raw).unwrap();
    assert_eq!(normalized.segments.len(), 3);
    assert!(normalized.segments[2].text.contains("parent=question"));
    assert!(
        !normalized
            .segments
            .iter()
            .any(|segment| segment.text.contains("PRIVATE-TOOL-RESULT"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_installed_source_mines_an_export_idempotently_and_finds_a_later_edit() {
    let scratch = tempfile::tempdir().unwrap();
    let sources_dir = scratch.path().join("installed");
    let daemon =
        TestDaemon::start_configured(move |config| config.mining.sources_dir = Some(sources_dir))
            .await;
    let client = reqwest::Client::new();
    let url = |path: &str| format!("{}{path}", daemon.base_url);
    let refusal: serde_json::Value = client
        .post(url("/api/source-packages"))
        .body(archive().to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(refusal["code"], "memcastle::source::consent_required");
    let digest = refusal["help"]
        .as_str()
        .unwrap()
        .split("--consent ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`');
    let install = client
        .post(url("/api/source-packages"))
        .query(&[("enable", "true"), ("consent", digest)])
        .body(archive().to_vec())
        .send()
        .await
        .unwrap();
    assert!(
        install.status().is_success(),
        "{}",
        install.text().await.unwrap()
    );

    let file = scratch.path().join("conversations.json");
    let original = include_str!("../sources/chatgpt/fixtures/export/conversations.json");
    std::fs::write(&file, original).unwrap();
    let mine = || async {
        let response = client.post(url("/api/jobs"))
            .json(&json!({"type": "mine", "source": "chatgpt", "locator": file, "requested_by": "test"}))
            .send().await.unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let job: Job = response.json().await.unwrap();
        wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await
    };

    let first = mine().await;
    assert_eq!(first.result.as_ref().unwrap()["documents"], 2);
    let hits: Vec<serde_json::Value> = client
        .get(url("/api/search"))
        .query(&[("q", "fortress")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["source"]["origin"]["document"], "conversation-a");

    let second = mine().await;
    assert_eq!(second.result.as_ref().unwrap()["unchanged"], 2);
    assert_eq!(second.result.as_ref().unwrap()["created"], 0);
    std::fs::write(&file, original.replace("A fortress.", "A stone fortress.")).unwrap();
    let third = mine().await;
    assert_eq!(third.result.as_ref().unwrap()["unchanged"], 1);
    assert_eq!(third.result.as_ref().unwrap()["superseded"], 1);
    daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_persistent_miner_for_multiple_projects_keeps_its_scope_and_cursor() {
    let (_lock, _adapter, _) = web_fixture().await;
    let scratch = tempfile::tempdir().unwrap();
    let sources_dir = scratch.path().join("installed");
    let daemon =
        TestDaemon::start_configured(move |config| config.mining.sources_dir = Some(sources_dir))
            .await;
    let client = reqwest::Client::new();
    let url = |path: &str| format!("{}{path}", daemon.base_url);
    let refusal: serde_json::Value = client
        .post(url("/api/source-packages"))
        .body(archive().to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let digest = refusal["help"]
        .as_str()
        .unwrap()
        .split("--consent ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`');
    let installed = client
        .post(url("/api/source-packages"))
        .query(&[("enable", "true"), ("consent", digest)])
        .body(archive().to_vec())
        .send()
        .await
        .unwrap();
    assert!(
        installed.status().is_success(),
        "{}",
        installed.text().await.unwrap()
    );

    let configured = client
        .put(url("/api/miners/chatgpt-project"))
        .json(
            &json!({"source": "chatgpt", "scope": {"projects": ["name:Beta", "id:g-p-alpha"]},
                     "config": {"mode": "web", "account": "fixture"}}),
        )
        .send()
        .await
        .unwrap();
    assert!(
        configured.status().is_success(),
        "{}",
        configured.text().await.unwrap()
    );

    let run = || async {
        let response = client
            .post(url("/api/miners/chatgpt-project/run"))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let job: Job = response.json().await.unwrap();
        wait_for_job_status(&client, &daemon.base_url, job.id, JobStatus::Completed).await
    };
    let first = run().await;
    assert_eq!(first.result.as_ref().unwrap()["documents"], 3);
    let second = run().await;
    assert_eq!(second.result.as_ref().unwrap()["unchanged"], 3);
    let widened = client
        .put(url("/api/miners/chatgpt-project"))
        .json(&json!({"unset_scope": ["projects"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(widened.status(), reqwest::StatusCode::CONFLICT);
    let widened: serde_json::Value = widened.json().await.unwrap();
    assert_eq!(widened["code"], "memcastle::miner::scope_broadened");
    daemon.shutdown().await;
}
