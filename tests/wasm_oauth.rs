//! A source that signs in with OAuth, end to end against a real daemon and the real command line (docs/adr/039).
//!
//! The source is a scaffolded Rust project whose `read` returns the access token the host hands it as the document, so
//! what a run mined shows which token the source was given. The provider is a small fake of the three endpoints a public
//! client talks to. Everything else is real: the package is installed with consent, `memcastle source auth` runs the
//! device flow through the daemon, the token is kept in the credentials file, renewed when it is about to expire, and
//! a miner runs with it.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Form, Json, Router};
use common::TestDaemon;
use common::wasm::{seed_lockfile, share_target};
use memcastle::domain::{Job, JobStatus};
use memcastle::source::build::Project;
use memcastle::source::scaffold::{Template, init};
use reqwest::StatusCode as Status;
use serde_json::{Value, json};

const NAME: &str = "oauth-demo";

// --- the fake provider ---

#[derive(Default)]
struct Provider {
    base: String,
    /// How many device polls answer `authorization_pending` before the user "finishes".
    pending: AtomicUsize,
    refreshes: AtomicUsize,
    issued: AtomicUsize,
    revoke: AtomicBool,
    expires_in: AtomicU64,
}

fn oauth_error(code: &str) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": code}))).into_response()
}

async fn device(State(provider): State<Arc<Provider>>) -> impl IntoResponse {
    Json(json!({
        "device_code": "device-secret",
        "user_code": "WDJB-MJHT",
        "verification_uri": format!("{}/activate", provider.base),
        "expires_in": 120,
        "interval": 0,
    }))
}

async fn token(
    State(provider): State<Arc<Provider>>,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    match form.get("grant_type").map(String::as_str).unwrap_or("") {
        "urn:ietf:params:oauth:grant-type:device_code" => {
            // Decremented by compare-exchange: the newer atomic helpers are past this crate's minimum Rust version, and
            // the older one is deprecated.
            let mut left = provider.pending.load(Ordering::SeqCst);
            while left > 0 {
                match provider.pending.compare_exchange(
                    left,
                    left - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return oauth_error("authorization_pending"),
                    Err(now) => left = now,
                }
            }
        }
        "refresh_token" => {
            provider.refreshes.fetch_add(1, Ordering::SeqCst);
            if provider.revoke.load(Ordering::SeqCst) {
                return oauth_error("invalid_grant");
            }
        }
        _ => return oauth_error("unsupported_grant_type"),
    }
    let n = provider.issued.fetch_add(1, Ordering::SeqCst) + 1;
    Json(json!({
        "access_token": format!("access-{n}"),
        "refresh_token": "refresh-secret-1",
        "token_type": "Bearer",
        "expires_in": provider.expires_in.load(Ordering::SeqCst),
        "scope": "read",
    }))
    .into_response()
}

async fn provider() -> Arc<Provider> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Arc::new(Provider {
        base: format!("http://{}", listener.local_addr().unwrap()),
        expires_in: AtomicU64::new(3600),
        ..Provider::default()
    });
    let app = Router::new()
        .route("/device", post(device))
        .route("/token", post(token))
        .with_state(Arc::clone(&provider));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    provider
}

// --- the source ---

