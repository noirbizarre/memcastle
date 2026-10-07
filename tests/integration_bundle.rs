//! The integrations that ship with MemCastle (docs/adr/034), bundled by the script the release runs and installed from
//! the tree it writes and from the checkout it builds in: the same lifecycle through both ways of choosing the root.
//!
//! This is what keeps "Pi, OpenCode and Claude Code ship with every release, with no npm package" from being a claim about the
//! release workflow that only a tag would test. It needs bun, so it is `#[ignore]`d from the basic suite and run by
//! `mise run integrations:check` (`cargo test --test integration_bundle -- --ignored`); an ignored test says so in the
//! output, which a silent skip would not.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Once;

use common::agents::Machine;
use memcastle::integration::manifest::{self, MANIFEST_FILE};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The package tree, built once for every test in this binary: the script rebuilds `dist/` in the checkout, so two
/// builds at once would race.
fn package() -> PathBuf {
    static BUILD: Once = Once::new();
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bundled-integrations");
    BUILD.call_once(|| {
        let output = std::process::Command::new("bash")
            .arg(root().join("packaging/integrations/build.sh"))
            .arg(&out)
            .output()
            .expect("bash starts");
        assert!(
            output.status.success(),
            "packaging/integrations/build.sh failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    });
    out
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

#[test]
#[ignore = "needs bun; `mise run integrations:check` runs it"]
fn the_package_holds_each_integration_bundled_beside_its_manifest_and_the_skills_and_nothing_else()
{
    let package = package();

    assert_eq!(names(&package), ["integrations", "skills"]);
    assert_eq!(
        names(&package.join("integrations")),
        ["claude-code", "opencode", "pi"]
    );
    for id in ["pi", "opencode", "claude-code"] {
        let dir = package.join("integrations").join(id);
        // `dist/` and the manifest, plus a `skills/` directory only for an integration with skills of its own.
        let own: Vec<String> = names(&dir)
            .into_iter()
            .filter(|n| n != "dist" && n != MANIFEST_FILE && n != "skills")
            .collect();
        assert!(own.is_empty(), "{id} ships unexpected files: {own:?}");
        assert!(names(&dir).contains(&"dist".to_string()), "{id}");
        let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).unwrap();
        let parsed = manifest::parse(&text).unwrap_or_else(|e| panic!("{id}: {e}"));
        // The manifest must accept the MemCastle that ships it, or the package installs nothing.
        let running = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let required = semver::VersionReq::parse(&parsed.compatibility.memcastle).unwrap();
        assert!(
            required.matches(&running),
            "{id} requires MemCastle {required}, and this is {running}"
        );
        let entry = parsed.agent.entry.as_deref().unwrap();
        assert!(
            dir.join("dist").join(entry).is_file(),
            "{id}: dist/{entry} is missing"
        );
        // A skill the manifest names is in the package where the manifest says: the shared ones beside the
        // integrations, a local one inside the integration's own directory.
        for skill in &parsed.skills {
            let base = if skill.local {
                dir.join("skills")
            } else {
                package.join("skills")
            };
            assert!(
                base.join(&skill.name).join("SKILL.md").is_file(),
                "{id}: skill {} is missing from the package",
                skill.name
            );
        }
    }

    // The sources the bundle replaced, and the dependencies it inlined, must not travel with it.
    for file in files_under(&package) {
        let name = file.file_name().unwrap().to_string_lossy();
        assert!(
            !name.ends_with(".ts")
                && name != "bun.lock"
                && !file.components().any(|c| c.as_os_str() == "node_modules"),
            "{} must not ship",
            file.display()
        );
    }

    // Skills are the five directories of the repository, and not its working notes.
    let skills = names(&package.join("skills"));
    assert!(!skills.contains(&"README.md".to_string()), "{skills:?}");
    assert_eq!(
        skills,
        names(&root().join("skills"))
            .into_iter()
            .filter(|n| n != "README.md")
            .collect::<Vec<_>>()
    );
    for skill in skills {
        assert!(
            package
                .join("skills")
                .join(&skill)
                .join("SKILL.md")
                .is_file(),
            "{skill}"
        );
    }
}

#[test]
#[ignore = "needs bun; `mise run integrations:check` runs it"]
fn the_bundles_import_nothing_the_installed_copy_would_not_have() {
    // A bare import left in a bundle would fail the moment the agent loads it, on a machine with no `node_modules`.
    let package = package();
    for (id, file) in [("pi", "dist/extension.js"), ("opencode", "dist/index.js")] {
        let text =
            std::fs::read_to_string(package.join("integrations").join(id).join(file)).unwrap();
        for dependency in [
            "@modelcontextprotocol/sdk",
            "@earendil-works/pi-mcp",
            "smol-toml",
        ] {
            assert!(
                !text.contains(&format!("from \"{dependency}")),
                "{id} still imports {dependency} instead of carrying it"
            );
        }
    }
}

/// Install every integration from `assets`, install again, then remove them, checking the state on the way.
fn lifecycle_from(assets: &Path) {
    let machine = Machine::new();
    let flag = assets.to_str().unwrap();

    for id in ["pi", "opencode", "claude-code"] {
        machine
            .memcastle()
            .args(["integration", "install", id, "--assets-dir", flag])
            .assert()
            .success();
    }

    let listed = machine
        .memcastle()
        .args(["integration", "list", "--json", "--assets-dir", flag])
        .output()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    for integration in report["integrations"].as_array().unwrap() {
        assert_eq!(integration["state"], "installed", "{integration}");
    }

    // The installed Pi copy is a package Pi can read: its entry exists, and the skills sit beside it.
    let pi = machine.installed("pi");
    let package: serde_json::Value =
        serde_json::from_slice(&std::fs::read(pi.join("package.json")).unwrap()).unwrap();
    let extension = package["pi"]["extensions"][0].as_str().unwrap();
    assert!(pi.join(extension).is_file(), "{extension}");
    assert!(pi.join("skills/wake-up/SKILL.md").is_file());
    // Whatever layout `assets` is, each integration exposes exactly the skills its own manifest names.
    for id in ["pi", "opencode", "claude-code"] {
        let text =
            std::fs::read_to_string(assets.join("integrations").join(id).join(MANIFEST_FILE))
                .unwrap();
        let declared: Vec<String> = manifest::parse(&text)
            .unwrap()
            .skills
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        assert!(!declared.is_empty(), "{id} exposes no skill");
        let mut installed = names(&machine.installed(id).join("skills"));
        installed.sort();
        let mut expected = declared;
        expected.sort();
        assert_eq!(installed, expected, "{id}");
        for skill in &expected {
            assert!(
                machine
                    .installed(id)
                    .join("skills")
                    .join(skill)
                    .join("SKILL.md")
                    .is_file(),
                "{id}: {skill}"
            );
        }
    }
    assert_eq!(machine.pi_packages(), [pi.display().to_string()]);
    // OpenCode's shim points at a file that exists.
    let shim = std::fs::read_to_string(machine.plugin_file()).unwrap();
    assert!(
        machine.installed("opencode").join("index.js").is_file(),
        "{shim}"
    );
    assert!(
        machine
            .installed("opencode")
            .join("skills/wake-up/SKILL.md")
            .is_file()
    );
    assert_eq!(machine.claude_plugins(), ["memcastle@memcastle-local"]);

    for id in ["pi", "opencode", "claude-code"] {
        let again = machine
            .memcastle()
            .args(["integration", "install", id, "--json", "--assets-dir", flag])
            .output()
            .unwrap();
        let outcome: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
        assert_eq!(outcome["action"], "unchanged", "{id}: {outcome}");
        machine
            .memcastle()
            .args(["integration", "remove", id])
            .assert()
            .success();
    }
    assert!(
        machine.pi_packages().is_empty()
            && machine.claude_plugins().is_empty()
            && !machine.plugin_file().exists()
    );
}

#[test]
#[ignore = "needs bun; `mise run integrations:check` runs it"]
fn the_packaged_tree_installs_and_removes_every_integration() {
    lifecycle_from(&package());
}

#[test]
#[ignore = "needs bun; `mise run integrations:check` runs it"]
fn the_development_checkout_installs_and_removes_every_integration_through_the_same_code() {
    // Building the package built `dist/` in the checkout, which is all development mode needs.
    package();
    lifecycle_from(&root());
}
