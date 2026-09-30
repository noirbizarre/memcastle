//! Runtime assets: the override is honoured and checked, and a standalone
//! binary needs no assets at all to start.
//!
//! Every daemon is a real `memcastle serve` subprocess with `XDG_*` pointed at
//! a tempdir and `MEMCASTLE_*` cleared, so a developer's own config or palace
//! can neither leak in nor be disturbed. The test binary lives under
//! `target/`, where no `share/memcastle` exists, so "nothing installed" holds
//! here without any setup.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;

/// A `memcastle` command whose XDG directories all live under `root`.
fn isolated(root: &Path) -> Command {
    let mut cmd = Command::new(cargo_bin("memcastle"));
    for name in [
        "MEMCASTLE_PALACE_PATH",
        "MEMCASTLE_CONFIG",
        "MEMCASTLE_BIND",
        "MEMCASTLE_PORT",
        "MEMCASTLE_MODE",
        "MEMCASTLE_ASSETS_DIR",
        "MEMCASTLE_AUTH_ENABLED",
        "MEMCASTLE_AUTH_TOKEN",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        // Port 0 so parallel tests never fight over 8420.
        .env("MEMCASTLE_PORT", "0")
        .arg("--palace")
        .arg(root.join("palace"));
    cmd
}

/// A daemon that is killed when the guard drops, so a failing assertion never
/// leaks a process holding a palace lock.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Whether a daemon has written its registry file under `root`'s state
/// directory, i.e. whether its listener is really serving.
fn registered(root: &Path) -> bool {
    std::fs::read_dir(root.join("state/memcastle/run"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().join("daemon.json").is_file())
}

/// Start `memcastle serve` with `configure` applied, and wait for it to
/// register. Panics if it exits first, with what it said.
async fn start(root: &Path, configure: impl FnOnce(&mut Command)) -> Daemon {
    let mut cmd = isolated(root);
    cmd.arg("serve");
    configure(&mut cmd);
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `memcastle serve`");
    let mut daemon = Daemon(child);

    // Generous: see `common::wait_for_registry` on slow, instrumented CI.
    for _ in 0..1200 {
        if registered(root) {
            return daemon;
        }
        if let Ok(Some(status)) = daemon.0.try_wait() {
            panic!("daemon exited with {status} before registering");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not register within 60s");
}

/// Run `memcastle` with `configure` applied and return its stderr, asserting
/// it failed.
fn failing_stderr(root: &Path, configure: impl FnOnce(&mut Command)) -> String {
    let mut cmd = isolated(root);
    configure(&mut cmd);
    let output = cmd.output().expect("run memcastle");
    assert!(!output.status.success(), "expected a failure");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn a_standalone_binary_serves_with_no_assets_configured_or_installed() {
    let root = tempfile::tempdir().expect("tempdir");

    // Offline-safe by construction: the daemon is only ever asked for a
    // loopback listener, and nothing in startup has anywhere to download from.
    let _daemon = start(root.path(), |_| {}).await;
}

#[tokio::test]
async fn serve_starts_with_an_existing_assets_directory() {
    let root = tempfile::tempdir().expect("tempdir");
    let assets = root.path().join("web-dist");
    std::fs::create_dir(&assets).expect("create the assets directory");

    let _daemon = start(root.path(), |cmd| {
        cmd.arg("--assets-dir").arg(&assets);
    })
    .await;
}

#[test]
fn a_missing_assets_directory_flag_is_refused_with_the_way_to_fix_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("typo");

    let stderr = failing_stderr(root.path(), |cmd| {
        cmd.arg("serve").arg("--assets-dir").arg(&missing);
    });

    assert!(stderr.contains("memcastle::assets::not_found"), "{stderr}");
    assert!(stderr.contains("--assets-dir"), "{stderr}");
    assert!(
        !registered(root.path()),
        "a refused start must not register a daemon"
    );
}

#[test]
fn a_missing_assets_directory_in_the_environment_is_refused_too() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("typo");

    let stderr = failing_stderr(root.path(), |cmd| {
        cmd.arg("serve").env("MEMCASTLE_ASSETS_DIR", &missing);
    });

    assert!(stderr.contains("memcastle::assets::not_found"), "{stderr}");
}

#[test]
fn a_missing_assets_directory_in_the_config_file_is_refused_too() {
    let root = tempfile::tempdir().expect("tempdir");
    let missing = root.path().join("typo");
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        format!("[assets]\ndir = {:?}\n", missing.display().to_string()),
    )
    .expect("write the config file");

    let stderr = failing_stderr(root.path(), |cmd| {
        cmd.arg("--config").arg(&config).arg("serve");
    });

    assert!(stderr.contains("memcastle::assets::not_found"), "{stderr}");
}

#[test]
fn a_relative_assets_directory_is_refused_with_a_diagnostic_naming_the_fix() {
    let root = tempfile::tempdir().expect("tempdir");

    let stderr = failing_stderr(root.path(), |cmd| {
        cmd.arg("serve").args(["--assets-dir", "web/dist"]);
    });

    assert!(stderr.contains("memcastle::config::invalid"), "{stderr}");
    assert!(stderr.contains("absolute"), "{stderr}");
}