/// The component and manifest text of the OAuth demo source, built once per process.
///
/// What is built does not depend on the endpoints, so each test sets its own provider's in the manifest it packs.
fn built() -> &'static (Vec<u8>, String) {
    static BUILT: OnceLock<(Vec<u8>, String)> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let (root, _) = init(dir.path(), NAME, Template::Rust).unwrap();
        let lib = root.join("src/lib.rs");
        let patched = std::fs::read_to_string(&lib).unwrap().replace(
            "let Ok(body) = std::fs::read_to_string(&path) else {\n            return Ok(None);\n        };",
            "let _ = &path;\n        let body = memcastle::source::host::access_token().map_err(SourceError::Failed)?;",
        );
        std::fs::write(&lib, patched).unwrap();
        let manifest_path = root.join("memcastle-source.toml");
        let manifest = share_target(&std::fs::read_to_string(&manifest_path).unwrap(), NAME)
            .replace("needs_credentials = false", "needs_credentials = true");
        std::fs::write(
            &manifest_path,
            format!(
                "{manifest}\n[permissions.oauth]\nclient_id = \"demo-client\"\nscopes = [\"read\"]\ntoken_url = \"https://auth.example.com/token\"\ndevice_authorization_url = \"https://auth.example.com/device\"\n"
            ),
        )
        .unwrap();
        seed_lockfile(&root);
        let project = Project::open(&root).unwrap();
        project.build().expect("the OAuth demo source builds");
        (
            std::fs::read(project.component_path()).unwrap(),
            std::fs::read_to_string(&manifest_path).unwrap(),
        )
    })
}

/// The package for a provider at `base`.
fn archive(base: &str) -> Vec<u8> {
    let (component, manifest) = built();
    let manifest = manifest.replace("https://auth.example.com", base);
    memcastle::source::package::pack(&manifest, component, &[]).expect("the package packs")
}

// --- the daemon and the command line ---

