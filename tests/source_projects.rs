//! Source projects end to end: `memcastle source init`, `build`, `test` and `package`, the sandbox a component runs
//! in, and installing the result through the CLI (docs/adr/026, docs/writing-sources.md).
//!
//! Every project here is built for real with `cargo build --target wasm32-wasip2`, into one shared target directory so
//! the dependencies compile once. Where a test needs a misbehaving source it scaffolds a project and patches the
//! source: the sandbox is only proven by running code that tries to leave it.

mod common;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use assert_cmd::Command as Blocking;
use assert_cmd::cargo::cargo_bin;
use common::TestDaemon;
use memcastle::config::MiningConfig;
use memcastle::domain::{Candidate, Cursor, SourceRef};
use memcastle::error::Error;
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::build::Project;
use memcastle::source::scaffold::{Template, init};
use predicates::str::contains;
use serde_json::Value;

/// Where every project in this binary builds, so Cargo compiles `wit-bindgen` and friends once.
fn shared_target() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("source-projects")
}

/// Point a scaffolded project's build at the shared target directory.
fn share_target(manifest: &str, name: &str) -> String {
    let target = shared_target();
    // A debug build: a project is built a dozen times here and nothing about the sandbox depends on optimisation, so
    // the release profile's LTO would only make the suite wait.
    manifest
        .replace("\"--release\", ", "")
        .replace(
            "\"--target-dir\", \"target\"]",
            &format!("\"--target-dir\", '{}']", target.display()),
        )
        .replace(
            &format!(
                "output = \"target/wasm32-wasip2/release/{}.wasm\"",
                name.replace('-', "_")
            ),
            &format!(
                "output = '{}'",
                target
                    .join("wasm32-wasip2/debug")
                    .join(format!("{}.wasm", name.replace('-', "_")))
                    .display()
            ),
        )
}

