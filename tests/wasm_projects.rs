//! Source projects end to end: `memcastle source init`, `build`, `test` and `package`, the sandbox a component runs
//! in, and installing the result through the CLI (docs/adr/026, docs/writing-sources.md).
//!
//! Every project here is built for real with `cargo build --target wasm32-wasip2`, into one shared target directory
//! and from one shared lockfile (`common::wasm`), so the dependencies compile once and nothing resolves against the
//! network. A test that drives the CLI must call `share_target_of` after `source init`: forgetting it builds in release
//! into a `target/` of its own and recompiles every dependency, which was two of the slowest tests here. Where a test needs a misbehaving source it scaffolds a project and patches the
//! source: the sandbox is only proven by running code that tries to leave it.

mod common;

use std::path::Path;
use std::process::Stdio;

use assert_cmd::Command as Blocking;
use assert_cmd::cargo::cargo_bin;
use common::TestDaemon;
use common::wasm::{seed_lockfile, share_target, share_target_of, shared_target};
use memcastle::config::MiningConfig;
use memcastle::domain::{Candidate, Cursor, RawDocument, SourceKind, SourceRef};
use memcastle::error::Error;
use memcastle::mining::adapter::SourceAdapter;
use memcastle::mining::wasm::WasmAdapter;
use memcastle::source::build::Project;
use memcastle::source::scaffold::{Template, init};
use predicates::str::contains;
use serde_json::Value;

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
    seed_lockfile(&root);
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

/// Build the fixture source that answers according to what its files are called.
fn misbehaving() -> Built {
    scaffold(
        "misbehaving",
        Template::Rust,
        |_| include_str!("fixtures/sources/misbehaving/lib.rs").to_string(),
        str::to_string,
    )
}

fn files(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        std::fs::write(dir.path().join(name), "x").unwrap();
    }
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn every_way_a_component_can_answer_becomes_the_matching_error_or_value() {
    let built = misbehaving();
    let adapter = load(&built);
    let tree = files(&[
        "dated",
        "badmeta",
        "baddate",
        "invalid",
        "failed",
        "cursor",
        "trap",
        "skip",
        "manual",
        "transcript",
        "other",
    ]);
    let source = source(&adapter, tree.path());

    // What the guest lists is what the host sees, in order.
    let found = adapter.discover(&source, &Cursor::Null, 100).await.unwrap();
    assert_eq!(found.candidates.len(), 11);
    assert!(found.exhausted);
    assert_eq!(
        found.candidates[0].cursor_after["after"], "baddate",
        "in name order"
    );

    let read = |handle: &'static str| {
        let (adapter, source) = (&adapter, &source);
        async move { adapter.read(source, &candidate(handle)).await }
    };

    let dated = read("dated").await.unwrap().expect("a document");
    assert_eq!(dated.metadata["k"], 1);
    assert_eq!(
        dated.occurred_at.unwrap().to_rfc3339(),
        "2026-01-02T03:04:05+00:00"
    );
    assert!(
        read("skip").await.unwrap().is_none(),
        "none from the guest is a skip"
    );

    let error = read("badmeta").await.unwrap_err().to_string();
    assert!(error.contains("metadata that is not JSON"), "{error}");
    let error = read("baddate").await.unwrap_err().to_string();
    assert!(error.contains("not RFC 3339"), "{error}");
    assert!(matches!(
        read("invalid").await,
        Err(Error::InvalidInput { .. })
    ));
    assert!(
        matches!(read("cursor").await, Err(Error::SourceCursorInvalid { ref provider, .. }) if provider == "misbehaving")
    );
    let failed = read("failed").await.unwrap_err();
    assert!(matches!(failed, Error::SourceFailed { .. }), "{failed}");
    assert!(failed.to_string().contains("boom"), "{failed}");
    assert!(
        matches!(read("trap").await, Err(Error::SourceFailed { .. })),
        "a guest that panics is a failed source, not a dead daemon"
    );
    // ... and the adapter is still usable afterwards: each call has a store of its own.
    assert!(read("dated").await.unwrap().is_some());

    for (name, expected) in [
        ("manual", SourceKind::Manual),
        ("transcript", SourceKind::Transcript),
        ("other", SourceKind::Other),
        ("plain", SourceKind::File),
    ] {
        let raw = RawDocument {
            external_id: name.to_string(),
            revision: "r".to_string(),
            body: "b".to_string(),
            metadata: serde_json::json!({}),
            occurred_at: None,
        };
        let canonical = adapter.normalize(&raw).unwrap();
        assert_eq!(canonical.kind, expected, "{name}");
        assert_eq!(canonical.room.as_deref(), Some("room"));
        assert_eq!(canonical.tags, ["tag"]);
    }

    assert_eq!(adapter.default_room(), "misc");
    assert_eq!(adapter.default_wing(&source), source.locator);
    assert!(format!("{adapter:?}").contains("misbehaving"));
    assert!(
        adapter.identify(None).is_err(),
        "a missing locator is the guest's invalid-input"
    );

    // Called from a thread with no async runtime at all, the same synchronous methods still work.
    let outside = std::thread::scope(|scope| {
        scope
            .spawn(|| adapter.identify(tree.path().to_str()))
            .join()
            .unwrap()
    });
    assert!(outside.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cursor_after_that_is_not_json_is_a_failure_of_the_source_not_a_crash() {
    let built = misbehaving();
    let adapter = load(&built);
    let parent = tempfile::tempdir().unwrap();
    let tree = parent.path().join("badcursor");
    std::fs::create_dir(&tree).unwrap();
    let source = source(&adapter, &tree);

    let error = adapter
        .discover(&source, &Cursor::Null, 10)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("cursor-after that is not JSON"), "{error}");
}

