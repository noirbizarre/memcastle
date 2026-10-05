//! Installable WebAssembly sources against a real daemon (docs/adr/026): install with consent, the lifecycle, mining
//! through the same pipeline as a built-in source, and refusing to run what was altered.
//!
//! The package is the reference source under `sources/directory`, built and packaged once per test process.

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::wasm::reference_component;
use common::{TestDaemon, wait_for_job_status};
use memcastle::domain::{Job, JobStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};

const NAME: &str = "directory-wasm";

/// The reference source's package, built once per process.
fn archive() -> &'static [u8] {
    static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
    ARCHIVE.get_or_init(|| {
        // Packed from the debug component `common::wasm` builds, and the manifest as it is in `sources/directory`: the
        // package a user installs is the same shape, and a release build with LTO would only make every test wait.
        let (component, manifest) = reference_component();
        let readme = std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("sources/directory/README.md"),
        )
        .ok()
        .map(|bytes| vec![("README.md".to_string(), bytes)])
        .unwrap_or_default();
        memcastle::source::package::pack(&manifest, &component, &readme)
            .expect("the reference source packs")
    })
}

/// A daemon whose sources are installed under a directory the test owns.
struct Fixture {
    daemon: TestDaemon,
    client: reqwest::Client,
    sources_dir: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sources_dir = dir.path().join("sources");
        let configured = sources_dir.clone();
        let daemon = TestDaemon::start_configured(move |config| {
            config.mining.sources_dir = Some(configured)
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

    async fn install(&self, query: &[(&str, &str)]) -> reqwest::Response {
        self.client
            .post(self.url("/api/source-packages"))
            .query(query)
            .body(archive().to_vec())
            .send()
            .await
            .expect("request")
    }

    /// The digest the daemon asks to be consented with, read from its refusal.
    async fn consent_digest(&self) -> String {
        let refusal = self.install(&[]).await;
        assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
        let body: Value = refusal.json().await.unwrap();
        assert_eq!(body["code"], "memcastle::source::consent_required");
        let help = body["help"].as_str().unwrap();
        help.split("--consent ")
            .nth(1)
            .unwrap()
            .trim_end_matches('`')
            .to_string()
    }

    async fn install_consented(&self, enable: bool) -> Value {
        let digest = self.consent_digest().await;
        let response = self
            .install(&[
                ("consent", &digest),
                ("enable", if enable { "true" } else { "false" }),
            ])
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
        response.json().await.unwrap()
    }

    async fn post(&self, path: &str) -> reqwest::Response {
        self.client
            .post(self.url(path))
            .send()
            .await
            .expect("request")
    }

    async fn listed(&self, name: &str) -> Value {
        let report: Value = self
            .client
            .get(self.url("/api/sources"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        report["adapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    }

    async fn mine(&self, tree: &Path) -> reqwest::Response {
        self.client
            .post(self.url("/api/jobs"))
            .json(&json!({"type": "mine", "source": NAME, "locator": tree, "requested_by": "test"}))
            .send()
            .await
            .expect("request")
    }

    async fn mined(&self, tree: &Path) -> Job {
        let response = self.mine(tree).await;
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        let job: Job = response.json().await.unwrap();
        wait_for_job_status(
            &self.client,
            &self.daemon.base_url,
            job.id,
            JobStatus::Completed,
        )
        .await
    }

    async fn search(&self, text: &str) -> Vec<Value> {
        self.client
            .get(self.url("/api/search"))
            .query(&[("q", text)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn notes() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("plan.md"),
        "the wasm source files this sentence about gazebos",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("other.md"),
        "a second document about lighthouses",
    )
    .unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_that_asks_for_permissions_is_refused_until_the_exact_permissions_are_consented_to()
 {
    let fixture = Fixture::start().await;

    let refusal: Value = fixture.install(&[]).await.json().await.unwrap();
    assert_eq!(refusal["code"], "memcastle::source::consent_required");
    assert!(
        refusal["error"].as_str().unwrap().contains("locator"),
        "{refusal}"
    );
    assert!(
        fixture.listed(NAME).await.is_null(),
        "a refused install leaves nothing behind"
    );

    let wrong = fixture.install(&[("consent", "0000")]).await;
    assert_eq!(
        wrong.status(),
        StatusCode::BAD_REQUEST,
        "a digest of something else is not consent"
    );

    let installed = fixture.install_consented(false).await;
    assert_eq!(installed["source"]["name"], NAME);
    assert_eq!(installed["source"]["state"], "installed");
    assert_eq!(installed["source"]["origin"], "package");
    assert_eq!(
        installed["source"]["permissions"]["filesystem"]["read"],
        json!(["locator"])
    );
    assert!(fixture.sources_dir.join(NAME).join("source.wasm").is_file());
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_installed_source_is_mined_only_while_enabled_and_idempotently() {
    let fixture = Fixture::start().await;
    let tree = notes();
    fixture.install_consented(false).await;

    let before = fixture.mine(tree.path()).await;
    assert_eq!(
        before.status(),
        StatusCode::CONFLICT,
        "installed is not enabled"
    );
    let body: Value = before.json().await.unwrap();
    assert_eq!(body["code"], "memcastle::source::not_enabled");

    let enabled: Value = fixture
        .post(&format!("/api/source-packages/{NAME}/enable"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(enabled["state"], "enabled");

    let first = fixture.mined(tree.path()).await;
    assert_eq!(
        first.result.as_ref().unwrap()["created"],
        2,
        "{:?}",
        first.result
    );
    let hits = fixture.search("gazebos").await;
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["source"]["origin"]["source"], NAME);

    let second = fixture.mined(tree.path()).await;
    let summary = second.result.as_ref().unwrap();
    assert_eq!(
        summary["created"], 0,
        "mining the same tree again files nothing: {summary}"
    );
    assert_eq!(summary["documents"], 0, "the cursor is past everything");

    fixture
        .post(&format!("/api/source-packages/{NAME}/disable"))
        .await;
    assert_eq!(
        fixture.mine(tree.path()).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.listed(NAME).await["state"], "disabled");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn enabling_and_disabling_are_idempotent_and_removal_deletes_the_files_but_not_what_was_mined()
 {
    let fixture = Fixture::start().await;
    let tree = notes();
    fixture.install_consented(true).await;
    fixture.mined(tree.path()).await;

    for _ in 0..2 {
        let state = fixture
            .post(&format!("/api/source-packages/{NAME}/enable"))
            .await;
        assert_eq!(state.status(), StatusCode::OK);
    }
    let removed = fixture
        .client
        .delete(fixture.url(&format!("/api/source-packages/{NAME}")))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);

    assert!(fixture.listed(NAME).await.is_null());
    assert!(!fixture.sources_dir.join(NAME).exists());
    assert_eq!(
        fixture.search("gazebos").await.len(),
        1,
        "removing a source does not forget what it mined"
    );
    let gone = fixture
        .client
        .delete(fixture.url(&format!("/api/source-packages/{NAME}")))
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_built_in_source_cannot_be_disabled_or_removed() {
    let fixture = Fixture::start().await;
    for name in ["directory"] {
        let disabled = fixture
            .post(&format!("/api/source-packages/{name}/disable"))
            .await;
        assert_eq!(disabled.status(), StatusCode::BAD_REQUEST, "{name}");
        let body: Value = disabled.json().await.unwrap();
        assert_eq!(body["code"], "memcastle::source::builtin");
    }
    let shown: Value = fixture
        .client
        .get(fixture.url("/api/source-packages/directory"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shown["origin"], "builtin");
    assert_eq!(shown["state"], "enabled");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_component_altered_after_install_is_unavailable_and_is_never_run() {
    let fixture = Fixture::start().await;
    let tree = notes();
    fixture.install_consented(true).await;
    assert_eq!(fixture.listed(NAME).await["state"], "enabled");

    // Anyone who can write the sources directory could otherwise swap in code the user never agreed to.
    let component = fixture.sources_dir.join(NAME).join("source.wasm");
    let mut bytes = std::fs::read(&component).unwrap();
    bytes.extend_from_slice(b"\0tampered");
    std::fs::write(&component, bytes).unwrap();

    let listed = fixture.listed(NAME).await;
    assert_eq!(listed["state"], "unavailable");
    assert!(
        listed["unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("no longer matches"),
        "{listed}"
    );
    let mine = fixture.mine(tree.path()).await;
    assert_eq!(mine.status(), StatusCode::CONFLICT);
    assert_eq!(
        fixture
            .post(&format!("/api/source-packages/{NAME}/enable"))
            .await
            .status(),
        StatusCode::CONFLICT
    );

    // Installing the package again repairs it.
    fixture.install_consented(false).await;
    assert_eq!(
        fixture.listed(NAME).await["state"],
        "enabled",
        "an upgrade keeps the state it had"
    );
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_source_and_a_garbage_package_are_refused_with_actionable_errors() {
    let fixture = Fixture::start().await;
    let tree = notes();

    let unknown = fixture.mine(tree.path()).await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let body: Value = unknown.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("known sources"),
        "{body}"
    );

    let garbage = fixture
        .client
        .post(fixture.url("/api/source-packages"))
        .body("not a package")
        .send()
        .await
        .unwrap();
    assert_eq!(garbage.status(), StatusCode::BAD_REQUEST);
    let body: Value = garbage.json().await.unwrap();
    assert_eq!(body["code"], "memcastle::source::package_invalid");
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_has_been_removed_while_a_job_was_queued_fails_the_job_not_the_daemon() {
    let fixture = Fixture::start().await;
    let tree = notes();
    fixture.install_consented(true).await;
    let job: Job = fixture.mine(tree.path()).await.json().await.unwrap();
    fixture
        .client
        .delete(fixture.url(&format!("/api/source-packages/{NAME}")))
        .send()
        .await
        .unwrap();

    // Either it ran before the removal or it failed after it; the daemon is up in both cases.
    for _ in 0..300 {
        let current = common::get_job(&fixture.client, &fixture.daemon.base_url, job.id).await;
        if matches!(current.status, JobStatus::Completed | JobStatus::Failed) {
            let health = fixture
                .client
                .get(fixture.url("/api/health"))
                .send()
                .await
                .unwrap();
            assert_eq!(health.status(), StatusCode::OK);
            fixture.daemon.shutdown().await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the job neither completed nor failed");
}
