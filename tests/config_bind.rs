//! Where the daemon listens: the bind address and port, resolved through the
//! same file -> environment -> flag layers as every other setting.
//!
//! Every daemon here is a real `memcastle serve` subprocess (the one startup
//! path systemd and `restart` use) with `XDG_*` pointed at a tempdir and
//! `MEMCASTLE_*` cleared, so a developer's own config, palace or daemon on the
//! default port can neither leak in nor be disturbed. Ports are asked of the
//! OS, never hard-coded, so the tests can run in parallel.

use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use memcastle::server::lifecycle::RuntimeInfo;

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
        .env("XDG_STATE_HOME", root.join("state"));
    cmd
}

/// A port that was free a moment ago.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// The registry entry of the daemon started under `root`, if it has written
/// one. `XDG_STATE_HOME` is private to `root`, so the run directory holds this
/// daemon's registry file and nothing else: it is found by scanning rather
/// than by `read_if_live`, which would look in the test process's own state
/// directory.
fn registered(root: &Path) -> Option<RuntimeInfo> {
    let run = root.join("state/memcastle/run");
    for entry in std::fs::read_dir(run).ok()?.flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path().join("daemon.json"))
            && let Ok(info) = serde_json::from_str(&text)
        {
            return Some(info);
        }
    }
    None
}

/// A daemon that is killed when the guard drops, so a failing assertion never
/// leaks a process holding a palace lock and a port.
struct Daemon {
    child: Child,
    info: RuntimeInfo,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `memcastle serve` with `configure` applied to the command, and wait
/// until it has registered, i.e. until its listener is really serving.
async fn start(root: &Path, palace: &Path, configure: impl FnOnce(&mut Command)) -> Daemon {
    let mut cmd = isolated(root);
    cmd.arg("--palace").arg(palace).arg("serve");
    configure(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `memcastle serve`");

    // Generous: see `common::wait_for_registry` on slow, instrumented CI.
    for _ in 0..1200 {
        if let Some(info) = registered(root) {
            return Daemon { child, info };
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("daemon exited with {status} before registering");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = child.kill();
    panic!("daemon did not register within 60s");
}

/// Run `memcastle` with `args` and return its stderr, asserting it failed.
fn failing_stderr(root: &Path, configure: impl FnOnce(&mut Command)) -> String {
    let mut cmd = isolated(root);
    configure(&mut cmd);
    let output = cmd.output().expect("run memcastle");
    assert!(!output.status.success(), "expected a failure");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_config(root: &Path, body: &str) -> PathBuf {
    let path = root.join("config.toml");
    std::fs::write(&path, body).expect("write the config file");
    path
}

#[tokio::test]
async fn serve_listens_on_the_bind_and_port_it_was_given() {
    let root = tempfile::tempdir().expect("tempdir");
    let port = free_port();

    let daemon = start(root.path(), &root.path().join("palace"), |cmd| {
        cmd.args(["--bind", "127.0.0.1", "--port", &port.to_string()]);
    })
    .await;

    assert_eq!(daemon.info.bind_addr, format!("127.0.0.1:{port}"));
    let health = reqwest::get(format!("http://127.0.0.1:{port}/api/health"))
        .await
        .expect("health request");
    assert!(health.status().is_success());
}

#[tokio::test]
async fn a_port_alone_keeps_the_daemon_on_loopback() {
    let root = tempfile::tempdir().expect("tempdir");

    // No bind anywhere: the default must be local-only, never a wildcard.
    let daemon = start(root.path(), &root.path().join("palace"), |cmd| {
        cmd.env("MEMCASTLE_PORT", "0");
    })
    .await;

    let addr: std::net::SocketAddr = daemon.info.bind_addr.parse().expect("a socket address");
    assert!(addr.ip().is_loopback(), "{addr} is not loopback");
    assert!(!addr.ip().is_unspecified());
}

#[tokio::test]
async fn the_port_comes_from_the_file_then_the_environment_then_the_flag() {
    let root = tempfile::tempdir().expect("tempdir");
    let (from_file, from_env, from_flag) = (free_port(), free_port(), free_port());
    let config = write_config(
        root.path(),
        &format!("[server]\nbind = \"127.0.0.1\"\nport = {from_file}\n"),
    );

    // One private root (hence palace and registry) per case, so the runs are
    // independent of how quickly a killed daemon releases its lock.
    let (r1, r2, r3) = (
        root.path().join("r1"),
        root.path().join("r2"),
        root.path().join("r3"),
    );
    let file_only = start(&r1, &r1.join("palace"), |cmd| {
        cmd.arg("--config").arg(&config);
    })
    .await;
    assert_eq!(file_only.info.bind_addr, format!("127.0.0.1:{from_file}"));
    drop(file_only);

    let env_over_file = start(&r2, &r2.join("palace"), |cmd| {
        cmd.arg("--config")
            .arg(&config)
            .env("MEMCASTLE_PORT", from_env.to_string());
    })
    .await;
    assert_eq!(
        env_over_file.info.bind_addr,
        format!("127.0.0.1:{from_env}"),
        "the environment must outrank the file"
    );
    drop(env_over_file);

    let flag_over_env = start(&r3, &r3.join("palace"), |cmd| {
        cmd.arg("--config")
            .arg(&config)
            .env("MEMCASTLE_PORT", from_env.to_string())
            .args(["--port", &from_flag.to_string()]);
    })
    .await;
    assert_eq!(
        flag_over_env.info.bind_addr,
        format!("127.0.0.1:{from_flag}"),
        "the flag must outrank the environment"
    );
}

#[tokio::test]
async fn the_bind_address_comes_from_the_environment_when_no_flag_is_given() {
    let root = tempfile::tempdir().expect("tempdir");

    // The wildcard, not another loopback address such as 127.0.0.2: macOS
    // configures only 127.0.0.1 on its loopback interface, so binding
    // anything else there fails. The wildcard binds everywhere and is not
    // the default, so seeing it proves the variable was applied.
    let daemon = start(root.path(), &root.path().join("palace"), |cmd| {
        cmd.env("MEMCASTLE_BIND", "0.0.0.0")
            .env("MEMCASTLE_PORT", "0");
    })
    .await;

    let addr: std::net::SocketAddr = daemon.info.bind_addr.parse().expect("a socket address");
    assert_eq!(addr.ip(), "0.0.0.0".parse::<IpAddr>().unwrap());
}

#[test]
fn a_port_that_is_already_taken_fails_with_a_diagnostic_naming_the_address() {
    let root = tempfile::tempdir().expect("tempdir");
    let taken = TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = taken.local_addr().expect("local addr").port();
    let palace = root.path().join("palace");

    let stderr = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace").arg(&palace).args([
            "serve",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ]);
    });

    assert!(
        stderr.contains("memcastle::server::bind_failed"),
        "{stderr}"
    );
    assert!(stderr.contains(&format!("127.0.0.1:{port}")), "{stderr}");
    assert!(
        stderr.contains("--port"),
        "the help must name the fix: {stderr}"
    );
    // Bound before anything else happens, so a failed start changes nothing.
    assert!(
        !palace.exists(),
        "a failed bind must not have created the palace"
    );
    assert!(
        registered(root.path()).is_none(),
        "a failed bind must not have registered a daemon"
    );
}

#[test]
fn an_address_that_is_not_on_this_machine_fails_with_a_bind_diagnostic() {
    let root = tempfile::tempdir().expect("tempdir");

    // TEST-NET-1 (RFC 5737) is reserved for documentation: no host owns it.
    let stderr = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace").arg(root.path().join("palace")).args([
            "serve",
            "--bind",
            "192.0.2.1",
            "--port",
            "0",
        ]);
    });

    assert!(
        stderr.contains("memcastle::server::bind_failed"),
        "{stderr}"
    );
    assert!(stderr.contains("192.0.2.1"), "{stderr}");
}

