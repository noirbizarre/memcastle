//! The Pi history source (`sources/pi`), built and run as the WebAssembly component users install (issue #161).
//!
//! What is Pi-specific is tested here and nowhere in the core: the reading of Pi's session files, what is kept and what
//! is left out, the identity and provenance a session carries, and that mining it is idempotent. The conformance cases
//! in `sources/pi/fixtures` run through the same runner every source is held to; the end-to-end test installs the
//! package into a real daemon, with consent, and mines through the shared pipeline.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use common::{TestDaemon, wait_for_job_status};
use memcastle::config::MiningConfig;
use memcastle::domain::{CanonicalDocument, Job, JobStatus, RawDocument, SourceKind};
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::build::Project;
use memcastle::source::conformance;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// The first fixture session, the one the unit-style tests below read.
const SESSION: &str = include_str!(
    "../sources/pi/fixtures/pi-sessions/tree/--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl"
);
const SESSION_ID: &str = "--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn project() -> Project {
    Project::open(&root().join("sources/pi")).expect("the Pi source opens")
}

/// The component, built once for the whole test binary.
fn component() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        project()
            .build()
            .expect("the Pi source builds; `rustup target add wasm32-wasip2` provides its target")
    })
}

fn adapter() -> WasmAdapter {
    let bytes = std::fs::read(component()).unwrap();
    WasmAdapter::load(&project().manifest, &bytes, &MiningConfig::default()).unwrap()
}

/// The package, built once.
fn archive() -> &'static [u8] {
    static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
    ARCHIVE.get_or_init(|| {
        let project = project();
        component();
        let out = tempfile::tempdir().unwrap();
        let (path, _) = project
            .package(Some(&out.path().join("pkg.tar.gz")))
            .unwrap();
        std::fs::read(path).unwrap()
    })
}

fn raw(body: &str) -> RawDocument {
    RawDocument {
        external_id: SESSION_ID.into(),
        revision: RawDocument::revision_of(body),
        body: body.to_string(),
        metadata: json!({"path": "/sessions/x.jsonl"}),
        occurred_at: None,
    }
}

fn normalized(body: &str) -> CanonicalDocument {
    adapter().normalize(&raw(body)).unwrap()
}

fn text(doc: &CanonicalDocument) -> String {
    doc.segments.iter().map(|s| s.text.as_str()).collect()
}