/// Make an `init`-ed project in `dir/name` build into the shared target directory too.
fn share_target_of(dir: &Path, name: &str) {
    let manifest = dir.join(name).join("memcastle-source.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, share_target(&text, name)).unwrap();
}

/// A scaffolded, patched and built project.
struct Built {
    _dir: tempfile::TempDir,
    project: Project,
}

fn scaffold(
    name: &str,
    template: Template,
    patch_lib: impl Fn(&str) -> String,
    patch_manifest: impl Fn(&str) -> String,
) -> Built {
    let dir = tempfile::tempdir().unwrap();
    let (root, _) = init(dir.path(), name, template).unwrap();
    let lib = root.join("src/lib.rs");
    let patched = patch_lib(&std::fs::read_to_string(&lib).unwrap());
    std::fs::write(&lib, patched).unwrap();
    let manifest = root.join("memcastle-source.toml");
    let patched = patch_manifest(&share_target(
        &std::fs::read_to_string(&manifest).unwrap(),
        name,
    ));
    std::fs::write(&manifest, patched).unwrap();
    let project = Project::open(&root).unwrap();
    project.build().expect("the scaffolded project builds");
    Built { _dir: dir, project }
}

fn load(built: &Built) -> WasmAdapter {
    let bytes = std::fs::read(built.project.component_path()).unwrap();
    WasmAdapter::load(&built.project.manifest, &bytes, &MiningConfig::default()).unwrap()
}

fn source(adapter: &WasmAdapter, locator: &Path) -> SourceRef {
    adapter.identify(locator.to_str()).unwrap()
}

fn candidate(handle: &str) -> Candidate {
    Candidate {
        external_id: handle.to_string(),
        cursor_after: Cursor::Null,
        handle: handle.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_rust_source_builds_and_passes_the_conformance_cases_untouched() {
    let built = scaffold("rust-demo", Template::Rust, str::to_string, str::to_string);

    let report = built.project.test(&MiningConfig::default()).await.unwrap();

    assert!(report.passed(), "{report:?}");
    assert_eq!(report.cases[0].name, "text-files");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_new_cli_source_builds_and_passes_the_same_conformance_cases_by_running_granted_programs()
{
    let built = scaffold("cli-demo", Template::Cli, str::to_string, str::to_string);

    let report = built.project.test(&MiningConfig::default()).await.unwrap();

    assert!(report.passed(), "{report:?}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_source_running_a_program_its_manifest_does_not_list_is_denied_and_the_error_says_so() {
    // `uname` exists on every machine this runs on, so a refusal can only be the host's.
    let built = scaffold(
        "cli-denied",
        Template::Cli,
        |lib| lib.replace("output(\"cat\"", "output(\"uname\""),
        str::to_string,
    );
    let adapter = load(&built);
    let tree = tempfile::tempdir().unwrap();
    std::fs::write(tree.path().join("a.txt"), "alpha\n").unwrap();
    let source = source(&adapter, tree.path());

    let error = adapter
        .read(&source, &candidate("a.txt"))
        .await
        .unwrap_err();

    assert!(
        matches!(error, Error::SourcePermissionDenied { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("uname"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_can_read_only_the_directory_it_was_asked_to_mine_and_sees_no_environment() {
    let built = scaffold(
        "rust-escape",
        Template::Rust,
        |lib| {
            lib.replace(
                "let path: PathBuf = Path::new(&source.locator).join(&candidate.handle);",
                "let path: PathBuf = if candidate.handle.starts_with('/') { PathBuf::from(&candidate.handle) } else { Path::new(&source.locator).join(&candidate.handle) };",
            )
            .replace(
                "metadata: json!({ \"path\": path.display().to_string() }).to_string(),",
                "metadata: json!({ \"path\": path.display().to_string(), \"path_env\": std::env::var(\"PATH\").ok() }).to_string(),",
            )
        },
        str::to_string,
    );
    let adapter = load(&built);
    let mined = tempfile::tempdir().unwrap();
    std::fs::write(mined.path().join("a.txt"), "inside\n").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "outside\n").unwrap();
    let source = source(&adapter, mined.path());

    let inside = adapter.read(&source, &candidate("a.txt")).await.unwrap();
    let escaped = adapter
        .read(&source, &candidate(secret.to_str().unwrap()))
        .await
        .unwrap();

    let inside = inside.expect("the mined directory is readable");
    assert_eq!(inside.body, "inside\n");
    assert!(
        escaped.is_none(),
        "a path outside the grant must not be readable"
    );
    assert!(
        inside.metadata["path_env"].is_null(),
        "the daemon's environment must not reach a source that did not ask for it: {}",
        inside.metadata
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_never_returns_is_stopped_at_its_time_limit() {
    let built = scaffold(
        "rust-spin",
        Template::Rust,
        |lib| {
            lib.replace(
                "let after = parse_cursor(&cursor)?;",
                "let after = parse_cursor(&cursor)?;\n        loop {\n            std::hint::black_box(&after);\n        }",
            )
        },
        |manifest| format!("{manifest}\n[limits]\ntimeout_secs = 1\n"),
    );
    let adapter = load(&built);
    let tree = tempfile::tempdir().unwrap();
    let source = source(&adapter, tree.path());

    let started = std::time::Instant::now();
    let error = adapter
        .discover(&source, &Cursor::Null, 10)
        .await
        .unwrap_err();

    assert!(
        matches!(error, Error::SourceTimeout { secs: 1, .. }),
        "{error}"
    );
    assert!(
        started.elapsed().as_secs() < 10,
        "the limit is enforced, not the guest's good manners"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_exhausts_its_memory_limit_fails_instead_of_taking_the_daemon_down() {
    let built = scaffold(
        "rust-greedy",
        Template::Rust,
        |lib| {
            lib.replace(
                "let after = parse_cursor(&cursor)?;",
                "let after = parse_cursor(&cursor)?;\n        let big = vec![1u8; 512 * 1024 * 1024];\n        std::hint::black_box(&big);",
            )
        },
        |manifest| format!("{manifest}\n[limits]\nmemory_mib = 32\n"),
    );
    let adapter = load(&built);
    let tree = tempfile::tempdir().unwrap();
    let source = source(&adapter, tree.path());

    let error = adapter
        .discover(&source, &Cursor::Null, 10)
        .await
        .unwrap_err();

    assert!(matches!(error, Error::SourceFailed { .. }), "{error}");
}

fn memcastle_in(dir: &Path) -> Blocking {
    let mut command = Blocking::cargo_bin("memcastle").unwrap();
    command
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", shared_target());
    command
}

#[test]
fn the_cli_takes_a_source_from_init_to_a_package_without_a_daemon() {
    let work = tempfile::tempdir().unwrap();

    memcastle_in(work.path())
        .args(["source", "init", "notes", "--template", "rust"])
        .assert()
        .success()
        .stdout(contains("Created"))
        .stdout(contains("memcastle-source.toml"));
    share_target_of(work.path(), "notes");
    let project = work.path().join("notes");
    assert!(project.join("wit/memcastle-source.wit").is_file());

    memcastle_in(&project)
        .args(["source", "build"])
        .assert()
        .success()
        .stdout(contains("Built"));
    memcastle_in(&project)
        .args(["source", "test", "--no-build"])
        .assert()
        .success()
        .stdout(contains("PASS text-files"));
    memcastle_in(&project)
        .args(["source", "package", "--no-build"])
        .assert()
        .success()
        .stdout(contains("Packaged"))
        .stdout(contains("read files under locator"));
    assert!(project.join("dist/notes-0.1.0.tar.gz").is_file());
}

#[test]
fn building_outside_a_source_project_says_how_to_make_one() {
    let empty = tempfile::tempdir().unwrap();
    memcastle_in(empty.path())
        .args(["source", "build"])
        .assert()
        .failure()
        .stderr(contains("memcastle::source::manifest_invalid"))
        .stderr(contains("source init"));
}

#[test]
fn a_failing_conformance_case_fails_the_test_command_and_names_what_broke() {
    let work = tempfile::tempdir().unwrap();
    memcastle_in(work.path())
        .args(["source", "init", "broken"])
        .assert()
        .success();
    let project = work.path().join("broken");
    // The cases expect `alpha`; the source now disagrees about what the first file says.
    std::fs::write(project.join("fixtures/text-files/tree/a.txt"), "changed\n").unwrap();

    memcastle_in(&project)
        .args(["source", "test"])
        .assert()
        .failure()
        .stdout(contains("FAIL text-files"))
        .stdout(contains("a.txt"));
}

/// The `memcastle` binary pointed at `daemon`'s palace.
fn memcastle_for(daemon: &TestDaemon) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(cargo_bin("memcastle"));
    command
        .env("MEMCASTLE_PALACE_PATH", &daemon.palace_path)
        .env_remove("MEMCASTLE_MODE")
        .env_remove("MEMCASTLE_AUTH_ENABLED")
        .env_remove("MEMCASTLE_AUTH_TOKEN")
        .stdin(Stdio::null());
    command
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_installs_lists_disables_and_removes_a_source_and_never_consents_for_a_script() {
    let dir = tempfile::tempdir().unwrap();
    let sources_dir = dir.path().join("sources");
    let configured = sources_dir.clone();
    let daemon =
        TestDaemon::start_configured(move |config| config.mining.sources_dir = Some(configured))
            .await;

    let work = tempfile::tempdir().unwrap();
    memcastle_in(work.path())
        .args(["source", "init", "notes"])
        .assert()
        .success();
    let project = work.path().join("notes");
    memcastle_in(&project)
        .args(["source", "package"])
        .assert()
        .success();
    let package = project.join("dist/notes-0.1.0.tar.gz");

    // No terminal, no `--yes`: a script is never asked and never consents on its own behalf.
    let refused = memcastle_for(&daemon)
        .args(["source", "install"])
        .arg(&package)
        .output()
        .await
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("memcastle::source::consent_required"),
        "{stderr}"
    );
    assert!(!sources_dir.join("notes").exists(), "nothing was installed");

    let installed = memcastle_for(&daemon)
        .args(["source", "install", "--yes", "--enable"])
        .arg(&package)
        .output()
        .await
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let installed: Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(installed["source"]["state"], "enabled");

    let listed = memcastle_for(&daemon)
        .arg("sources")
        .output()
        .await
        .unwrap();
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(
        listed["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "notes")
    );

    let disabled = memcastle_for(&daemon)
        .args(["source", "disable", "notes"])
        .output()
        .await
        .unwrap();
    assert!(disabled.status.success());
    let shown = memcastle_for(&daemon)
        .args(["source", "show", "notes"])
        .output()
        .await
        .unwrap();
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["state"], "disabled");

    let removed = memcastle_for(&daemon)
        .args(["source", "remove", "--yes", "notes"])
        .output()
        .await
        .unwrap();
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!sources_dir.join("notes").exists());
    daemon.shutdown().await;
}