fn manifest_text(contract: &str) -> String {
    format!(
        "[source]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"d\"\n\n[compatibility]\ncontract = \"{contract}\"\nmemcastle = \">=0.1\"\n"
    )
}

#[test]
fn bytes_that_are_not_a_component_or_do_not_fit_the_contract_are_incompatible_not_a_panic() {
    let manifest = memcastle::source::manifest::parse(&manifest_text("0.1"), &[]).unwrap();
    let config = MiningConfig::default();

    // The component header, then nonsense.
    let garbage = WasmAdapter::load(&manifest, b"\0asm\x0d\0\x01\0nonsense", &config).unwrap_err();
    assert!(
        matches!(garbage, Error::SourceIncompatible { .. }),
        "{garbage}"
    );
    assert!(
        garbage
            .to_string()
            .contains("not a valid WebAssembly component"),
        "{garbage}"
    );

    // A valid, empty component: well formed, and exports nothing of the contract.
    let empty = WasmAdapter::load(&manifest, b"\0asm\x0d\0\x01\0", &config).unwrap_err();
    assert!(matches!(empty, Error::SourceIncompatible { .. }), "{empty}");
    assert!(empty.to_string().contains("memcastle:source"), "{empty}");

    // A manifest for another contract is refused before any byte is compiled.
    let other = memcastle::source::manifest::parse(&manifest_text("0.9"), &[]).unwrap();
    let error = WasmAdapter::load(&other, b"", &config).unwrap_err();
    assert!(matches!(error, Error::SourceIncompatible { .. }), "{error}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_crashes_after_being_refused_a_program_is_reported_as_the_refusal() {
    // The guest unwraps the host's answer, so the refusal reaches the host as a trap: the cause the user needs is the
    // permission, not "unreachable executed".
    let built = scaffold(
        "cli-crash",
        Template::Cli,
        |lib| {
            lib.replace("output(\"cat\"", "output(\"uname\"").replace(
                "run_process(program, args, None).map_err(SourceError::Failed)?",
                "run_process(program, args, None).expect(\"refused\")",
            )
        },
        str::to_string,
    );
    let adapter = load(&built);
    let tree = files(&["a.txt"]);
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
    share_target_of(work.path(), "broken");
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
    share_target_of(work.path(), "notes");
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

#[test]
fn init_can_create_the_project_somewhere_other_than_the_working_directory() {
    let here = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    memcastle_in(here.path())
        .args([
            "source",
            "init",
            "placed",
            "--template",
            "python",
            "--parent",
        ])
        .arg(elsewhere.path())
        .assert()
        .success()
        .stdout(contains("python template"));

    assert!(elsewhere.path().join("placed/app.py").is_file());
    assert!(!here.path().join("placed").exists());
}

fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_installs_a_package_that_asks_for_nothing_unasked_and_one_that_asks_with_a_reviewed_digest()
 {
    let dir = tempfile::tempdir().unwrap();
    let sources_dir = dir.path().join("sources");
    let configured = sources_dir.clone();
    let daemon =
        TestDaemon::start_configured(move |config| config.mining.sources_dir = Some(configured))
            .await;

    // One built component, packaged twice: as written (it reads the locator) and with its permission removed.
    let built = scaffold("lifecycle", Template::Rust, str::to_string, str::to_string);
    let out = tempfile::tempdir().unwrap();
    let (asking, package) = built
        .project
        .package(Some(&out.path().join("asking.tar.gz")))
        .unwrap();
    let quiet_manifest = package
        .manifest_text
        .replace("[permissions.filesystem]\nread = [\"locator\"]\n", "");
    assert_ne!(
        quiet_manifest, package.manifest_text,
        "the permission was there to remove"
    );
    let quiet = out.path().join("quiet.tar.gz");
    std::fs::write(
        &quiet,
        memcastle::source::package::pack(&quiet_manifest, &package.component, &[]).unwrap(),
    )
    .unwrap();

    // Nothing to consent to: installed without `--yes`, and not enabled.
    let installed = memcastle_for(&daemon)
        .args(["source", "install"])
        .arg(&quiet)
        .output()
        .await
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert_eq!(json(&installed)["source"]["state"], "installed");

    // Asking for the locator now: refused without consent, accepted with the digest of exactly those permissions.
    let digest = package
        .manifest
        .permissions
        .normalized()
        .consent_digest("lifecycle");
    let refused = memcastle_for(&daemon)
        .args(["source", "install", "--consent", "0000"])
        .arg(&asking)
        .output()
        .await
        .unwrap();
    assert!(
        !refused.status.success(),
        "a digest of something else is not consent"
    );
    let accepted = memcastle_for(&daemon)
        .args(["source", "install", "--consent", &digest])
        .arg(&asking)
        .output()
        .await
        .unwrap();
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let accepted = json(&accepted);
    assert_eq!(accepted["replaced"], true);
    assert_eq!(
        accepted["source"]["permissions"]["filesystem"]["read"],
        serde_json::json!(["locator"])
    );

    let enabled = memcastle_for(&daemon)
        .args(["source", "enable", "lifecycle"])
        .output()
        .await
        .unwrap();
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert_eq!(json(&enabled)["state"], "enabled");

    let shown = memcastle_for(&daemon)
        .args(["source", "show", "lifecycle"])
        .output()
        .await
        .unwrap();
    assert_eq!(json(&shown)["version"], "0.1.0");
    let listed = memcastle_for(&daemon)
        .args(["source", "list"])
        .output()
        .await
        .unwrap();
    let listed = json(&listed);
    let names: Vec<_> = listed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["directory", "lifecycle"]);

    let missing = memcastle_for(&daemon)
        .args(["source", "show", "nope"])
        .output()
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&missing.stderr).contains("memcastle::source::not_found"));
    daemon.shutdown().await;
}