/// Write `content` to `root/name`, with a modification time `seconds` after the epoch so ordering never depends on how
/// fast the test runs.
fn write(root: &Path, name: &str, content: &str, seconds: u64) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pi_source_passes_the_conformance_cases_it_ships_with() {
    let report = conformance::run_all(&adapter(), &root().join("sources/pi/fixtures"))
        .await
        .unwrap();
    assert!(!report.cases.is_empty());
    for case in &report.cases {
        assert!(
            case.failures.is_empty(),
            "case `{}` failed:\n{}",
            case.name,
            case.failures.join("\n")
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_becomes_a_header_and_its_user_and_assistant_messages_in_order() {
    let text = text(&normalized(SESSION));
    assert!(text.starts_with("# Pi session 019f6106"), "{text}");
    assert!(text.contains("working directory: /home/me/project"));
    let user = text.find("### user").unwrap();
    let assistant = text.find("### assistant").unwrap();
    assert!(
        user < assistant,
        "messages must stay in the order they were written"
    );
    assert!(text.contains("How do I rotate the signing keys?"));
    assert!(text.contains("Run the rotation script"));
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_calls_are_kept_as_markers_and_tool_results_and_reasoning_are_dropped() {
    let text = text(&normalized(SESSION));
    assert!(text.contains("[tool call: read]"));
    assert!(
        !text.contains("SECRET-TOOL-OUTPUT"),
        "tool results are large and not what was said"
    );
    assert!(
        !text.contains("PRIVATE-REASONING"),
        "reasoning is not what was said"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_that_are_not_conversation_and_lines_that_are_not_json_are_skipped() {
    let text = text(&normalized(SESSION));
    assert!(!text.contains("thinkingLevel"));
    assert!(!text.contains("model_change"));
    assert!(
        text.contains("### assistant"),
        "a bad line must not stop the lines after it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_is_filed_as_a_transcript_under_its_working_directory_and_named_by_its_file() {
    let doc = normalized(SESSION);
    assert_eq!(doc.kind, SourceKind::Transcript);
    assert_eq!(doc.room.as_deref(), Some("project"));
    assert_eq!(
        doc.name.as_deref(),
        Some("2026-07-14T14-27-12-546Z_019f6106")
    );
    assert_eq!(
        doc.title.as_deref(),
        Some("How do I rotate the signing keys?")
    );
    assert_eq!(doc.uri.as_deref(), Some("/sessions/x.jsonl"));
    assert_eq!(doc.tags, ["transcript", "pi"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_with_no_messages_files_nothing() {
    let body = r#"{"type":"session","version":3,"id":"x","cwd":"/a"}
{"type":"model_change","provider":"p","modelId":"m"}
{"type":"message","message":{"role":"assistant","content":[]}}
"#;
    assert!(normalized(body).segments.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn appending_a_message_only_adds_a_segment_at_the_end() {
    let grown = format!(
        "{SESSION}{}\n",
        r#"{"type":"message","timestamp":"2026-07-14T15:00:00Z","message":{"role":"user","content":"and then?"}}"#
    );
    let before = normalized(SESSION).segments;
    let after = normalized(&grown).segments;
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "earlier segments must be byte-identical so their chunks keep their hashes"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_discovered_one_directory_down_and_only_jsonl_files_never_the_credentials_file()
 {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "--home-me-project--/a.jsonl", SESSION, 1_000);
    write(
        root.path(),
        "--home-me-project--/notes.txt",
        "not a session",
        1_000,
    );
    write(
        root.path(),
        "--home-me-project--/deep/b.jsonl",
        SESSION,
        1_000,
    );
    write(
        root.path(),
        "auth.json",
        "{\"token\":\"never read\"}",
        1_000,
    );
    #[cfg(unix)]
    {
        // Neither a link to a session nor a link to a directory is followed.
        std::os::unix::fs::symlink(
            root.path().join("--home-me-project--/a.jsonl"),
            root.path().join("link.jsonl"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            root.path().join("--home-me-project--"),
            root.path().join("linked-dir"),
        )
        .unwrap();
    }
    let adapter = adapter();
    let source = adapter.identify(root.path().to_str()).unwrap();

    let found = adapter.discover(&source, &json!(null), 10).await.unwrap();

    let ids: Vec<_> = found
        .candidates
        .iter()
        .map(|c| c.external_id.as_str())
        .collect();
    assert_eq!(ids, ["--home-me-project--/a.jsonl"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blank_session_file_is_skipped_at_read_time() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "a.jsonl", "  \n", 1_000);
    let adapter = adapter();
    let source = adapter.identify(root.path().to_str()).unwrap();
    let found = adapter.discover(&source, &json!(null), 10).await.unwrap();
    assert!(
        adapter
            .read(&source, &found.candidates[0])
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_session_carries_its_identity_provenance_and_start_time() {
    let tree = root().join("sources/pi/fixtures/pi-sessions/tree");
    let adapter = adapter();
    let source = adapter.identify(tree.to_str()).unwrap();
    let found = adapter.discover(&source, &json!(null), 100).await.unwrap();
    let candidate = found
        .candidates
        .iter()
        .find(|c| c.external_id == SESSION_ID)
        .expect("the fixture session is discovered");

    let first = adapter.read(&source, candidate).await.unwrap().unwrap();
    let again = adapter.read(&source, candidate).await.unwrap().unwrap();

    assert_eq!(first.external_id, SESSION_ID);
    assert_eq!(first.revision, again.revision);
    assert_eq!(first.body, SESSION);
    assert_eq!(
        first.metadata["session_id"],
        "019f6106-5422-783d-a568-61a5b1a4a98a"
    );
    assert_eq!(first.metadata["cwd"], "/home/me/project");
    assert_eq!(first.metadata["version"], 3);
    assert!(
        first.metadata["path"]
            .as_str()
            .unwrap()
            .ends_with(SESSION_ID),
        "the path says which file the session came from: {}",
        first.metadata
    );
    assert_eq!(
        first.occurred_at,
        Some(
            DateTime::parse_from_rfc3339("2026-07-14T14:27:12.546Z")
                .unwrap()
                .with_timezone(&Utc)
        ),
        "a session happened when it started, not when its file was last touched"
    );
    let doc = adapter.normalize(&first).unwrap();
    assert!(doc.uri.as_deref().unwrap().ends_with(SESSION_ID));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_header_time_that_is_not_rfc_3339_does_not_fail_the_session() {
    let root = tempfile::tempdir().unwrap();
    let body = r#"{"type":"session","version":3,"id":"x","timestamp":"last tuesday","cwd":"/a"}
{"type":"message","timestamp":"2026-07-14T15:00:00Z","message":{"role":"user","content":"hello"}}
"#;
    write(root.path(), "p/a.jsonl", body, 1_000);
    let adapter = adapter();
    let source = adapter.identify(root.path().to_str()).unwrap();
    let found = adapter.discover(&source, &json!(null), 10).await.unwrap();

    let raw = adapter
        .read(&source, &found.candidates[0])
        .await
        .expect("an odd timestamp must not fail the read")
        .unwrap();

    assert_eq!(raw.occurred_at, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_locator_that_is_not_a_directory_is_refused_and_the_default_is_pis_own_directory() {
    let adapter = adapter();
    let error = adapter
        .identify(Some("/nonexistent/pi/sessions"))
        .unwrap_err();
    assert!(
        error.to_string().contains("/nonexistent/pi/sessions"),
        "{error}"
    );

    // With no locator the source looks where Pi keeps its sessions: it is found on a machine that has Pi and refused,
    // naming that place, on one that has not.
    match adapter.identify(None) {
        Ok(source) => assert!(
            source.locator.ends_with(".pi/agent/sessions"),
            "{}",
            source.locator
        ),
        Err(error) => assert!(error.to_string().contains(".pi/agent/sessions"), "{error}"),
    }
}

/// A daemon whose sources are installed under a directory the test owns.
struct Fixture {
    daemon: TestDaemon,
    client: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sources_dir = dir.path().join("sources");
        let daemon = TestDaemon::start_configured(move |config| {
            config.mining.sources_dir = Some(sources_dir)
        })
        .await;
        Self {
            daemon,
            client: reqwest::Client::new(),
            _dir: dir,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.daemon.base_url)
    }

    async fn install(&self, query: &[(&str, &str)]) -> reqwest::Response {
        self.client
            .post(self.url("/api/source-packages"))
            .query(query)
            .body(archive().to_vec())
            .send()
            .await
            .expect("request")
    }

    async fn mined(&self, root: &Path) -> Job {
        let response = self
            .client
            .post(self.url("/api/jobs"))
            .json(
                &json!({"type": "mine", "provider": "pi", "locator": root, "requested_by": "test"}),
            )
            .send()
            .await
            .expect("request");
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let job: Job = response.json().await.unwrap();
        wait_for_job_status(
            &self.client,
            &self.daemon.base_url,
            job.id,
            JobStatus::Completed,
        )
        .await
    }

    async fn search(&self, query: &str) -> Vec<Value> {
        self.client
            .get(self.url("/api/search"))
            .query(&[("q", query)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_installed_pi_source_mines_history_idempotently_with_provenance_and_adds_only_the_tail_of_a_grown_session()
 {
    let fixture = Fixture::start().await;

    // Installing asks for consent to exactly what the manifest lists: Pi's sessions and the one variable.
    let refusal = fixture.install(&[]).await;
    assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    let refusal: Value = refusal.json().await.unwrap();
    assert_eq!(refusal["code"], "memcastle::source::consent_required");
    let error = refusal["error"].as_str().unwrap();
    assert!(error.contains("~/.pi/agent/sessions"), "{error}");
    assert!(error.contains("HOME"), "{error}");
    let digest = refusal["help"]
        .as_str()
        .unwrap()
        .split("--consent ")
        .nth(1)
        .unwrap()
        .trim_end_matches('`')
        .to_string();
    let installed = fixture
        .install(&[("consent", &digest), ("enable", "true")])
        .await;
    assert_eq!(installed.status(), StatusCode::OK);
    let installed: Value = installed.json().await.unwrap();
    assert_eq!(installed["source"]["name"], "pi");
    assert_eq!(installed["source"]["capabilities"]["retains_raw"], true);
    assert_eq!(
        installed["source"]["capabilities"]["needs_credentials"],
        false
    );

    let sessions = tempfile::tempdir().unwrap();
    write(
        sessions.path(),
        "--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl",
        SESSION,
        1_000,
    );

    let first = fixture.mined(sessions.path()).await;
    assert_eq!(
        first.result.as_ref().unwrap()["created"],
        1,
        "{:?}",
        first.result
    );
    let hits = fixture.search("rotate signing keys").await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["source"]["kind"], "transcript");
    assert_eq!(hits[0]["source"]["origin"]["provider"], "pi");
    assert_eq!(hits[0]["source"]["origin"]["document"], SESSION_ID);
    assert!(
        fixture.search("SECRET-TOOL-OUTPUT").await.is_empty(),
        "tool results are not filed"
    );

    let second = fixture.mined(sessions.path()).await;
    assert_eq!(
        second.result.as_ref().unwrap()["documents"],
        0,
        "re-running acquisition reads nothing: the cursor is past the session"
    );
    assert_eq!(
        fixture.search("rotate signing keys").await.len(),
        1,
        "and duplicates nothing"
    );

    let grown = format!(
        "{SESSION}{}\n",
        r#"{"type":"message","timestamp":"2026-07-14T15:00:00Z","message":{"role":"user","content":"what about certificate revocation?"}}"#
    );
    write(
        sessions.path(),
        "--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl",
        &grown,
        2_000,
    );
    let third = fixture.mined(sessions.path()).await;
    let summary = third.result.as_ref().unwrap();
    assert_eq!(
        summary["documents"], 1,
        "only the session that grew is read"
    );
    assert_eq!(
        summary["superseded"], 1,
        "its one chunk is replaced by the longer one"
    );
    assert_eq!(fixture.search("certificate revocation").await.len(), 1);

    fixture.daemon.shutdown().await;
}