#[test]
fn invalid_ports_and_addresses_are_refused_before_anything_starts() {
    let root = tempfile::tempdir().expect("tempdir");
    let palace = root.path().join("palace");

    let out_of_range = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace")
            .arg(&palace)
            .args(["serve", "--port", "70000"]);
    });
    assert!(out_of_range.contains("--port"), "{out_of_range}");

    let with_port = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace")
            .arg(&palace)
            .args(["serve", "--bind", "127.0.0.1:8420"]);
    });
    assert!(
        with_port.contains("--port"),
        "a legacy host:port must point at --port: {with_port}"
    );

    let bad_env_port = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace")
            .arg(&palace)
            .arg("serve")
            .env("MEMCASTLE_PORT", "abc");
    });
    assert!(
        bad_env_port.contains("memcastle::config::invalid")
            && bad_env_port.contains("MEMCASTLE_PORT"),
        "{bad_env_port}"
    );

    let bad_env_bind = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace")
            .arg(&palace)
            .arg("serve")
            .env("MEMCASTLE_BIND", "localhost");
    });
    assert!(
        bad_env_bind.contains("memcastle::config::invalid")
            && bad_env_bind.contains("MEMCASTLE_BIND"),
        "{bad_env_bind}"
    );

    let legacy_env_bind = failing_stderr(root.path(), |cmd| {
        cmd.arg("--palace")
            .arg(&palace)
            .arg("serve")
            .env("MEMCASTLE_BIND", "127.0.0.1:8420");
    });
    assert!(
        legacy_env_bind.contains("MEMCASTLE_PORT"),
        "a legacy host:port must point at MEMCASTLE_PORT: {legacy_env_bind}"
    );

    assert!(
        !palace.exists(),
        "a refused start must not create the palace"
    );
}
