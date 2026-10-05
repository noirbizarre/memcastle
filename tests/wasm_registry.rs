//! Source registries end to end (docs/adr/033): a daemon that searches, installs and updates sources by name from a
//! registry or from the bundle, and holds what it fetches to the index's digest and the configured trust policy.
//!
//! The registry is a directory on disk (a registry whose location is a path, which is also what offline installation
//! is), written with the same library calls `memcastle source index` makes. The package is the reference source under
//! `sources/directory`, re-packaged at the versions and with the permissions each test needs.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::TestDaemon;
use common::wasm::reference_component;
use memcastle::config::TrustMode;
use memcastle::domain::SourceIndex;
use memcastle::source::{package, publish, signing};
use reqwest::StatusCode;
use serde_json::{Value, json};

const NAME: &str = "directory-wasm";
const NETWORK: &str = "[permissions]\nnetwork = true\n\n[permissions.filesystem]";
const LOCATOR: &str = "[permissions.filesystem]";

/// The reference component and manifest, built once per process.
fn reference() -> &'static (Vec<u8>, String) {
    static REFERENCE: OnceLock<(Vec<u8>, String)> = OnceLock::new();
    REFERENCE.get_or_init(reference_component)
}

/// The package at `version`, asking for the network too when `network`, and requiring MemCastle `memcastle`.
fn archive(version: &str, network: bool, memcastle: &str) -> Vec<u8> {
    let (component, manifest) = reference();
    let manifest = manifest
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""))
        .replace(
            "memcastle = \">=0.2\"",
            &format!("memcastle = \"{memcastle}\""),
        )
        .replace(LOCATOR, if network { NETWORK } else { LOCATOR });
    package::pack(&manifest, component, &[]).expect("the reference source packs")
}

/// A registry directory under construction: archives beside an index.
struct Registry {
    dir: tempfile::TempDir,
    key: Option<ed25519_dalek::SigningKey>,
}

impl Registry {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            key: None,
        }
    }

    fn signed_with(key: ed25519_dalek::SigningKey) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            key: Some(key),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn label(&self) -> String {
        self.path().display().to_string()
    }

    /// Publish `bytes` the way `memcastle source index` does.
    fn publish(&self, bytes: &[u8]) -> String {
        let index_path = self.path().join(memcastle::distribution::INDEX_FILE);
        let mut index = publish::read_index(&index_path, Some("test registry")).unwrap();
        let version = package::inspect(bytes).unwrap().manifest.source.version;
        let file = format!("{NAME}-{version}.tar.gz");
        std::fs::write(self.path().join(&file), bytes).unwrap();
        publish::add_archive(&mut index, bytes, &file, self.key.as_ref()).unwrap();
        publish::write_index(&index_path, &mut index).unwrap();
        version
    }

    fn index(&self) -> SourceIndex {
        publish::read_index(&self.path().join(memcastle::distribution::INDEX_FILE), None).unwrap()
    }
}

struct Fixture {
    daemon: TestDaemon,
    client: reqwest::Client,
    sources_dir: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn start(registries: Vec<String>) -> Self {
        Self::start_with(registries, None, |_| {}).await
    }

    async fn start_with(
        registries: Vec<String>,
        bundled: Option<&Path>,
        configure: impl FnOnce(&mut memcastle::config::MiningConfig),
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sources_dir = dir.path().join("sources");
        let configured = sources_dir.clone();
        // A bundle that does not exist unless a test makes one, so the machine's own installation never leaks in.
        let bundle = bundled.map_or_else(|| dir.path().join("no-bundle"), Path::to_path_buf);
        let daemon = TestDaemon::start_configured(move |config| {
            config.mining.sources_dir = Some(configured);
            config.mining.registries = registries;
            config.mining.bundled_dir = Some(bundle);
            configure(&mut config.mining);
        })
        .await;
        Self {
            daemon,
            client: reqwest::Client::new(),
            sources_dir,
            _dir: dir,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.daemon.base_url)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let response = self.client.get(self.url(path)).send().await.unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    }

    /// The digest the user agrees to, as the preview reports it.
    async fn consent_digest(&self, version: Option<&str>) -> String {
        let query = version.map_or_else(String::new, |v| format!("?version={v}"));
        let (status, preview) = self
            .get(&format!("/api/source-registry/sources/{NAME}{query}"))
            .await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        preview["consent_digest"].as_str().unwrap().to_string()
    }

