//! `memcastle integration` end to end: the real binary, a fake `pi` and `opencode` on `PATH`, and a throwaway home.
//!
//! The assets root is built here in the layout both a package and a checkout share (`integrations/<id>/`, `skills/`),
//! so the same lifecycle is driven through every way of choosing the root: the flag, the environment, and the prefix
//! the binary sits in (docs/adr/034).

#![cfg(unix)]

mod common;

use std::path::Path;

use assert_cmd::Command;
use common::agents::Machine;
use predicates::str::contains;

const MANIFEST: &str = r#"format = 1
[integration]
id = "ID"
version = "0.1.0"
description = "the ID integration"
[compatibility]
memcastle = ">=0.1"
agent = ">=1.0"
[agent]
kind = "ID"
entry = "dist/index.js"
[[assets]]
from = "dist"
to = "dist"
[skills]
install = true
"#;

/// Lay out an assets root with both integrations built, and the shared skills.
fn assets(root: &Path) {
    for id in ["pi", "opencode"] {
        let dir = root.join("integrations").join(id);
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("dist/index.js"),
            format!("export default '{id}'\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("memcastle-integration.toml"),
            MANIFEST.replace("ID", id),
        )
        .unwrap();
    }
    std::fs::create_dir_all(root.join("skills/wake-up")).unwrap();
    std::fs::write(root.join("skills/wake-up/SKILL.md"), "# wake\n").unwrap();
}

/// A machine with the fixture integrations at `assets/`.
fn machine() -> Machine {
    let machine = Machine::new();
    assets(&machine.path("assets"));
    machine
}

fn json(output: &[u8]) -> serde_json::Value {
    serde_json::from_slice(output)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(output)))
}

