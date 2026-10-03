//! A stopping process leaves the embedded datastore time to stop itself.
//!
//! SurrealDB shuts its datastore down in a detached task once the last handle is dropped.
//! If the tokio runtime is dropped first, that task is cancelled and logs
//! `Background task did not shut down cleanly`, and the storage engine may never flush.
//! The race is intermittent, so each test repeats the stop a few times.
//!
//! Subprocesses, because the runtime teardown being tested only happens at process exit.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;

/// The marker SurrealDB logs when the runtime cancelled its shutdown.
const MARKER: &str = "did not shut down cleanly";

/// How many times each scenario is repeated: the race does not fire on every exit.
const ROUNDS: usize = 5;

/// A `memcastle` command scoped to a sandbox rooted at `root`, with every ambient setting removed.
fn memcastle(root: &Path) -> Command {
    let mut cmd = Command::new(cargo_bin("memcastle"));
    for name in [
        "MEMCASTLE_CONFIG",
        "MEMCASTLE_BIND",
        "MEMCASTLE_PORT",
        "MEMCASTLE_MODE",
        "MEMCASTLE_LOG",
        "MEMCASTLE_AUTH_ENABLED",
        "MEMCASTLE_AUTH_TOKEN",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("MEMCASTLE_PALACE_PATH", root.join("palace"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .stdin(Stdio::null());
    cmd
}

/// Whether the daemon of the sandbox at `root` has written its registry entry, i.e. is serving.
fn registered(root: &Path) -> bool {
    std::fs::read_dir(root.join("state/memcastle/run"))
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.path().join("daemon.json").exists())
        })
        .unwrap_or(false)
}

#[test]
fn a_stopped_daemon_logs_no_datastore_shutdown_errors() {
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        let log = std::fs::File::create(&log_path).expect("create the daemon log");

        let mut daemon = memcastle(dir.path())
            .arg("serve")
            .env("MEMCASTLE_BIND", "127.0.0.1")
            .env("MEMCASTLE_PORT", "0")
            // Pinned to `info` so the check does not depend on the developer's own level.
            .env("MEMCASTLE_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn `memcastle serve`");

        let deadline = Instant::now() + Duration::from_secs(60);
        while !registered(dir.path()) {
            if let Ok(Some(status)) = daemon.try_wait() {
                panic!("daemon exited early with {status}");
            }
            if Instant::now() > deadline {
                let _ = daemon.kill();
                panic!("daemon did not register within 60s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let stop = memcastle(dir.path())
            .arg("stop")
            .output()
            .expect("run `stop`");
        assert!(
            stop.status.success(),
            "`stop` failed: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
        let status = daemon.wait().expect("wait for the daemon");
        assert!(status.success(), "daemon exited with {status}");

        let log = std::fs::read_to_string(&log_path).expect("read the daemon log");
        assert!(!log.contains(MARKER), "round {round}, daemon log:\n{log}");
    }
}

#[test]
fn a_finished_migrate_logs_no_datastore_shutdown_errors() {
    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = memcastle(dir.path())
            .env("MEMCASTLE_LOG", "info")
            .arg("migrate")
            .output()
            .expect("run `migrate`");
        assert!(output.status.success(), "migrate failed in round {round}");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains(MARKER), "round {round}, stderr:\n{stderr}");
    }
}
