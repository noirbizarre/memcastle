//! The sources that ship with MemCastle (docs/adr/039), packaged by the script the release runs: a bundle of unpacked
//! packages a daemon runs in place from the start, and the archives and registry index the documentation site publishes.
//!
//! This is what keeps "official sources ship with releases" from being a claim about the release workflow that only a
//! tag would test: the script, the bundle it writes, the index it writes and the daemon's use of the bundle are all run
//! here.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::TestDaemon;
use serde_json::{Value, json};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Run `packaging/sources/build.sh` into `out`, with `env` set for it.
fn bundle_with(out: &Path, env: &[(&str, &str)]) {
    let output = std::process::Command::new("bash")
        .arg(root().join("packaging/sources/build.sh"))
        .arg(assert_cmd::cargo::cargo_bin("memcastle"))
        .arg(out)
        .envs(env.iter().copied())
        .output()
        .expect("bash starts");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The release script's output with no environment, in a directory that goes with the test.
fn default_bundle() -> tempfile::TempDir {
    let out = tempfile::tempdir().unwrap();
    bundle_with(out.path(), &[]);
    out
}

fn read_index(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn indexed_urls(index: &Value) -> Vec<String> {
    index["sources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|source| source["versions"].as_array().unwrap())
        .map(|version| version["url"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_release_script_writes_the_unpacked_bundle_the_archives_and_an_index_that_lists_them() {
    let out = default_bundle();
    let out = out.path();

    for name in ["pi", "opencode"] {
        let package = out.join("sources").join(name);
        assert!(package.join("memcastle-source.toml").is_file(), "{name}");
        assert!(package.join("source.wasm").is_file(), "{name}");
    }
    let index = read_index(&out.join("memcastle-index.json"));
    let names: Vec<&str> = index["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["opencode", "pi"], "{index}");
    for url in indexed_urls(&index) {
        // No base URL: the archives sit beside the index, as a local registry directory has them.
        assert!(
            out.join("archives").join(&url).is_file(),
            "{url} is listed and must be among the archives"
        );
        assert!(
            out.join("archives").join(format!("{url}.sha256")).is_file(),
            "the checksum file is shipped with each archive"
        );
    }
}

#[test]
fn the_release_script_publishes_absolute_urls_and_never_reindexes_a_version_already_published() {
    let first = tempfile::tempdir().unwrap();
    bundle_with(
        first.path(),
        &[("SOURCES_BASE_URL", "https://example.test/releases/v1/")],
    );
    let published = first.path().join("memcastle-index.json");
    let index = read_index(&published);
    for url in indexed_urls(&index) {
        assert!(
            url.starts_with("https://example.test/releases/v1/"),
            "{url}"
        );
    }

    // The next release carries the same versions of these sources: the index and its digests stay what they were,
    // and no archive is offered for upload again, since the published URL points at the first release.
    let second = tempfile::tempdir().unwrap();
    bundle_with(
        second.path(),
        &[
            ("SOURCES_BASE_URL", "https://example.test/releases/v2"),
            ("SOURCES_PREVIOUS_INDEX", published.to_str().unwrap()),
        ],
    );
    assert_eq!(
        read_index(&second.path().join("memcastle-index.json")),
        index
    );
    assert_eq!(
        std::fs::read_dir(second.path().join("archives"))
            .unwrap()
            .count(),
        0
    );
    // The bundle is the release's own, whatever the index already lists.
    assert!(second.path().join("sources/pi/source.wasm").is_file());
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
