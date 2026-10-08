//! The Codex history source is tested as the installed WebAssembly component, not as a native adapter.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use memcastle::config::MiningConfig;
use memcastle::domain::RawDocument;
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::build::Project;
use memcastle::source::conformance;
use serde_json::json;

const ROLLOUT: &str = include_str!(
    "../sources/codex/fixtures/codex-rollouts/tree/2026/10/08/rollout-2026-10-08T10-00-00-session-a.jsonl"
);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn project() -> Project {
    Project::open(&root().join("sources/codex")).expect("the Codex source opens")
}

fn component() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| project().build().expect("the Codex source builds"))
}

fn adapter() -> WasmAdapter {
    WasmAdapter::load(
        &project().manifest,
        &std::fs::read(component()).unwrap(),
        &MiningConfig::default(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_codex_source_passes_the_conformance_cases_it_ships_with() {
    let report = conformance::run_all(&adapter(), &root().join("sources/codex/fixtures"))
        .await
        .unwrap();
    assert!(
        report.cases.iter().all(|case| case.failures.is_empty()),
        "{report:?}"
    );
}

#[test]
fn reasoning_and_tool_output_are_not_normalized_into_the_transcript() {
    let document = adapter()
        .normalize(&RawDocument {
            external_id: "2026/10/08/rollout-2026-10-08T10-00-00-session-a.jsonl".into(),
            revision: RawDocument::revision_of(ROLLOUT),
            body: ROLLOUT.into(),
            metadata: json!({"path": "/sessions/rollout.jsonl"}),
            occurred_at: None,
        })
        .unwrap();
    let text: String = document
        .segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect();
    assert!(text.contains("[tool call: read_file]"), "{text}");
    assert!(!text.contains("PRIVATE-REASONING"), "{text}");
    assert!(!text.contains("SECRET-TOOL-OUTPUT"), "{text}");
}
