//! The integrations guide against what ships (docs/integrations.md).
//!
//! These read files and build nothing, so they stay in the basic suite on every OS.

use std::path::PathBuf;

use memcastle::integration::manifest;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn page() -> String {
    // A checkout with CRLF line endings (Windows) must read the same as one with LF.
    std::fs::read_to_string(root().join("docs/integrations.md"))
        .expect("docs/integrations.md exists")
        .replace("\r\n", "\n")
}

#[test]
fn every_integration_error_code_is_explained_on_the_page_a_user_is_sent_to() {
    // The code is what a user greps for, so a code the page does not list leaves them with a name and no next step.
    let errors = std::fs::read_to_string(root().join("src/error.rs")).unwrap();
    let page = page();
    let mut codes = 0;
    for line in errors.lines() {
        if let Some(code) = line.trim().strip_prefix("code(memcastle::integration::") {
            let code = code.trim_end_matches("),").trim_end_matches(')');
            assert!(
                page.contains(&format!("`memcastle::integration::{code}`")),
                "docs/integrations.md does not explain memcastle::integration::{code}"
            );
            codes += 1;
        }
    }
    assert!(
        codes >= 9,
        "found only {codes} integration codes in src/error.rs"
    );
}

#[test]
fn the_manifest_example_on_the_page_is_a_manifest_this_memcastle_accepts() {
    // A documented example that does not parse is the first thing someone adding an integration copies.
    let page = page();
    let section = page
        .split("## The manifest")
        .nth(1)
        .expect("the page has a manifest section");
    let example = section
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("the manifest section has a toml example");

    let parsed = manifest::parse(example)
        .unwrap_or_else(|e| panic!("the documented manifest is refused: {e}"));

    assert_eq!(parsed.integration.id, "pi");
}

#[test]
fn every_key_the_manifest_accepts_is_in_the_reference_table() {
    // Unknown keys are refused, so a key the table omits is one nobody can discover.
    let page = page();
    for key in [
        "format",
        "integration.id",
        "integration.version",
        "integration.description",
        "compatibility.memcastle",
        "compatibility.agent",
        "agent.kind",
        "agent.entry",
        "[[assets]]",
        "[[skills]]",
        "skills.name",
        "skills.local",
    ] {
        assert!(
            page.contains(&format!("| `{key}` |")),
            "the manifest table lacks `{key}`"
        );
    }
}

#[test]
fn every_integration_the_repository_ships_has_a_manifest_this_memcastle_accepts() {
    // `tests/integration_bundle.rs` checks the built package; this checks the committed manifests on every run, with no
    // bun, so a typo in one is found by the basic suite and not by a release.
    let running = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let mut found = 0;
    for entry in std::fs::read_dir(root().join("integrations"))
        .unwrap()
        .flatten()
    {
        let file = entry.path().join(manifest::MANIFEST_FILE);
        if !file.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let parsed = manifest::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(
            parsed.integration.id,
            entry.file_name().to_string_lossy(),
            "{}: the id must equal the directory",
            file.display()
        );
        let required = semver::VersionReq::parse(&parsed.compatibility.memcastle).unwrap();
        assert!(
            required.matches(&running),
            "{} requires MemCastle {required}, which this {running} does not satisfy",
            file.display()
        );
        found += 1;
    }
    assert_eq!(
        found, 2,
        "Pi and OpenCode ship; add the new one to this count and to packaging/integrations/build.sh"
    );
}

#[test]
fn every_skill_a_shipped_manifest_names_exists_where_the_manifest_says_it_is() {
    // The installer refuses a missing skill at install time; this finds the typo in the basic suite instead, from a
    // checkout with no build, because a skill needs no `dist/`.
    let mut named = 0;
    for entry in std::fs::read_dir(root().join("integrations"))
        .unwrap()
        .flatten()
    {
        let file = entry.path().join(manifest::MANIFEST_FILE);
        if !file.is_file() {
            continue;
        }
        let parsed = manifest::parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for skill in &parsed.skills {
            let base = if skill.local {
                entry.path().join("skills")
            } else {
                root().join("skills")
            };
            assert!(
                base.join(&skill.name).join("SKILL.md").is_file(),
                "{} names the skill `{}`, and {} has no SKILL.md",
                file.display(),
                skill.name,
                base.join(&skill.name).display()
            );
            named += 1;
        }
    }
    assert!(named > 0, "no shipped integration exposes a skill");
}

#[test]
fn the_release_script_bundles_exactly_the_integrations_that_have_a_manifest() {
    // The set that ships is a line in the script; a manifest the script does not list would be documented, tested and
    // never released.
    let script = std::fs::read_to_string(root().join("packaging/integrations/build.sh")).unwrap();
    let line = script
        .lines()
        .find(|line| line.starts_with("BUNDLED=("))
        .expect("the script lists what it bundles");
    for id in ["pi", "opencode"] {
        assert!(line.contains(id), "BUNDLED lacks {id}: {line}");
        assert!(
            root()
                .join(format!("integrations/{id}/{}", manifest::MANIFEST_FILE))
                .is_file(),
            "{id} has no manifest"
        );
    }
}
