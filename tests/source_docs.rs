//! The source guide against what ships (docs/writing-sources.md).
//!
//! These read files and build nothing, so they stay in the basic suite: the `wasm_` binaries, which build and run
//! components, are a separate suite (docs/development.md).

use std::path::PathBuf;

use memcastle::source::conformance;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn cases() -> PathBuf {
    root().join("tests/fixtures/sources/conformance")
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

#[test]
fn the_official_registry_published_with_the_docs_is_an_index_this_memcastle_reads() {
    // The file is edited by hand in a pull request and deployed as it is (docs/adr/040), so a typo in it would reach
    // every installation's default registry. It names the bundled sources by the repository that releases them, and
    // each entry's manifest must agree with what it says about itself.
    let text = std::fs::read_to_string(root().join("docs/registry.json")).unwrap();
    let index = memcastle::domain::SourceIndex::parse(&text).unwrap();

    for name in ["pi", "opencode", "claude", "codex"] {
        let source = index
            .find(name)
            .unwrap_or_else(|| panic!("the official registry does not list `{name}`"));
        assert_eq!(source.repository.as_deref(), Some("noirbizarre/memcastle"));
        let manifest =
            std::fs::read_to_string(root().join(format!("sources/{name}/memcastle-source.toml")))
                .unwrap();
        let manifest = memcastle::source::manifest::parse(&manifest, &[]).unwrap();
        assert_eq!(
            source.description, manifest.source.description,
            "`{name}`'s description in the registry differs from its manifest"
        );
    }
}
