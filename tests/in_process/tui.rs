//! Real daemon and terminal exercise: keyboard navigation must reach REST and SSE without leaving raw mode behind.
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::TestDaemon;

#[tokio::test]
async fn the_console_navigates_search_jobs_and_maintenance_in_a_real_terminal() {
    let daemon = TestDaemon::start().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("note.md"), "hello from the TUI test").unwrap();
    let palace = daemon.palace_path.clone();
    let mine = dir.path().display().to_string();

    // A PTY exercises crossterm's raw-mode lifecycle; a pipe would be refused.
    let output = tokio::task::spawn_blocking(move || {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let size = libc::winsize {
            ws_row: 35,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initialises both descriptors, then File owns and
        // closes each one exactly once.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &raw mut master_fd,
                    &raw mut slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    &raw const size,
                )
            },
            0
        );
        let mut master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        // The reader must be able to leave when the child exits even if
        // another handle briefly keeps the PTY slave open.
        assert_eq!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        let mut process = Command::new(env!("CARGO_BIN_EXE_memcastle"));
        process
            .args(["--palace", palace.to_str().unwrap(), "tui"])
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // The CLI must find the daemon under test, not mise's MEMCASTLE_* defaults.
        for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("MEMCASTLE_")) {
            process.env_remove(name);
        }
        let mut child = process.spawn().expect("start TUI in pseudo-terminal");
        let mut stdout = master.try_clone().unwrap();
        let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reading = stopped.clone();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut chunk = [0; 8192];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(size) => out.extend_from_slice(&chunk[..size]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if reading.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    // Closing the PTY slave is reported as EIO on Linux.
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                    Err(error) => panic!("reading terminal: {error}"),
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        });
        for (keys, delay) in [
            ("2", 200),
            ("/", 100),
            ("hello", 100),
            ("\r", 450),
            ("3", 250),
            ("4", 250),
            ("a", 450),
            ("1", 250),
        ] {
            master.write_all(keys.as_bytes()).unwrap();
            std::thread::sleep(Duration::from_millis(delay));
        }
        master.write_all(b"n").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        master.write_all(mine.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        master.write_all(b"\r").unwrap();
        std::thread::sleep(Duration::from_secs(2));
        master.write_all(b"\x03").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let status = child.wait().unwrap();
        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        drop(master);
        (status, reader.join().unwrap())
    })
    .await
    .unwrap();

    let jobs: Vec<memcastle::domain::Job> = reqwest::Client::new()
        .get(format!("{}/api/jobs?kind=mine&limit=5", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "the mining form must submit through the daemon"
    );
    let audits: Vec<memcastle::domain::Job> = reqwest::Client::new()
        .get(format!("{}/api/jobs?kind=audit&limit=5", daemon.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        audits.len(),
        1,
        "maintenance must submit an audit through the daemon"
    );
    daemon.shutdown().await;
    assert!(output.0.success(), "terminal command failed: {}", output.1);
    for expected in ["MEMCASTLE", "SEARCH TEST", "MINING JOBS", "ACTIVITY"] {
        assert!(
            output.1.contains(expected),
            "{expected} was not rendered: {}",
            output.1
        );
    }
    assert!(
        output.1.contains("\x1b["),
        "terminal should emit styled output"
    );
}
