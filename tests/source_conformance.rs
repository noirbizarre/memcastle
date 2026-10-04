//! The same conformance cases, run against a built-in adapter and against a WebAssembly component
//! (docs/adr/026, docs/writing-sources.md).
//!
//! If the two kinds of source ever disagree about what the contract requires, a case fails for one and not the
//! other, here.

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

/// The names in the first backticks of each table row after `heading`, up to the table's end.
fn documented_names(page: &str, heading: &str) -> Vec<String> {
    let after = page
        .split(heading)
        .nth(1)
        .unwrap_or_else(|| panic!("docs/writing-sources.md lacks `{heading}`"));
    after
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|line| line.split('`').nth(1).map(str::to_string))
        .collect()
}

#[test]
fn the_conformance_cases_the_guide_lists_are_exactly_the_cases_shipped() {
    // The guide is what a source author reads to know what they will be held to: a case that is shipped and not
    // documented, or documented and gone, is a surprise at the worst moment.
    let page = std::fs::read_to_string(root().join("docs/writing-sources.md")).unwrap();
    let mut documented = documented_names(&page, "The cases shipped with MemCastle:");
    let mut shipped: Vec<String> = conformance::load_cases(&cases())
        .unwrap()
        .into_iter()
        .map(|(case, _)| case.name)
        .collect();
    documented.sort();
    shipped.sort();
    assert_eq!(documented, shipped);
}

#[test]
fn every_source_diagnostic_code_is_explained_in_the_guide() {
    // A diagnostic code is a public identifier users grep for; the page they land on must say what it means.
    let page = std::fs::read_to_string(root().join("docs/writing-sources.md")).unwrap();
    let errors = std::fs::read_to_string(root().join("src/error.rs")).unwrap();
    let mut codes: Vec<&str> = errors
        .split("code(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .filter(|code| code.starts_with("memcastle::source::"))
        .collect();
    codes.sort_unstable();
    codes.dedup();
    assert!(codes.len() >= 10, "{codes:?}");
    for code in codes {
        assert!(
            page.contains(&format!("`{code}`")),
            "docs/writing-sources.md does not explain `{code}`"
        );
    }
}
