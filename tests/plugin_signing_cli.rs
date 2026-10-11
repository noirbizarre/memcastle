//! Signing is local publisher tooling and works before any daemon or configuration exists.

use std::collections::BTreeMap;

use assert_cmd::cargo::cargo_bin;
use memcastle::source::signing;

#[test]
fn a_plugin_archive_is_signed_locally_without_a_daemon_or_a_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let key = signing::generate().unwrap();
    let key_path = dir.path().join("publisher.key");
    signing::write_signing_key(&key_path, &key).unwrap();
    let archive = memcastle::plugin::package::pack(&BTreeMap::from([(
        "plugin.toml".to_string(),
        b"format = 1\nmemcastle = '>=0.4'\n[plugin]\nid = 'example'\nversion = '0.1.0'\nprovider = 'example'\ndescription = 'example'\nrepository = 'https://github.com/example/example'\nlicense = 'MIT'\n".to_vec(),
    )])).unwrap();
    let archive_path = dir.path().join("example-0.1.0.tar.gz");
    std::fs::write(&archive_path, &archive).unwrap();
    let output = std::process::Command::new(cargo_bin("memcastle"))
        .args([
            "--config",
            dir.path().join("missing.toml").to_str().unwrap(),
            "--json",
            "plugin",
            "sign",
        ])
        .arg(&archive_path)
        .arg("--key")
        .arg(&key_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["sha256"], memcastle::domain::sha256_hex(&archive));
    let signature: memcastle::domain::IndexSignature =
        serde_json::from_value(report["signature"].clone()).unwrap();
    signing::verify(&key.verifying_key(), &archive, &signature).unwrap();
}
