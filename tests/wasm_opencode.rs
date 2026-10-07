//! The OpenCode history source (`sources/opencode`), built and run as the WebAssembly component users install (issue #90).
//!
//! What is OpenCode-specific is tested here and nowhere in the core: how history is acquired through the `opencode`
//! command, what is kept and what is left out of a session, the identity and provenance a session carries, and that
//! mining it is incremental and idempotent. The conformance cases in `sources/opencode/fixtures` run through the same
//! runner every source is held to; the end-to-end test installs the package into a real daemon, with consent, and mines
//! through the shared pipeline.
//!
//! No test needs OpenCode: `sources/opencode/fixtures/bin/opencode` stands in for it, answering from
//! `fixtures/data`, and each test that runs the source puts its own copy first on the `PATH`.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::{TestDaemon, wait_for_job_status};
use memcastle::config::MiningConfig;
use memcastle::domain::{
    Candidate, CanonicalDocument, Cursor, Job, JobStatus, RawDocument, SourceKind,
};
use memcastle::error::Error;
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::build::Project;
use memcastle::source::conformance;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::{Mutex, MutexGuard};

const ALPHA: &str = include_str!("../sources/opencode/fixtures/data/export/ses_alpha.out");
const BRAVO: &str = include_str!("../sources/opencode/fixtures/data/export/ses_bravo.out");
const CHARLIE: &str = include_str!("../sources/opencode/fixtures/data/export/ses_charlie.out");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> PathBuf {
    root().join("sources/opencode/fixtures")
}

fn project() -> Project {
    Project::open(&root().join("sources/opencode")).expect("the OpenCode source opens")
}

/// The component, built once for the whole test binary.
fn component() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        project().build().expect(
            "the OpenCode source builds; `rustup target add wasm32-wasip2` provides its target",
        )
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

fn raw(external_id: &str, body: &str) -> RawDocument {
    RawDocument {
        external_id: external_id.into(),
        revision: RawDocument::revision_of(body),
        body: body.to_string(),
        metadata: json!({}),
        occurred_at: None,
    }
}

fn normalized(external_id: &str, body: &str) -> CanonicalDocument {
    adapter().normalize(&raw(external_id, body)).unwrap()
}

fn text(doc: &CanonicalDocument) -> String {
    doc.segments.iter().map(|s| s.text.as_str()).collect()
}

fn candidate(id: &str) -> Candidate {
    Candidate {
        external_id: id.into(),
        cursor_after: Cursor::Null,
        handle: id.into(),
    }
}

/// The `PATH` the test process started with, read before any test changes it.
fn original_path() -> &'static std::ffi::OsString {
    static PATH: OnceLock<std::ffi::OsString> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os("PATH").unwrap_or_default())
}

/// Only one test at a time may own the `PATH` and the data the stand-in answers from.
static HISTORY: Mutex<()> = Mutex::const_new(());

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            // `copy` keeps the executable bit the stand-in needs.
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A private copy of the fixtures with its stand-in `opencode` first on the `PATH`, held for the length of a test.
///
/// The copy is what lets a test change what OpenCode "contains" without touching the repository, and the lock is what
/// keeps another test in the same process from seeing it or from swapping the `PATH` underneath it.
struct History {
    _lock: MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

impl History {
    async fn new() -> Self {
        let lock = HISTORY.lock().await;
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixtures(), dir.path());
        let history = Self { _lock: lock, dir };
        history.put_on_path(&history.dir.path().join("bin"));
        history
    }

