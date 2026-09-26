//! Shared helpers for integration tests: spin up a real daemon (embedded
//! store, ephemeral port, tempdir palace) as a real Tokio task, and hand
//! back its base URL once it's actually listening.
//!
//! Used unevenly across integration test binaries (each `tests/*.rs` file
//! compiles this module separately), so unused-item warnings here are
//! expected and silenced rather than a signal of dead code.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use memcastle::config::Config;
use memcastle::server::lifecycle::RuntimeInfo;
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// A running-in-process daemon plus the guard that keeps its tempdir alive.
pub struct TestDaemon {
    pub base_url: String,
    pub palace_path: PathBuf,
    handle: JoinHandle<memcastle::Result<()>>,
    _tempdir: TempDir,
}

impl TestDaemon {
    /// Start a daemon on an OS-assigned port, in a fresh tempdir palace, and
    /// wait until it's actually accepting connections.
    pub async fn start() -> Self {
        Self::start_with(1).await
    }

    /// Like [`Self::start`], but with a configurable job concurrency —
    /// tests exercising several jobs at once need more than the default.
    pub async fn start_with(max_concurrency: usize) -> Self {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let palace_path = tempdir.path().join("palace");

        let mut config = Config::default();
        config.palace.path = palace_path.clone();
        config.jobs.max_concurrency = max_concurrency;
        config.server.bind = "127.0.0.1:0".parse().expect("valid socket addr");

        let handle = tokio::spawn(memcastle::server::run(config));

        let info = wait_for_registry(&palace_path).await;
        Self {
            base_url: format!("http://{}", info.bind_addr),
            palace_path,
            handle,
            _tempdir: tempdir,
        }
    }

    /// Ask the daemon to shut down and wait for its task to finish,
    /// propagating anything it returned.
    pub async fn shutdown(self) {
        let client = reqwest::Client::new();
        let _ = client
            .post(format!("{}/api/shutdown", self.base_url))
            .send()
            .await;
        tokio::time::timeout(Duration::from_secs(15), self.handle)
            .await
            .expect("daemon did not shut down within 15s")
            .expect("daemon task panicked")
            .expect("daemon returned an error");
    }
}

/// Poll the registry file for `palace_path` until a live daemon has written
/// one — used directly by tests that spawn `memcastle serve` as a real
/// subprocess, not through [`TestDaemon`].
pub async fn wait_for_registry(palace_path: &Path) -> RuntimeInfo {
    // Generous relative to the ~200ms an idle daemon actually takes to bind:
    // a busy CI runner (or, locally, `mise run ci` compiling docs/lint/test
    // targets concurrently) can starve this process for several seconds
    // without it being a real failure to detect. `TestDaemon` never comes
    // close to this ceiling either way — it starts the daemon as an
    // in-process Tokio task, not a real OS process, so it has no process-
    // spawn overhead to absorb. The one caller that does — `persistence.rs`,
    // which spawns genuine `memcastle serve` subprocesses — is also the one
    // that has actually needed the slack: Windows CI under `cargo llvm-cov`
    // instrumentation, spawning a real process with SurrealDB 3.x's heavier
    // embedded-engine startup (group-commit setup, a datastore-version
    // check), has been observed taking noticeably longer than 15s even for
    // a first, uncontended start.
    for _ in 0..1200 {
        if let Some(info) = memcastle::server::lifecycle::read_if_live(palace_path) {
            return info;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not start within 60s");
}
