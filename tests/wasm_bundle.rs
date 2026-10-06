//! The sources that ship with MemCastle (docs/adr/040), packaged by the script the release runs: a bundle of unpacked
//! packages a daemon runs in place from the start, and the archives a release attaches for the official registry.
//!
//! This is what keeps "official sources ship with releases" from being a claim about the release workflow that only a
//! tag would test: the script, the bundle and archives it writes and the daemon's use of the bundle are all run here.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::TestDaemon;
use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Run `packaging/sources/build.sh` into `out`.
fn bundle(out: &Path) {
    let output = std::process::Command::new("bash")
        .arg(root().join("packaging/sources/build.sh"))
        .arg(assert_cmd::cargo::cargo_bin("memcastle"))
        .arg(out)
        .output()
        .expect("bash starts");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The release script's output, in a directory that goes with the test.
fn default_bundle() -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    bundle(out.path());
    out
}

#[test]
fn the_release_script_writes_the_unpacked_bundle_and_the_archives_the_official_registry_resolves_to()
 {
    let out = default_bundle();
    let out = out.path();

    for name in ["pi", "opencode"] {
        let package = out.join("sources").join(name);
        assert!(package.join("memcastle-source.toml").is_file(), "{name}");
        assert!(package.join("source.wasm").is_file(), "{name}");

        // `<name>-<version>.tar.gz` is the file name the registry looks for among a release's assets, and the
        // version in it is the manifest's.
        let manifest = std::fs::read_to_string(package.join("memcastle-source.toml")).unwrap();
        let version = memcastle::source::manifest::parse(&manifest, &[])
            .unwrap()
            .source
            .version;
        let archive = format!("{name}-{version}.tar.gz");
        assert!(out.join("archives").join(&archive).is_file(), "{archive}");
        assert!(
            out.join("archives")
                .join(format!("{archive}.sha256"))
                .is_file(),
            "the checksum file is shipped with each archive"
        );
    }
    // The registry is a file in the documentation that names this repository, so a release builds no index.
    assert!(!out.join("memcastle-index.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bundled_sources_are_installed_from_the_start_and_only_need_enabling() {
    let out = default_bundle();
    let scratch = tempfile::tempdir().unwrap();

    let sources_dir = scratch.path().join("installed");
    let configured = sources_dir.clone();
    let bundled = out.path().join("sources");
    let daemon = TestDaemon::start_configured(move |config| {
        config.mining.sources_dir = Some(configured);
        config.mining.bundled_dir = Some(bundled);
    })
    .await;
    let client = reqwest::Client::new();
    let url = |path: &str| format!("{}{path}", daemon.base_url);

    let listed: Value = client
        .get(url("/api/sources"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for name in ["pi", "opencode"] {
        let source = listed["adapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not listed: {listed}"));
        assert_eq!(source["origin"], "bundled", "{source}");
        assert_eq!(source["state"], "installed", "{source}");
        assert!(
            !source["permissions"].is_null(),
            "what it may do is shown before it is enabled: {source}"
        );

        // Enabling is the consent: no digest to name, however much the source asks for.
        let enabled: Value = client
            .post(url(&format!("/api/source-packages/{name}/enable")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(enabled["state"], "enabled", "{enabled}");
        assert_eq!(enabled["origin"], "bundled", "{enabled}");

        let refused: Value = client
            .post(url("/api/source-registry/install"))
            .json(&json!({"name": name}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(refused["code"], "memcastle::source::bundled", "{refused}");
        let removed = client
            .delete(url(&format!("/api/source-packages/{name}")))
            .send()
            .await
            .unwrap();
        assert_eq!(removed.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(
            !sources_dir.join(name).exists(),
            "a bundled source runs where the release put it"
        );
    }

    // Turning one off is the user's, and sticks.
    let disabled: Value = client
        .post(url("/api/source-packages/pi/disable"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(disabled["state"], "disabled", "{disabled}");
    daemon.shutdown().await;
}
