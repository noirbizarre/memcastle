//! The web UI that ships with MemCastle (docs/adr/035), built by the script the release runs and served from
//! the tree it writes and from the checkout it builds in: the same `web/dist/` found through both ways of choosing the
//! assets root.
//!
//! This is what keeps "the dashboard ships with every release, packaged and in development alike" from being a claim
//! about the release workflow that only a tag would test. It needs bun and node, so it is `#[ignore]`d from the basic
//! suite and run by `mise run web:check` (`cargo test --test web_bundle -- --ignored`); an ignored test says so in the
//! output, which a silent skip would not. What the daemon does with the files (headers, authentication, the page for a
//! missing build) is tested without bun in `tests/in_process/web.rs` and `src/assets`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Once;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The package tree, built once for every test in this binary: the script rebuilds `web/dist/` in the checkout, so two
/// builds at once would race.
fn package() -> PathBuf {
    static BUILD: Once = Once::new();
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("bundled-web");
    BUILD.call_once(|| {
        let output = Command::new("bash")
            .arg(root().join("packaging/web/build.sh"))
            .arg(&out)
            .output()
            .expect("bash starts");
        assert!(
            output.status.success(),
            "packaging/web/build.sh failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    });
    out
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A daemon that is killed when the guard drops, so a failing assertion never leaks a process holding a palace lock.
struct Daemon {
    child: Child,
    base_url: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `binary serve` with the dashboard enabled and every XDG directory under `home`, with `args` after it. No
/// `MEMCASTLE_*` variable of the developer's reaches it.
async fn serve(binary: &Path, home: &Path, args: &[&str]) -> Daemon {
    let mut command = Command::new(binary);
    for (name, _) in std::env::vars() {
        if name.starts_with("MEMCASTLE_") {
            command.env_remove(name);
        }
    }
    let child = command
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("MEMCASTLE_STORE_SYNC", "never")
        .env("MEMCASTLE_WEB_ENABLE", "true")
        .args(["serve", "--palace"])
        .arg(home.join("palace"))
        .args(["--port", "0"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `memcastle serve`");
    let mut daemon = Daemon {
        child,
        base_url: String::new(),
    };

    for _ in 0..1200 {
        let registry = std::fs::read_dir(home.join("state/memcastle/run"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("daemon.json"))
            .find(|path| path.is_file());
        if let Some(path) = registry
            && let Ok(info) =
                serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap())
        {
            daemon.base_url = format!("http://{}", info["bind_addr"].as_str().unwrap());
            return daemon;
        }
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!("`memcastle serve` exited early: {status}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the daemon did not start within 60s");
}

async fn get(daemon: &Daemon, path: &str) -> (reqwest::StatusCode, String) {
    let response = reqwest::get(format!("{}{path}", daemon.base_url))
        .await
        .expect("request");
    (response.status(), response.text().await.unwrap())
}

#[test]
#[ignore = "needs bun and node; `mise run web:check` runs it"]
fn the_package_holds_the_built_dashboard_under_web_dist_and_nothing_else() {
    let package = package();

    let mut top: Vec<_> = std::fs::read_dir(&package)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, ["web"]);
    let dist = package.join("web/dist");
    assert!(dist.join("index.html").is_file());
    assert!(dist.join("assets").is_dir());

    // The sources the build replaced, their dependencies and source maps (which would let the daemon serve the
    // sources) must not travel with it.
    for file in files_under(&package) {
        let name = file.file_name().unwrap().to_string_lossy();
        assert!(
            !name.ends_with(".ts")
                && !name.ends_with(".vue")
                && !name.ends_with(".map")
                && name != "bun.lock"
                && name != "package.json"
                && !file.components().any(|c| c.as_os_str() == "node_modules"),
            "{} must not ship",
            file.display()
        );
    }

    // The page loads what the build wrote, from the prefix it is served under.
    let index = std::fs::read_to_string(dist.join("index.html")).unwrap();
    assert!(index.contains("/ui/assets/"), "{index}");
}

#[tokio::test]
#[ignore = "needs bun and node; `mise run web:check` runs it"]
async fn a_daemon_run_from_an_installed_prefix_serves_the_packaged_dashboard_with_no_flag() {
    let package = package();
    let home = tempfile::tempdir().unwrap();
    // `<prefix>/bin/memcastle` beside `<prefix>/share/memcastle/web`: what a package or an unpacked tarball installs.
    let prefix = home.path().join("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::create_dir_all(prefix.join("share/memcastle")).unwrap();
    std::fs::copy(cargo_bin("memcastle"), prefix.join("bin/memcastle")).unwrap();
    assert!(
        Command::new("cp")
            .arg("-r")
            .arg(package.join("web"))
            .arg(prefix.join("share/memcastle/"))
            .status()
            .unwrap()
            .success()
    );

    let daemon = serve(&prefix.join("bin/memcastle"), home.path(), &[]).await;

    let (status, body) = get(&daemon, "/ui/").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        std::fs::read_to_string(package.join("web/dist/index.html")).unwrap()
    );
    let (status, config) = get(&daemon, "/api/config").await;
    assert_eq!(status, 200);
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(config["assets"]["source"], "installed");
    assert_eq!(config["web"]["built"], true);
}

#[tokio::test]
#[ignore = "needs bun and node; `mise run web:check` runs it"]
async fn a_daemon_pointed_at_the_checkout_serves_the_same_dashboard_through_the_same_lookup() {
    // Building the package built `web/dist/` in the checkout, which is all development mode needs.
    let package = package();
    let home = tempfile::tempdir().unwrap();
    let checkout = root();

    let daemon = serve(
        &cargo_bin("memcastle"),
        home.path(),
        &["--assets-dir", checkout.to_str().unwrap()],
    )
    .await;

    let (status, body) = get(&daemon, "/ui/").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        std::fs::read_to_string(package.join("web/dist/index.html")).unwrap()
    );
    let config: serde_json::Value =
        serde_json::from_str(&get(&daemon, "/api/config").await.1).unwrap();
    assert_eq!(config["assets"]["source"], "override");
    assert_eq!(config["web"]["built"], true);
}
