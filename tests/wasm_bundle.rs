//! The sources that ship with MemCastle (docs/adr/033), packaged by the script the release runs and installed by name
//! from the bundle it writes: no registry, no network.
//!
//! This is what keeps "official sources ship with releases" from being a claim about the release workflow that only a
//! tag would test: the script, the index it writes, and the daemon's lookup of that index are all run here.

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

#[tokio::test(flavor = "multi_thread")]
async fn the_bundled_sources_are_packaged_indexed_and_installable_by_name_with_no_registry() {
    let out = tempfile::tempdir().unwrap();
    let bundle_dir = out.path().join("sources");
    bundle(&bundle_dir);

    let index: Value =
        serde_json::from_slice(&std::fs::read(bundle_dir.join("memcastle-index.json")).unwrap())
            .unwrap();
    let names: Vec<&str> = index["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["opencode", "pi"], "{index}");
    for source in index["sources"].as_array().unwrap() {
        let url = source["versions"][0]["url"].as_str().unwrap();
        assert!(
            bundle_dir.join(url).is_file(),
            "{url} is listed and must be beside the index"
        );
        assert!(
            bundle_dir.join(format!("{url}.sha256")).is_file(),
            "the checksum file is shipped with each archive"
        );
    }

    let sources_dir = out.path().join("installed");
    let configured = sources_dir.clone();
    let bundled = bundle_dir.clone();
    let daemon = TestDaemon::start_configured(move |config| {
        config.mining.sources_dir = Some(configured);
        config.mining.bundled_dir = Some(bundled);
    })
    .await;
    let client = reqwest::Client::new();
    let url = |path: &str| format!("{}{path}", daemon.base_url);

    let found: Value = client
        .get(url("/api/source-registry/search"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for entry in found["entries"].as_array().unwrap() {
        assert_eq!(entry["origin"], "bundled", "{entry}");
        assert!(
            entry["version"].is_string(),
            "a bundled source must run on the MemCastle that carries it: {entry}"
        );
    }
    assert_eq!(found["entries"].as_array().unwrap().len(), 2, "{found}");

    for name in ["pi", "opencode"] {
        let preview: Value = client
            .get(url(&format!("/api/source-registry/sources/{name}")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let installed: Value = client
            .post(url("/api/source-registry/install"))
            .json(&json!({"name": name, "consent": preview["consent_digest"], "enable": true}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(installed["source"]["origin"], "bundled", "{installed}");
        assert_eq!(installed["source"]["state"], "enabled", "{installed}");
        assert!(sources_dir.join(name).join("source.wasm").is_file());
    }
    daemon.shutdown().await;
}