    async fn install(&self, enable: bool) -> Value {
        let digest = self.consent_digest(None).await;
        let (status, body) = self
            .post(
                "/api/source-registry/install",
                json!({"name": NAME, "consent": digest, "enable": enable}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn listed(&self) -> Value {
        let (_, report) = self.get("/api/sources").await;
        report["adapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == NAME)
            .cloned()
            .unwrap_or(Value::Null)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_search_lists_what_a_registry_offers_and_a_source_installs_by_name_after_consent() {
    let registry = Registry::new();
    registry.publish(&archive("0.1.0", false, ">=0.2"));
    let fixture = Fixture::start(vec![registry.label()]).await;

    let (status, found) = fixture.get("/api/source-registry/search?q=wasm").await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let entry = &found["entries"][0];
    assert_eq!(entry["name"], NAME);
    assert_eq!(entry["version"], "0.1.0");
    assert_eq!(entry["origin"], "registry");
    assert!(entry["installed"].is_null(), "{entry}");
    let (_, nothing) = fixture.get("/api/source-registry/search?q=zzz").await;
    assert_eq!(nothing["entries"], json!([]));

    // Asking for a permission nobody agreed to is refused, and leaves nothing behind.
    let (status, refusal) = fixture
        .post("/api/source-registry/install", json!({"name": NAME}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refusal}");
    assert_eq!(refusal["code"], "memcastle::source::consent_required");
    assert!(fixture.listed().await.is_null());

    let installed = fixture.install(true).await;
    assert_eq!(installed["source"]["origin"], "registry");
    assert_eq!(installed["source"]["state"], "enabled");
    assert_eq!(installed["source"]["registry"], registry.label());
    assert!(fixture.sources_dir.join(NAME).join("source.wasm").is_file());

    let (_, after) = fixture.get("/api/source-registry/search").await;
    assert_eq!(after["entries"][0]["installed"]["version"], "0.1.0");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archive_that_is_not_the_one_the_index_published_is_refused_and_nothing_is_installed() {
    let registry = Registry::new();
    registry.publish(&archive("0.1.0", false, ">=0.2"));
    let fixture = Fixture::start(vec![registry.label()]).await;

    // The registry now serves other bytes than it published.
    let entry = &registry.index().sources[0].versions[0];
    std::fs::write(
        registry.path().join(&entry.url),
        archive("0.1.0", true, ">=0.2"),
    )
    .unwrap();

    let (status, body) = fixture
        .post("/api/source-registry/install", json!({"name": NAME}))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "memcastle::source::integrity");
    assert!(fixture.listed().await.is_null());
    assert!(!fixture.sources_dir.join(NAME).exists());
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_that_is_not_the_one_the_index_says_it_is_fails_its_integrity_check() {
    let registry = Registry::new();
    registry.publish(&archive("0.1.0", false, ">=0.2"));
    // The index lists the digest of a harmless 0.1.0 under a version the package inside does not carry.
    let index_path = registry.path().join(memcastle::distribution::INDEX_FILE);
    let text = std::fs::read_to_string(&index_path)
        .unwrap()
        .replace("\"0.1.0\"", "\"9.9.9\"");
    std::fs::write(&index_path, text).unwrap();
    let fixture = Fixture::start(vec![registry.label()]).await;

    let (status, body) = fixture
        .get(&format!("/api/source-registry/sources/{NAME}"))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "memcastle::source::integrity");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_newest_version_that_runs_here_is_chosen_and_an_incompatible_one_can_not_be_forced() {
    let registry = Registry::new();
    registry.publish(&archive("0.1.0", false, ">=0.2"));
    registry.publish(&archive("0.2.0", false, ">=99"));
    let fixture = Fixture::start(vec![registry.label()]).await;

    let (_, found) = fixture.get("/api/source-registry/search").await;
    assert_eq!(found["entries"][0]["version"], "0.1.0");

    let (status, body) = fixture
        .get(&format!(
            "/api/source-registry/sources/{NAME}?version=0.2.0"
        ))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "memcastle::source::not_in_registry");
    assert!(body["error"].as_str().unwrap().contains(">=99"), "{body}");

    let (status, body) = fixture.get("/api/source-registry/sources/unknown").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_required_trust_policy_installs_only_what_a_trusted_key_signed() {
    let key = signing::generate().unwrap();
    let stranger = signing::generate().unwrap();
    let trusted = vec![signing::public_key_text(&key.verifying_key())];

    let unsigned = Registry::new();
    unsigned.publish(&archive("0.1.0", false, ">=0.2"));
    let by_stranger = Registry::signed_with(stranger);
    by_stranger.publish(&archive("0.1.0", false, ">=0.2"));
    let by_trusted = Registry::signed_with(key.clone());
    by_trusted.publish(&archive("0.1.0", false, ">=0.2"));

    for refused in [&unsigned, &by_stranger] {
        let fixture = Fixture::start_with(vec![refused.label()], None, |mining| {
            mining.trust = TrustMode::Required;
            mining.trusted_keys = trusted.clone();
        })
        .await;
        let (status, body) = fixture
            .post("/api/source-registry/install", json!({"name": NAME}))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "memcastle::source::untrusted");
        assert!(fixture.listed().await.is_null());
        fixture.daemon.shutdown().await;
    }

    let fixture = Fixture::start_with(vec![by_trusted.label()], None, |mining| {
        mining.trust = TrustMode::Required;
        mining.trusted_keys = trusted.clone();
    })
    .await;
    let installed = fixture.install(false).await;
    assert_eq!(
        installed["source"]["signed_by"],
        signing::key_id(&key.verifying_key())
    );
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsigned_package_installs_when_trust_is_optional_and_a_forged_signature_does_not() {
    let key = signing::generate().unwrap();
    let trusted = vec![signing::public_key_text(&key.verifying_key())];

    let unsigned = Registry::new();
    unsigned.publish(&archive("0.1.0", false, ">=0.2"));
    let fixture = Fixture::start_with(vec![unsigned.label()], None, |mining| {
        mining.trusted_keys = trusted.clone()
    })
    .await;
    let installed = fixture.install(false).await;
    assert!(installed["source"]["signed_by"].is_null(), "{installed}");
    fixture.daemon.shutdown().await;

    // The index claims the trusted key signed it, but the signature is of other bytes: tampering, not "unsigned".
    let forged = Registry::signed_with(key.clone());
    forged.publish(&archive("0.1.0", false, ">=0.2"));
    let index_path = forged.path().join(memcastle::distribution::INDEX_FILE);
    let mut index = forged.index();
    index.sources[0].versions[0].signature = Some(signing::sign(&key, b"some other archive"));
    std::fs::write(&index_path, serde_json::to_string(&index).unwrap()).unwrap();
    let fixture = Fixture::start_with(vec![forged.label()], None, |mining| {
        mining.trusted_keys = trusted.clone()
    })
    .await;
    let (status, body) = fixture
        .post("/api/source-registry/install", json!({"name": NAME}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "memcastle::source::untrusted");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_keeps_the_state_needs_no_consent_for_the_same_permissions_and_asks_for_new_ones()
{
    let registry = Registry::new();
    registry.publish(&archive("0.1.0", false, ">=0.2"));
    let fixture = Fixture::start(vec![registry.label()]).await;
    fixture.install(true).await;

    let (_, nothing) = fixture.get("/api/source-registry/updates").await;
    assert_eq!(nothing["updates"], json!([]));

    // The same permissions at a newer version: the agreement already given still stands.
    registry.publish(&archive("0.1.1", false, ">=0.2"));
    let (_, check) = fixture.get("/api/source-registry/updates").await;
    assert_eq!(check["updates"][0]["name"], NAME);
    assert_eq!(check["updates"][0]["installed"], "0.1.0");
    assert_eq!(check["updates"][0]["available"], "0.1.1");

    let (status, outcomes) = fixture.post("/api/source-registry/update", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{outcomes}");
    assert_eq!(outcomes[0]["status"], "updated", "{outcomes}");
    let listed = fixture.listed().await;
    assert_eq!(listed["version"], "0.1.1");
    assert_eq!(
        listed["state"], "enabled",
        "an update keeps the state it had"
    );

    // A version that asks for the network as well must not be installed on the old agreement.
    registry.publish(&archive("0.2.0", true, ">=0.2"));
    let (_, outcomes) = fixture
        .post("/api/source-registry/update", json!({"name": NAME}))
        .await;
    assert_eq!(outcomes[0]["status"], "needs_consent", "{outcomes}");
    assert!(
        outcomes[0]["permissions"]
            .as_str()
            .unwrap()
            .contains("network"),
        "{outcomes}"
    );
    assert_eq!(
        fixture.listed().await["version"],
        "0.1.1",
        "nothing changed yet"
    );

    let digest = outcomes[0]["digest"].as_str().unwrap().to_string();
    let (_, outcomes) = fixture
        .post(
            "/api/source-registry/update",
            json!({"name": NAME, "consent": digest}),
        )
        .await;
    assert_eq!(outcomes[0]["status"], "updated", "{outcomes}");
    assert_eq!(fixture.listed().await["version"], "0.2.0");

    let (_, current) = fixture
        .post("/api/source-registry/update", json!({"name": NAME}))
        .await;
    assert_eq!(current[0]["status"], "current");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_installed_from_a_file_has_no_upstream_and_is_never_updated() {
    let registry = Registry::new();
    registry.publish(&archive("0.1.1", false, ">=0.2"));
    let fixture = Fixture::start(vec![registry.label()]).await;

    // The user's own file is installed under the same name, so a registry's newer version must not replace it.
    let response = fixture
        .client
        .post(fixture.url("/api/source-packages"))
        .query(&[(
            "consent",
            &memcastle::source::package::inspect(&archive("0.1.0", false, ">=0.2"))
                .unwrap()
                .manifest
                .permissions
                .normalized()
                .consent_digest(NAME),
        )])
        .body(archive("0.1.0", false, ">=0.2"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (_, check) = fixture.get("/api/source-registry/updates").await;
    assert_eq!(check["updates"], json!([]));
    let (status, body) = fixture
        .post("/api/source-registry/update", json!({"name": NAME}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["code"], "memcastle::source::not_in_registry");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bundled_sources_install_with_no_registry_and_ignore_the_trust_policy_the_bundle_carries() {
    let key = signing::generate().unwrap();
    let bundle = Registry::new();
    bundle.publish(&archive("0.1.0", false, ">=0.2"));
    // No registry is configured and trust is required, yet the bundle is as trusted as the MemCastle carrying it.
    let fixture = Fixture::start_with(vec![], Some(bundle.path()), |mining| {
        mining.trust = TrustMode::Required;
        mining.trusted_keys = vec![signing::public_key_text(&key.verifying_key())];
    })
    .await;

    let (_, found) = fixture.get("/api/source-registry/search").await;
    assert_eq!(found["entries"][0]["origin"], "bundled", "{found}");
    let installed = fixture.install(false).await;
    assert_eq!(installed["source"]["origin"], "bundled");

    // A newer release of MemCastle carries a newer bundle, and `update` finds it by origin, not by path.
    bundle.publish(&archive("0.1.1", false, ">=0.2"));
    let (_, outcomes) = fixture.post("/api/source-registry/update", json!({})).await;
    assert_eq!(outcomes[0]["status"], "updated", "{outcomes}");
    assert_eq!(fixture.listed().await["version"], "0.1.1");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registry_that_is_down_is_a_warning_for_search_and_an_error_when_it_was_asked_for() {
    let registry = Registry::new();
    registry.publish(&archive("0.1.0", false, ">=0.2"));
    let gone = registry.path().join("gone").display().to_string();
    let fixture = Fixture::start(vec![gone.clone(), registry.label()]).await;

    let (status, found) = fixture.get("/api/source-registry/search").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        found["entries"][0]["name"], NAME,
        "the registry that is up still answers"
    );
    assert!(
        found["warnings"][0].as_str().unwrap().contains(&gone),
        "{found}"
    );

    let (status, body) = fixture
        .get(&format!("/api/source-registry/search?registry={gone}"))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "memcastle::source::registry_unavailable");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_registry_chosen_for_one_install_replaces_the_configured_ones_for_it() {
    let configured = Registry::new();
    configured.publish(&archive("0.1.0", false, ">=0.2"));
    let chosen = Registry::new();
    chosen.publish(&archive("0.1.5", false, ">=0.2"));
    let fixture = Fixture::start(vec![configured.label()]).await;

    let (status, preview) = fixture
        .get(&format!(
            "/api/source-registry/sources/{NAME}?registry={}",
            chosen.label()
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["version"], "0.1.5");
    assert_eq!(preview["registry"], chosen.label());
    fixture.daemon.shutdown().await;
}

// --- the command line ---------------------------------------------------------------------------------------------

fn memcastle_for(daemon: &TestDaemon) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("memcastle"));
    command
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .env_remove("MEMCASTLE_MODE")
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdin(std::process::Stdio::null());
    command
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_publishes_signs_searches_installs_and_updates_without_extra_tooling() {
    let publisher = tempfile::tempdir().unwrap();
    let key_file = publisher.path().join("publisher.key");
    let keygen = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("memcastle"))
        .args(["source", "keygen"])
        .arg(&key_file)
        .output()
        .await
        .unwrap();
    assert!(
        keygen.status.success(),
        "{}",
        String::from_utf8_lossy(&keygen.stderr)
    );
    let printed = String::from_utf8_lossy(&keygen.stdout).to_string();
    let public = printed
        .lines()
        .find_map(|line| line.trim().strip_prefix("public key"))
        .expect("the public key is printed")
        .trim()
        .to_string();
    let again = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("memcastle"))
        .args(["source", "keygen"])
        .arg(&key_file)
        .output()
        .await
        .unwrap();
    assert!(!again.status.success(), "a key is never overwritten");

    let registry = tempfile::tempdir().unwrap();
    let publish_version = |version: &'static str| {
        let archive_path = registry.path().join(format!("{NAME}-{version}.tar.gz"));
        std::fs::write(&archive_path, archive(version, false, ">=0.2")).unwrap();
        let index = registry.path().join("memcastle-index.json");
        let key_file = key_file.clone();
        async move {
            let output = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("memcastle"))
                .args(["source", "index", "--sign"])
                .arg(&key_file)
                .arg("--output")
                .arg(&index)
                .arg(&archive_path)
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("(signed)"));
        }
    };
    publish_version("0.1.0").await;

    let location = registry.path().display().to_string();
    let configured = location.clone();
    let dir = tempfile::tempdir().unwrap();
    let sources_dir = dir.path().join("sources");
    let sources = sources_dir.clone();
    let daemon = TestDaemon::start_configured(move |config| {
        config.mining.sources_dir = Some(sources);
        config.mining.registries = vec![configured];
        config.mining.bundled_dir = Some(PathBuf::from("/nonexistent-bundle"));
        config.mining.trust = TrustMode::Required;
        config.mining.trusted_keys = vec![public];
    })
    .await;

    let search = memcastle_for(&daemon)
        .args(["source", "search", "wasm"])
        .output()
        .await
        .unwrap();
    assert!(
        search.status.success(),
        "{}",
        String::from_utf8_lossy(&search.stderr)
    );
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["entries"][0]["name"], NAME);

    // A script is never asked, so the daemon refuses until it is told to agree.
    let refused = memcastle_for(&daemon)
        .args(["source", "install", NAME])
        .output()
        .await
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("consent_required"));

    let installed = memcastle_for(&daemon)
        .args([
            "source",
            "install",
            "--yes",
            "--enable",
            &format!("{NAME}@0.1.0"),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let installed: Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(installed["source"]["origin"], "registry");
    assert!(installed["source"]["signed_by"].is_string(), "{installed}");

    publish_version("0.1.1").await;
    let check = memcastle_for(&daemon)
        .args(["source", "update", "--check"])
        .output()
        .await
        .unwrap();
    let check: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(check["updates"][0]["available"], "0.1.1");

    let updated = memcastle_for(&daemon)
        .args(["source", "update"])
        .output()
        .await
        .unwrap();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let updated: Value = serde_json::from_slice(&updated.stdout).unwrap();
    assert_eq!(updated[0]["status"], "updated");

    // Installed from the project's own archive file, with a path, nothing about the registry applies.
    let file = registry.path().join(format!("{NAME}-0.1.1.tar.gz"));
    let local = memcastle_for(&daemon)
        .args(["source", "install", "--yes"])
        .arg(&file)
        .output()
        .await
        .unwrap();
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    let local: Value = serde_json::from_slice(&local.stdout).unwrap();
    assert_eq!(local["replaced"], true);
    assert_eq!(local["source"]["origin"], "package");
    daemon.shutdown().await;
}