#[test]
fn the_lifecycle_installs_updates_and_removes_both_agents_from_a_development_checkout() {
    let machine = machine();
    let assets = machine.path("assets");
    let assets = assets.to_str().unwrap();

    // Piped, so JSON: the human sentences are held by the `integration::render` unit tests.
    let installed = machine
        .memcastle()
        .args(["integration", "install", "pi", "--assets-dir", assets])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let installed = json(&installed);
    assert_eq!(
        (
            &installed["id"],
            &installed["action"],
            &installed["version"]
        ),
        (&"pi".into(), &"installed".into(), &"0.1.0".into())
    );
    let installed = machine
        .memcastle()
        .args(["integration", "install", "opencode", "--assets-dir", assets])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let installed = json(&installed);
    assert_eq!(
        (
            &installed["id"],
            &installed["action"],
            &installed["version"]
        ),
        (&"opencode".into(), &"installed".into(), &"0.1.0".into())
    );

    assert!(machine.installed("pi").join("dist/index.js").is_file());
    assert!(
        machine
            .installed("pi")
            .join("skills/wake-up/SKILL.md")
            .is_file()
    );
    assert_eq!(
        machine.pi_packages(),
        [machine.installed("pi").display().to_string()]
    );
    assert!(
        std::fs::read_to_string(machine.plugin_file())
            .unwrap()
            .contains("dist/index.js")
    );

    let listed = machine
        .memcastle()
        .args(["integration", "list", "--json", "--assets-dir", assets])
        .output()
        .unwrap();
    let report = json(&listed.stdout);
    assert_eq!(report["assets_source"], "override");
    let states: Vec<_> = report["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap(), i["state"].as_str().unwrap()))
        .collect();
    assert_eq!(states, [("opencode", "installed"), ("pi", "installed")]);

    // A rebuild is a change even at the same version.
    std::fs::write(
        machine.path("assets/integrations/pi/dist/index.js"),
        "export default 'v2'\n",
    )
    .unwrap();
    machine
        .memcastle()
        .args(["integration", "update", "pi", "--assets-dir", assets])
        .assert()
        .success()
        .stdout(contains("\"action\": \"updated\""));

    machine
        .memcastle()
        .args(["integration", "remove", "pi"])
        .assert()
        .success()
        .stdout(contains("\"action\": \"removed\""));
    machine
        .memcastle()
        .args(["integration", "remove", "opencode"])
        .assert()
        .success();
    assert!(machine.pi_packages().is_empty());
    assert!(!machine.plugin_file().exists());
    assert!(!machine.installed("pi").exists() && !machine.installed("opencode").exists());
}

#[test]
fn installing_twice_reports_that_nothing_changed() {
    let machine = machine();
    let assets = machine.path("assets");
    let install = || {
        machine
            .memcastle()
            .args([
                "integration",
                "install",
                "pi",
                "--json",
                "--assets-dir",
                assets.to_str().unwrap(),
            ])
            .output()
            .unwrap()
    };

    assert_eq!(json(&install().stdout)["action"], "installed");
    let second = json(&install().stdout);

    assert_eq!(second["action"], "unchanged");
    assert_eq!(second["changes"].as_array().unwrap().len(), 0);
    assert_eq!(machine.pi_packages().len(), 1, "Pi was told once");
}

#[test]
fn the_assets_directory_environment_variable_chooses_the_root_like_the_flag() {
    let machine = machine();

    machine
        .memcastle()
        .env("MEMCASTLE_ASSETS_DIR", machine.path("assets"))
        .args(["integration", "install", "opencode"])
        .assert()
        .success();

    assert!(machine.plugin_file().is_file());
}

#[test]
fn an_installation_resolves_its_integrations_from_the_prefix_the_binary_sits_in() {
    // The packaged layout: <prefix>/bin/memcastle beside <prefix>/share/memcastle/{integrations,skills}.
    let machine = machine();
    let prefix = machine.path("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    assets(&prefix.join("share/memcastle"));
    let binary = prefix.join("bin/memcastle");
    let built = assert_cmd::cargo::cargo_bin("memcastle");
    // A hard link where the filesystem allows it, because a debug binary is large.
    std::fs::hard_link(&built, &binary)
        .or_else(|_| std::fs::copy(&built, &binary).map(|_| ()))
        .unwrap();

    let mut command = Command::new(&binary);
    machine.environment(&mut command);
    let listed = command
        .args(["integration", "list", "--json"])
        .output()
        .unwrap();

    let report = json(&listed.stdout);
    assert_eq!(report["assets_source"], "installed");
    assert_eq!(report["integrations"].as_array().unwrap().len(), 2);
}

#[test]
fn an_agent_outside_the_supported_range_is_refused_with_both_versions_and_nothing_is_written() {
    let machine = machine();

    machine
        .memcastle()
        .env("FAKE_AGENT_VERSION", "0.9.0")
        .args([
            "integration",
            "install",
            "pi",
            "--assets-dir",
            machine.path("assets").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("memcastle::integration::incompatible"))
        .stderr(contains("0.9.0"));

    assert!(!machine.installed("pi").exists());
    assert!(machine.pi_packages().is_empty());
}

#[test]
fn another_plugin_file_and_the_users_opencode_config_survive_install_and_remove() {
    let machine = machine();
    let config = machine.path("config/opencode");
    std::fs::create_dir_all(config.join("plugins")).unwrap();
    std::fs::write(config.join("plugins/mine.ts"), "// mine\n").unwrap();
    std::fs::write(config.join("opencode.json"), "{\"theme\":\"dark\"}\n").unwrap();
    let assets = machine.path("assets");

    machine
        .memcastle()
        .args([
            "integration",
            "install",
            "opencode",
            "--assets-dir",
            assets.to_str().unwrap(),
        ])
        .assert()
        .success();
    // `remove` reads no assets, so it takes no `--assets-dir`.
    machine
        .memcastle()
        .args(["integration", "remove", "opencode"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(config.join("plugins/mine.ts")).unwrap(),
        "// mine\n"
    );
    assert_eq!(
        std::fs::read_to_string(config.join("opencode.json")).unwrap(),
        "{\"theme\":\"dark\"}\n"
    );
}

#[test]
fn an_unknown_integration_lists_nothing_to_guess_from_and_points_at_list() {
    let machine = machine();

    machine
        .memcastle()
        .args([
            "integration",
            "install",
            "emacs",
            "--assets-dir",
            machine.path("assets").to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("memcastle::integration::not_found"))
        .stderr(contains("memcastle integration list"));
}

#[test]
fn an_assets_directory_that_does_not_exist_is_an_error_and_never_falls_back() {
    let machine = machine();

    machine
        .memcastle()
        .args(["integration", "list", "--assets-dir", "/nonexistent-assets"])
        .assert()
        .failure();
}

#[test]
fn removing_works_without_any_assets_so_it_outlives_the_package() {
    let machine = machine();
    let assets = machine.path("assets");
    machine
        .memcastle()
        .args([
            "integration",
            "install",
            "pi",
            "--assets-dir",
            assets.to_str().unwrap(),
        ])
        .assert()
        .success();
    std::fs::remove_dir_all(&assets).unwrap();

    machine
        .memcastle()
        .args(["integration", "remove", "pi"])
        .assert()
        .success();

    assert!(!machine.installed("pi").exists());
    assert!(machine.pi_packages().is_empty());
}

#[test]
fn removing_refuses_an_assets_directory_flag_because_it_reads_no_assets() {
    let machine = machine();

    // A flag that is accepted and ignored would suggest `remove` depends on the assets root.
    machine
        .memcastle()
        .args(["integration", "remove", "pi", "--assets-dir", "/tmp"])
        .assert()
        .failure()
        .stderr(contains("--assets-dir"));
}

#[test]
fn the_integration_help_lists_the_four_lifecycle_commands() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["integration", "--help"])
        .assert()
        .success()
        .stdout(contains("list"))
        .stdout(contains("install"))
        .stdout(contains("update"))
        .stdout(contains("remove"));
}