struct Fixture {
    daemon: TestDaemon,
    provider: Arc<Provider>,
    client: reqwest::Client,
    credentials_dir: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let provider = provider().await;
        let dir = tempfile::tempdir().unwrap();
        let credentials_dir = dir.path().join("credentials");
        let (sources, credentials) = (dir.path().join("sources"), credentials_dir.clone());
        let daemon = TestDaemon::start_configured(move |config| {
            config.mining.sources_dir = Some(sources);
            // A file, so the test never touches the keyring of the machine it runs on.
            config.credentials.backend = memcastle::config::CredentialBackend::File;
            config.credentials.dir = Some(credentials);
        })
        .await;
        Self {
            daemon,
            provider,
            client: reqwest::Client::new(),
            credentials_dir,
            _dir: dir,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.daemon.base_url)
    }

    async fn install(&self) {
        let package = archive(&self.provider.base);
        let refusal: Value = self
            .client
            .post(self.url("/api/source-packages"))
            .body(package.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(refusal["code"], "memcastle::source::consent_required");
        let described = refusal["error"].as_str().unwrap();
        assert!(
            described.contains("sign in with OAuth at 127.0.0.1"),
            "consent names the sign-in: {described}"
        );
        let digest = refusal["help"]
            .as_str()
            .unwrap()
            .split("--consent ")
            .nth(1)
            .unwrap()
            .trim_end_matches('`')
            .to_string();
        let response = self
            .client
            .post(self.url("/api/source-packages"))
            .query(&[("consent", digest.as_str()), ("enable", "true")])
            .body(package)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            Status::OK,
            "{}",
            response.text().await.unwrap()
        );
    }

    /// `memcastle ...` against this daemon, stdout and stderr captured.
    async fn cli(&self, args: &[&str]) -> (bool, String, String) {
        let output = tokio::process::Command::new(assert_cmd::cargo::cargo_bin("memcastle"))
            .args(args)
            .env("MEMCASTLE_PALACE_PATH", &self.daemon.palace_path)
            .env_remove("MEMCASTLE_MODE")
            .env_remove("MEMCASTLE_AUTH_ENABLED")
            .env_remove("MEMCASTLE_AUTH_TOKEN")
            .stdin(Stdio::null())
            .output()
            .await
            .unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    async fn source(&self) -> Value {
        self.client
            .get(self.url(&format!("/api/source-packages/{NAME}")))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn mine(&self, tree: &Path) -> Job {
        let job: Job = self
            .client
            .post(self.url("/api/jobs"))
            .json(&json!({"type": "mine", "source": NAME, "locator": tree, "requested_by": "test"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        for _ in 0..300 {
            let current: Job = self
                .client
                .get(self.url(&format!("/api/jobs/{}", job.id)))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if matches!(current.status, JobStatus::Completed | JobStatus::Failed) {
                return current;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("the job did not finish");
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

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "unused: the source returns its token instead",
    )
    .unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_signs_in_cannot_be_mined_until_it_is_and_the_error_says_how() {
    let fixture = Fixture::start().await;
    fixture.install().await;
    let tree = tree();

    let job = fixture.mine(tree.path()).await;

    assert_eq!(job.status, JobStatus::Failed);
    let error = job.error.unwrap();
    assert!(
        error.contains("memcastle source auth oauth-demo"),
        "{error}"
    );
    assert!(
        fixture.source().await["auth"]["signed_in"] == false,
        "the source says it is not signed in"
    );
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn source_auth_signs_a_source_in_and_a_run_is_given_the_token_and_no_secret_is_kept_anywhere_else()
 {
    let fixture = Fixture::start().await;
    fixture.install().await;
    fixture.provider.pending.store(1, Ordering::SeqCst);

    let (ok, stdout, stderr) = fixture.cli(&["source", "auth", NAME]).await;

    assert!(ok, "{stderr}");
    // Instructions to the person, on stderr; the one answer, with no token in it, on stdout.
    assert!(stderr.contains("WDJB-MJHT"), "the code to type: {stderr}");
    let answer: Value = serde_json::from_str(&stdout).expect("stdout is the JSON answer");
    assert_eq!(answer["signed_in"], true);
    assert_eq!(answer["stored_in"], "file");
    assert!(
        !stdout.contains("access-") && !stdout.contains("refresh-"),
        "{stdout}"
    );
    assert!(
        fixture
            .credentials_dir
            .join(format!("{NAME}.json"))
            .is_file()
    );
    let shown = fixture.source().await;
    assert_eq!(shown["auth"]["signed_in"], true);
    assert_eq!(shown["auth"]["scopes"], json!(["read"]));

    let job = fixture.mine(tree().path()).await;
    assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
    let hits = fixture.search("access-1").await;
    assert_eq!(
        hits.len(),
        1,
        "the source was handed the access token the provider issued: {hits:?}"
    );

    // The refresh token is in the credentials file and nowhere else the daemon wrote: not the palace, not the
    // configuration, not what a source mined.
    let mut searched = 0;
    for root in [
        fixture.daemon.palace_path.clone(),
        fixture.daemon.config_path.clone(),
    ] {
        for file in files_under(&root) {
            searched += 1;
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !contains(&bytes, b"refresh-secret-1"),
                "the refresh token is in {}",
                file.display()
            );
        }
    }
    assert!(searched > 0);
    let kept =
        std::fs::read_to_string(fixture.credentials_dir.join(format!("{NAME}.json"))).unwrap();
    assert!(kept.contains("refresh-secret-1"));
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_about_to_expire_is_renewed_for_the_run_that_needs_it_and_signing_in_again_replaces_it()
 {
    let fixture = Fixture::start().await;
    fixture.install().await;
    // Under the renewal margin, so every use renews it first.
    fixture.provider.expires_in.store(30, Ordering::SeqCst);
    assert!(fixture.cli(&["source", "auth", NAME]).await.0);
    assert_eq!(fixture.provider.refreshes.load(Ordering::SeqCst), 0);

    fixture.provider.expires_in.store(3600, Ordering::SeqCst);
    let job = fixture.mine(tree().path()).await;

    assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
    assert!(fixture.provider.refreshes.load(Ordering::SeqCst) >= 1);
    // The token the source saw is the renewed one, not the one it was signed in with.
    assert!(fixture.search("access-1").await.is_empty());
    assert!(!fixture.search("access-2").await.is_empty());

    let issued = fixture.provider.issued.load(Ordering::SeqCst);
    assert!(fixture.cli(&["source", "auth", NAME]).await.0);
    assert_eq!(
        fixture.provider.issued.load(Ordering::SeqCst),
        issued + 1,
        "signing in again asks the provider again"
    );
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_the_provider_revoked_ends_the_run_with_the_fix_and_is_forgotten() {
    let fixture = Fixture::start().await;
    fixture.install().await;
    fixture.provider.expires_in.store(30, Ordering::SeqCst);
    assert!(fixture.cli(&["source", "auth", NAME]).await.0);
    fixture.provider.revoke.store(true, Ordering::SeqCst);

    let job = fixture.mine(tree().path()).await;

    assert_eq!(job.status, JobStatus::Failed);
    let error = job.error.unwrap();
    assert!(
        error.contains("memcastle source auth oauth-demo"),
        "{error}"
    );
    assert!(
        !fixture
            .credentials_dir
            .join(format!("{NAME}.json"))
            .exists(),
        "a credential that can never work again is not kept"
    );
    assert_eq!(fixture.source().await["auth"]["signed_in"], false);
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_miner_with_the_oauth_credential_is_unavailable_until_the_source_is_signed_in_and_never_shows_a_token()
 {
    let fixture = Fixture::start().await;
    fixture.install().await;
    let tree = tree();
    let locator = tree.path().to_str().unwrap();
    let put = |body: Value| {
        let request = fixture
            .client
            .put(fixture.url("/api/miners/demo"))
            .json(&body);
        async move { request.send().await.unwrap() }
    };

    let created = put(json!({
        "source": NAME, "locator": locator, "credential": {"type": "oauth"}, "enabled": false
    }))
    .await;
    assert_eq!(
        created.status(),
        Status::OK,
        "{}",
        created.text().await.unwrap()
    );

    // Enabling a miner whose source is not signed in is refused, with the command that fixes it.
    let refused = fixture
        .client
        .post(fixture.url("/api/miners/demo/enable"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), Status::BAD_REQUEST);
    let body: Value = refused.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("memcastle source auth oauth-demo"),
        "{body}"
    );

    assert!(fixture.cli(&["source", "auth", NAME]).await.0);
    let enabled = fixture
        .client
        .post(fixture.url("/api/miners/demo/enable"))
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), Status::OK);
    let miner: Value = fixture
        .client
        .get(fixture.url("/api/miners/demo"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(miner["state"], "ready", "{miner}");
    assert_eq!(
        miner["credential"],
        json!({"kind": "oauth", "available": true})
    );
    let file = std::fs::read_to_string(&fixture.daemon.config_path).unwrap();
    assert!(file.contains("type = \"oauth\""), "{file}");
    assert!(
        !file.contains("access-") && !file.contains("refresh-"),
        "no token in the configuration file: {file}"
    );

    let run = fixture
        .client
        .post(fixture.url("/api/miners/demo/run"))
        .send()
        .await
        .unwrap();
    assert!(run.status().is_success(), "{}", run.text().await.unwrap());
    fixture.daemon.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_source_removes_its_credentials_and_a_source_that_does_not_sign_in_cannot_be_authenticated()
 {
    let fixture = Fixture::start().await;
    fixture.install().await;
    assert!(fixture.cli(&["source", "auth", NAME]).await.0);
    assert!(
        fixture
            .credentials_dir
            .join(format!("{NAME}.json"))
            .is_file()
    );

    let removed = fixture
        .client
        .delete(fixture.url(&format!("/api/source-packages/{NAME}")))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), Status::OK);
    assert!(
        !fixture
            .credentials_dir
            .join(format!("{NAME}.json"))
            .exists()
    );

    let (ok, _, stderr) = fixture.cli(&["source", "auth", "directory"]).await;
    assert!(!ok);
    assert!(
        stderr.contains("memcastle::credential::oauth_unsupported"),
        "{stderr}"
    );
    let (ok, _, stderr) = fixture.cli(&["source", "auth", NAME]).await;
    assert!(!ok);
    assert!(stderr.contains("memcastle::source::not_found"), "{stderr}");
    fixture.daemon.shutdown().await;
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
