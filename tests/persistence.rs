//! Proves data survives a daemon restart — at the process boundary, on
//! purpose: SurrealDB's embedded RocksDB engine does not release its file
//! lock when a `Surreal` handle merely drops within the same process (see
//! `store::tests`'s comment), so this spawns two genuinely separate
//! `memcastle serve` processes against the same palace directory, which is
//! also the more representative test of the actual claim ("stop and restart
//! the daemon; your data is still there").

mod common;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use memcastle::server::lifecycle::{RuntimeInfo, read_if_live};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

#[tokio::test]
async fn drawers_survive_a_daemon_restart_against_the_same_palace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = dir.path().join("palace");
    let fixture = dir.path().join("fixture");
    std::fs::create_dir_all(&fixture).expect("create fixture dir");
    std::fs::write(
        fixture.join("note.txt"),
        "a note written before the restart",
    )
    .expect("write fixture file");

    let bin = cargo_bin("memcastle");
    let client = reqwest::Client::new();

    // Round 1: a fresh daemon mines the fixture, then is asked to stop.
    {
        let mut child = spawn_daemon(&bin, &palace);
        let info = common::wait_for_registry(&palace).await;
        let base = format!("http://{}", info.bind_addr);

        client
            .post(format!("{base}/api/jobs"))
            .json(&serde_json::json!({
                "type": "mine",
                "path": fixture,
                "wing": null,
                "requested_by": "test",
            }))
            .send()
            .await
            .expect("submit mine job")
            .error_for_status()
            .expect("mine job accepted");

        wait_for_all_jobs_completed(&client, &base).await;
        stop_daemon(&bin, &palace, &mut child).await;
    }

    // Round 2: a brand-new process, same palace directory — the drawer the
    // first process mined must still be there and still searchable.
    {
        let (mut child, info) = spawn_daemon_for_restart(&bin, &palace).await;
        let base = format!("http://{}", info.bind_addr);

        let hits: Vec<serde_json::Value> = client
            .get(format!("{base}/api/search"))
            .query(&[("q", "note written before"), ("limit", "10")])
            .send()
            .await
            .expect("search request")
            .json()
            .await
            .expect("search response is json");
        assert!(
            !hits.is_empty(),
            "expected the drawer mined before the restart to still be searchable"
        );

        stop_daemon(&bin, &palace, &mut child).await;
    }
}

fn spawn_daemon(bin: &Path, palace: &Path) -> Child {
    Command::new(bin)
        .arg("serve")
        .env("MEMCASTLE_PALACE_PATH", palace)
        .env("MEMCASTLE_BIND", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // If the test panics before explicitly stopping the daemon, don't
        // leak a process holding the palace's RocksDB lock forever.
        .kill_on_drop(true)
        .spawn()
        .expect("spawn `memcastle serve`")
}

/// Like [`spawn_daemon`], but for reopening the very palace directory a
/// previous process just released.
///
/// SurrealDB 3.x's embedded RocksDB engine does noticeably more at open
/// than before (group-commit setup, a datastore-version check); a real
/// `memcastle serve` subprocess (as opposed to `TestDaemon`'s in-process
/// task) paying for that under Windows CI's `cargo llvm-cov` instrumentation
/// has been observed needing well over the general 60s startup allowance —
/// see `common::wait_for_registry` — on top of which reopening the exact
/// path a previous process just released can transiently fail outright if
/// the OS is still settling that file handle. A bounded retry absorbs both
/// without weakening what this test actually proves (data really does
/// survive a restart); stderr is captured so a *real* failure still fails
/// loudly with the daemon's own diagnostic instead of an uninformative
/// timeout.
async fn spawn_daemon_for_restart(bin: &Path, palace: &Path) -> (Child, RuntimeInfo) {
    let mut last_error = "the daemon never exited or registered".to_string();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut child = Command::new(bin)
            .arg("serve")
            .env("MEMCASTLE_PALACE_PATH", palace)
            .env("MEMCASTLE_BIND", "127.0.0.1:0")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `memcastle serve`");

        let mut exited = false;
        for _ in 0..1200 {
            if let Some(info) = read_if_live(palace) {
                return (child, info);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut stderr = String::new();
                    if let Some(mut out) = child.stderr.take() {
                        let _ = out.read_to_string(&mut stderr).await;
                    }
                    last_error = format!("exited with {status}: {}", stderr.trim());
                    exited = true;
                    break;
                }
                Ok(None) => {}
                Err(error) => last_error = format!("could not poll the process: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !exited {
            last_error = "did not register within 60s (the process was still running)".to_string();
            // Dropping `child` here kills it (`kill_on_drop`) before the
            // next attempt spawns another one on the same path — otherwise
            // a merely-slow-not-dead daemon from this attempt would still
            // be holding the lock the next attempt needs.
        }
    }
    panic!(
        "daemon did not start after 3 attempts reopening the same palace; last failure: {last_error}"
    );
}

async fn wait_for_all_jobs_completed(client: &reqwest::Client, base: &str) {
    for _ in 0..100 {
        let jobs: Vec<serde_json::Value> = client
            .get(format!("{base}/api/jobs"))
            .send()
            .await
            .expect("list jobs")
            .json()
            .await
            .expect("jobs response is json");
        if !jobs.is_empty() && jobs.iter().all(|job| job["status"] == "completed") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("mining job did not complete in time");
}

async fn stop_daemon(bin: &Path, palace: &Path, child: &mut Child) {
    let status = Command::new(bin)
        .arg("stop")
        .env("MEMCASTLE_PALACE_PATH", palace)
        .status()
        .await
        .expect("run `memcastle stop`");
    assert!(status.success(), "`memcastle stop` should succeed");

    tokio::time::timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("daemon process exits within 15s of being asked to stop")
        .expect("daemon process can be waited on");
}