    fn put_on_path(&self, first: &Path) {
        let mut paths = vec![first.to_path_buf()];
        paths.extend(std::env::split_paths(original_path()));
        // SAFETY: tests that read or spawn through the `PATH` hold `HISTORY`, so none runs while it changes, and the
        // value is restored by the next `History`.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    }

    /// Make `opencode` unfindable, as on a machine that does not have it.
    fn uninstall(&self) {
        let empty = self.dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        // The original path is left out too: a developer's machine may have the real one.
        // SAFETY: as in `put_on_path`.
        unsafe { std::env::set_var("PATH", &empty) };
    }

    fn data(&self, name: &str) -> PathBuf {
        self.dir.path().join("data").join(name)
    }

    /// A session grown by one user message, and its `time_updated` moved on, as OpenCode records a new prompt.
    fn grow_alpha(&self) {
        let path = self.data("export/ses_alpha.out");
        let mut session: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        session["messages"].as_array_mut().unwrap().push(json!({
            "info": {"id": "msg_a4", "sessionID": "ses_alpha", "role": "user", "time": {"created": 1_784_039_290_000_i64}},
            "parts": [{"id": "prt_a9", "type": "text", "text": "what about certificate revocation?"}]
        }));
        std::fs::write(&path, serde_json::to_string_pretty(&session).unwrap()).unwrap();
        let sessions = self.data("sessions.tsv");
        let listed = std::fs::read_to_string(&sessions).unwrap();
        std::fs::write(
            sessions,
            listed.replace("ses_alpha\t1784039280000", "ses_alpha\t1784365205000"),
        )
        .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_opencode_source_passes_the_conformance_cases_it_ships_with() {
    let _history = History::new().await;
    let report = conformance::run_all(&adapter(), &fixtures()).await.unwrap();
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
    let text = text(&normalized("ses_alpha", ALPHA));
    assert!(text.starts_with("# OpenCode session ses_alpha"), "{text}");
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
async fn tool_calls_are_kept_as_markers_and_outputs_reasoning_and_injected_text_are_dropped() {
    let text = text(&normalized("ses_alpha", ALPHA));
    assert!(text.contains("[tool: read] scripts/rotate.sh"));
    for left_out in [
        "SECRET-TOOL-OUTPUT",
        "SECRET-REASONING",
        "SYNTHETIC-INJECTED-TEXT",
    ] {
        assert!(
            !text.contains(left_out),
            "`{left_out}` is not what was said: {text}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attached_file_is_named_and_never_filed_and_a_compaction_summary_is_dropped() {
    let text = text(&normalized("ses_bravo", BRAVO));
    assert!(text.contains("[file: TODO.md]"));
    assert!(
        !text.contains("U0VDUkVULUZJTEUtQk9EWQ"),
        "the encoded file is not filed"
    );
    assert!(
        !text.contains("SUMMARY-OF-EARLIER-TURNS"),
        "a summary restates messages that are filed already"
    );
    assert!(!text.contains("SECRET-GIT-OUTPUT"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_is_filed_as_a_transcript_under_its_working_directory_and_named_by_its_id() {
    let doc = normalized("ses_alpha", ALPHA);
    assert_eq!(doc.kind, SourceKind::Transcript);
    assert_eq!(doc.room.as_deref(), Some("project"));
    assert_eq!(doc.name.as_deref(), Some("ses_alpha"));
    assert_eq!(doc.title.as_deref(), Some("Rotate the signing keys"));
    assert_eq!(doc.uri.as_deref(), Some("opencode://session/ses_alpha"));
    assert_eq!(doc.tags, ["transcript", "opencode"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_with_no_messages_or_no_json_body_files_nothing_and_does_not_fail() {
    assert!(normalized("ses_charlie", CHARLIE).segments.is_empty());
    assert!(normalized("ses_x", "this is not json").segments.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn appending_a_message_only_adds_a_segment_at_the_end_even_when_the_session_is_renamed() {
    let mut grown: Value = serde_json::from_str(ALPHA).unwrap();
    grown["messages"].as_array_mut().unwrap().push(json!({
        "info": {"id": "msg_a4", "role": "user", "time": {"created": 1_784_039_290_000_i64}},
        "parts": [{"id": "prt_a9", "type": "text", "text": "and then?"}]
    }));
    // OpenCode renames a session once it has a title; the header must not carry it, or every rename refiles the head.
    grown["info"]["title"] = json!("A different title");

    let before = normalized("ses_alpha", ALPHA).segments;
    let after = normalized("ses_alpha", &grown.to_string()).segments;

    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "earlier segments must be byte-identical so their chunks keep their hashes"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn identify_names_the_database_opencode_reports_unless_a_locator_names_the_history() {
    let _history = History::new().await;
    let adapter = adapter();

    let default = adapter.identify(None, &Default::default()).unwrap();
    assert_eq!(default.source, "opencode");
    assert_eq!(default.locator, "/fixture/opencode/opencode.db");

    let named = adapter
        .identify(Some("work-laptop"), &Default::default())
        .unwrap();
    assert_eq!(named.locator, "work-laptop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_machine_without_opencode_is_told_what_is_missing_and_a_failing_opencode_is_quoted() {
    let history = History::new().await;
    let adapter = adapter();

    std::fs::write(history.data("fail"), "").unwrap();
    let failing = adapter
        .identify(None, &Default::default())
        .unwrap_err()
        .to_string();
    assert!(failing.contains("database is locked"), "{failing}");
    assert!(failing.contains("opencode db path"), "{failing}");

    history.uninstall();
    let missing = adapter
        .identify(None, &Default::default())
        .unwrap_err()
        .to_string();
    assert!(missing.contains("opencode"), "{missing}");
    assert!(missing.contains("PATH"), "{missing}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_off_discovery_answer_is_quoted_and_not_blamed_only_on_the_opencode_version() {
    let history = History::new().await;
    let adapter = adapter();
    let source = adapter.identify(None, &Default::default()).unwrap();

    std::fs::write(history.data("cut"), "").unwrap();
    let error = adapter
        .discover(&source, &json!(null), 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("cut off"), "{error}");
    assert!(error.contains("ses_al"), "what arrived is quoted: {error}");
    assert!(error.contains("bytes"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cursor_the_source_did_not_write_is_refused_and_never_reaches_the_query() {
    let _history = History::new().await;
    let adapter = adapter();
    let source = adapter.identify(None, &Default::default()).unwrap();

    for hostile in [
        json!({"updated": 1, "id": "x' OR '1'='1"}),
        json!({"updated": "1 OR 1=1", "id": "ses_alpha"}),
        json!({"updated": -5, "id": "ses_alpha"}),
        json!({"id": "ses_alpha"}),
        json!("not a cursor"),
    ] {
        let error = adapter.discover(&source, &hostile, 2).await.unwrap_err();
        assert!(
            matches!(error, Error::SourceCursorInvalid { .. }),
            "{hostile}: {error}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_session_carries_its_identity_provenance_and_start_time() {
    let _history = History::new().await;
    let adapter = adapter();
    let source = adapter.identify(None, &Default::default()).unwrap();

    let first = adapter
        .read(&source, &candidate("ses_alpha"))
        .await
        .unwrap()
        .unwrap();
    let again = adapter
        .read(&source, &candidate("ses_alpha"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first.external_id, "ses_alpha");
    assert_eq!(first.revision, again.revision);
    assert_eq!(first.metadata["session_id"], "ses_alpha");
    assert_eq!(first.metadata["directory"], "/home/me/project");
    assert_eq!(first.metadata["project_id"], "prj_one");
    assert_eq!(first.metadata["title"], "Rotate the signing keys");
    assert_eq!(first.metadata["version"], "1.18.34");
    assert_eq!(
        first.occurred_at.map(|at| at.to_rfc3339()),
        Some("2026-07-14T14:27:00+00:00".to_string()),
        "a session happened when it started, not when it was last updated"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_notice_printed_before_an_export_is_ignored() {
    let _history = History::new().await;
    let adapter = adapter();
    let source = adapter.identify(None, &Default::default()).unwrap();

    let raw = adapter
        .read(&source, &candidate("ses_delta"))
        .await
        .unwrap()
        .unwrap();

    assert!(raw.body.starts_with('{'), "{}", raw.body);
    assert!(serde_json::from_str::<Value>(&raw.body).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_is_empty_or_gone_since_discovery_is_skipped_not_an_error() {
    let _history = History::new().await;
    let adapter = adapter();
    let source = adapter.identify(None, &Default::default()).unwrap();

    for id in ["ses_charlie", "ses_deleted", "-rf"] {
        assert!(
            adapter
                .read(&source, &candidate(id))
                .await
                .unwrap()
                .is_none(),
            "{id} has nothing to file"
        );
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

    async fn mined(&self) -> Job {
        let response = self
            .client
            .post(self.url("/api/jobs"))
            .json(&json!({"type": "mine", "source": "opencode", "requested_by": "test"}))
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
async fn the_installed_opencode_source_mines_history_incrementally_with_provenance_and_adds_only_the_tail_of_a_grown_session()
 {
    let history = History::new().await;
    let fixture = Fixture::start().await;

    // Installing asks for consent to exactly what the manifest lists: the one program and the one variable.
    let refusal = fixture.install(&[]).await;
    assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    let refusal: Value = refusal.json().await.unwrap();
    assert_eq!(refusal["code"], "memcastle::source::consent_required");
    let error = refusal["error"].as_str().unwrap();
    assert!(error.contains("opencode"), "{error}");
    assert!(error.contains("XDG_DATA_HOME"), "{error}");
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
    assert_eq!(installed["source"]["name"], "opencode");
    assert_eq!(installed["source"]["capabilities"]["retains_raw"], true);
    assert_eq!(
        installed["source"]["capabilities"]["needs_credentials"],
        false
    );

    let first = fixture.mined().await;
    assert_eq!(
        first.result.as_ref().unwrap()["created"],
        3,
        "{:?}",
        first.result
    );
    let hits = fixture.search("rotate signing keys").await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["source"]["kind"], "transcript");
    assert_eq!(hits[0]["source"]["origin"]["source"], "opencode");
    assert_eq!(hits[0]["source"]["origin"]["document"], "ses_alpha");
    for left_out in ["SECRET-TOOL-OUTPUT", "SECRET-REASONING"] {
        assert!(
            fixture.search(left_out).await.is_empty(),
            "`{left_out}` is not filed"
        );
    }

    let second = fixture.mined().await;
    assert_eq!(
        second.result.as_ref().unwrap()["documents"],
        0,
        "re-running acquisition reads nothing: the cursor is past every session"
    );
    assert_eq!(
        fixture.search("rotate signing keys").await.len(),
        1,
        "and duplicates nothing"
    );

    history.grow_alpha();
    let third = fixture.mined().await;
    let summary = third.result.as_ref().unwrap();
    assert_eq!(
        summary["documents"], 1,
        "only the session that changed is read"
    );
    assert_eq!(
        summary["superseded"], 1,
        "its one chunk is replaced by the longer one"
    );
    assert_eq!(fixture.search("certificate revocation").await.len(), 1);

    fixture.daemon.shutdown().await;
}

fn options(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

async fn found(adapter: &WasmAdapter, pairs: &[(&str, &str)]) -> Vec<String> {
    let source = adapter.identify(None, &options(pairs)).unwrap();
    adapter
        .discover(&source, &json!(null), 100)
        .await
        .unwrap()
        .candidates
        .into_iter()
        .map(|c| c.external_id)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn dir_and_since_narrow_the_query_and_combine() {
    let _history = History::new().await;
    let adapter = adapter();

    assert_eq!(found(&adapter, &[]).await.len(), 4);
    assert_eq!(
        found(&adapter, &[("dir", "/home/me/notes")]).await,
        ["ses_bravo", "ses_charlie"]
    );
    assert_eq!(
        found(&adapter, &[("since", "2026-07-17")]).await,
        ["ses_charlie", "ses_delta"]
    );
    assert_eq!(
        found(
            &adapter,
            &[("since", "2026-07-17"), ("dir", "/home/me/notes")]
        )
        .await,
        ["ses_charlie"],
        "both filters apply"
    );
    // An RFC 3339 time with an offset is the instant it names: 2026-07-15 02:00 at +02:00 is the 15th 00:00 UTC.
    assert_eq!(
        found(&adapter, &[("since", "2026-07-15T02:00:00+02:00")]).await,
        ["ses_bravo", "ses_charlie", "ses_delta"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quote_in_dir_is_data_and_never_part_of_the_query() {
    let _history = History::new().await;
    let adapter = adapter();
    // The stand-in reads the literal back out of the query text, so an unescaped quote would end the literal early and
    // leave the rest as query: it would fail to be read, not come back empty.
    assert!(
        found(&adapter, &[("dir", "/x' OR '1'='1")])
            .await
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_dir_is_a_source_of_its_own_and_since_is_not() {
    let _history = History::new().await;
    let adapter = adapter();
    let identify = |pairs: &[(&str, &str)]| adapter.identify(None, &options(pairs)).unwrap();

    let plain = identify(&[]);
    assert_ne!(plain.id(), identify(&[("dir", "/home/me/project")]).id());
    assert_ne!(
        identify(&[("dir", "/home/me/project")]).id(),
        identify(&[("dir", "/home/me/notes")]).id()
    );
    assert_eq!(plain.id(), identify(&[("since", "2026-09")]).id());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_option_a_bad_date_or_a_control_character_is_refused() {
    let _history = History::new().await;
    let adapter = adapter();
    let refused = |pairs: &[(&str, &str)]| {
        adapter
            .identify(None, &options(pairs))
            .unwrap_err()
            .to_string()
    };

    let unknown = refused(&[("dates", "2026")]);
    assert!(
        unknown.contains("`dates`") && unknown.contains("`since`") && unknown.contains("`dir`"),
        "{unknown}"
    );
    let bad = refused(&[("since", "2026-02-30")]);
    assert!(
        bad.contains("2026-02-30") && bad.contains("not a date"),
        "{bad}"
    );
    assert!(refused(&[("dir", "/a\nb")]).contains("`dir`"));
    assert!(refused(&[("dir", "")]).contains("`dir`"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_star_in_dir_matches_any_run_of_characters_and_a_spelling_does_not_matter() {
    let _history = History::new().await;
    let adapter = adapter();
    let project = ["ses_alpha", "ses_delta"];

    assert_eq!(
        found(&adapter, &[("dir", "/home/me/*")]).await.len(),
        4,
        "every directory under /home/me"
    );
    assert_eq!(
        found(&adapter, &[("dir", "/home/*/project")]).await,
        project,
        "a star in the middle"
    );
    assert_eq!(
        found(&adapter, &[("dir", "*/pro*")]).await,
        project,
        "a pattern may start with a star"
    );
    assert!(found(&adapter, &[("dir", "/home/me/x*")]).await.is_empty());
    for spelling in [
        "/home/me/project/",
        "/home/me//project",
        "/home/me/./project",
        "/home/me/other/../project",
    ] {
        assert_eq!(
            found(&adapter, &[("dir", spelling)]).await,
            project,
            "{spelling}"
        );
    }
    let identify = |dir: &str| {
        adapter
            .identify(None, &options(&[("dir", dir)]))
            .unwrap()
            .id()
    };
    assert_eq!(identify("/home/me/project/"), identify("/home/me/project"));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_star_is_a_wildcard_in_dir_and_a_quote_or_bracket_is_a_character() {
    let _history = History::new().await;
    let adapter = adapter();
    // `?` and `[` are syntax in `GLOB`: they are escaped, so they match themselves and nothing else.
    assert!(
        found(&adapter, &[("dir", "/home/me/proj?ct")])
            .await
            .is_empty()
    );
    assert!(
        found(&adapter, &[("dir", "/home/me/pr[a-z]j")])
            .await
            .is_empty()
    );
    assert!(
        found(&adapter, &[("dir", "/home/me/o'brien")])
            .await
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dir_that_names_no_place_is_refused_rather_than_matching_nothing() {
    let _history = History::new().await;
    let adapter = adapter();
    let relative = adapter
        .identify(None, &options(&[("dir", "work/app")]))
        .unwrap_err()
        .to_string();
    assert!(
        relative.contains("not an absolute path") && relative.contains("`work/app`"),
        "{relative}"
    );
}
