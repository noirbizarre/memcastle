//! The Claude Code history source is exercised as the installed WebAssembly component, not as host Rust.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use memcastle::config::MiningConfig;
use memcastle::domain::RawDocument;
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::{build::Project, conformance};
use serde_json::json;

const SESSION: &str =
    include_str!("../sources/claude/fixtures/claude-projects/tree/-work-app/session-a.jsonl");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn project() -> Project {
    Project::open(&root().join("sources/claude")).expect("the Claude source opens")
}

fn adapter() -> WasmAdapter {
    static COMPONENT: OnceLock<PathBuf> = OnceLock::new();
    let component = COMPONENT.get_or_init(|| project().build().expect("the Claude source builds"));
    WasmAdapter::load(
        &project().manifest,
        &std::fs::read(component).unwrap(),
        &MiningConfig::default(),
    )
    .unwrap()
}

fn text(body: &str) -> String {
    adapter()
        .normalize(&RawDocument {
            external_id: "-work-app/session-a.jsonl".into(),
            revision: RawDocument::revision_of(body),
            body: body.into(),
            metadata: json!({"path":"/sessions/session-a.jsonl"}),
            occurred_at: None,
        })
        .unwrap()
        .segments
        .into_iter()
        .map(|segment| segment.text)
        .collect()
}

fn write(root: &Path, name: &str, content: &str, seconds: u64) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_claude_source_passes_the_conformance_cases_it_ships_with() {
    let report = conformance::run_all(&adapter(), &root().join("sources/claude/fixtures"))
        .await
        .unwrap();
    for case in report.cases {
        assert!(
            case.failures.is_empty(),
            "{}: {}",
            case.name,
            case.failures.join("\n")
        );
    }
}

#[test]
fn private_and_unknown_claude_records_are_never_normalized() {
    let normalized = text(SESSION);
    assert!(normalized.contains("[tool: Read]"));
    assert!(!normalized.contains("PRIVATE-REASONING"));
    assert!(!normalized.contains("SECRET-TOOL-OUTPUT"));
    assert!(!normalized.contains("compact_boundary"));
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_is_limited_to_session_jsonl_files_one_project_directory_down_and_never_follows_links()
 {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "project/session.jsonl", SESSION, 1_000);
    write(root.path(), "project/nested/deep.jsonl", SESSION, 1_000);
    write(root.path(), "settings.json", "credentials", 1_000);
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.path().join("project/session.jsonl"),
        root.path().join("project/link.jsonl"),
    )
    .unwrap();
    let source = adapter()
        .identify(root.path().to_str(), &Default::default())
        .unwrap();
    let found = adapter().discover(&source, &json!(null), 10).await.unwrap();
    assert_eq!(
        found
            .candidates
            .iter()
            .map(|candidate| candidate.external_id.as_str())
            .collect::<Vec<_>>(),
        ["project/session.jsonl"]
    );
}

#[test]
fn appending_a_transcript_keeps_existing_segments_byte_identical() {
    let before = adapter()
        .normalize(&RawDocument {
            external_id: "a.jsonl".into(),
            revision: "a".into(),
            body: SESSION.into(),
            metadata: "{}".into(),
            occurred_at: None,
        })
        .unwrap()
        .segments;
    let after = adapter().normalize(&RawDocument { external_id: "a.jsonl".into(), revision: "b".into(), body: format!("{SESSION}\n{{\"type\":\"user\",\"timestamp\":\"2026-07-14T15:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"and then?\"}}}}"), metadata: "{}".into(), occurred_at: None }).unwrap().segments;
    assert_eq!(&after[..before.len()], &before[..]);
}
