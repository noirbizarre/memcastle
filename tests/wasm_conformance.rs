//! The same conformance cases, run against a built-in adapter and against a WebAssembly component
//! (docs/adr/026, docs/writing-sources.md).
//!
//! If the two kinds of source ever disagree about what the contract requires, a case fails for one and not the
//! other, here.
//!
//! This binary builds a component, so it is part of the `wasm_` suite; the guide's documentation guards, which build
//! nothing, are in `tests/source_docs.rs`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use memcastle::config::MiningConfig;
use memcastle::mining::adapters::directory::DirectoryAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::build::Project;
use memcastle::source::conformance;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cases() -> PathBuf {
    root().join("tests/fixtures/sources/conformance")
}

/// The reference source, built once for the whole test binary.
fn reference_component() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let project =
            Project::open(&root().join("sources/directory")).expect("the reference source opens");
        project.build().expect(
            "the reference source builds; `rustup target add wasm32-wasip2` provides its target",
        )
    })
}

fn wasm_directory() -> WasmAdapter {
    let project = Project::open(&root().join("sources/directory")).unwrap();
    let bytes = std::fs::read(reference_component()).unwrap();
    WasmAdapter::load(&project.manifest, &bytes, &MiningConfig::default()).unwrap()
}

fn assert_passes(report: &conformance::Report) {
    for case in &report.cases {
        assert!(
            case.failures.is_empty(),
            "case `{}` failed:\n{}",
            case.name,
            case.failures.join("\n")
        );
    }
}

#[tokio::test]
async fn the_native_directory_source_passes_every_conformance_case() {
    let report = conformance::run_all(&DirectoryAdapter::new(2 * 1024 * 1024), &cases())
        .await
        .unwrap();
    assert!(!report.cases.is_empty());
    assert_passes(&report);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_reference_webassembly_source_passes_the_same_conformance_cases() {
    let report = conformance::run_all(&wasm_directory(), &cases())
        .await
        .unwrap();
    assert!(!report.cases.is_empty());
    assert_passes(&report);
}
