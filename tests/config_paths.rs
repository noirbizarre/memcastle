//! Where the `memcastle` binary finds its config file and palace.
//!
//! Every invocation is a real subprocess with `XDG_*` pointed at a tempdir
//! and `MEMCASTLE_*` cleared, so a developer's own config or palace can
//! neither leak into these tests nor be touched by them.

use assert_cmd::Command;

/// A `memcastle` invocation whose XDG directories all live under `root`.
fn isolated(root: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("memcastle").expect("find the memcastle binary");
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

#[test]
fn the_default_palace_lives_under_xdg_data_home() {
    let root = tempfile::tempdir().expect("tempdir");
    isolated(root.path()).arg("migrate").assert().success();
    assert!(
        root.path().join("data/memcastle/default/db").is_dir(),
        "the embedded database must be created under $XDG_DATA_HOME/memcastle/default"
    );
}

#[test]
fn the_default_config_file_under_xdg_config_home_is_read() {
    let root = tempfile::tempdir().expect("tempdir");
    let palace = root.path().join("from-file");
    let dir = root.path().join("config/memcastle");
    std::fs::create_dir_all(&dir).expect("create the config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!("[palace]\npath = {:?}\n", palace.display().to_string()),
    )
    .expect("write the config file");

    isolated(root.path()).arg("migrate").assert().success();

    assert!(palace.join("db").is_dir(), "the file's palace must be used");
    assert!(
        !root.path().join("data/memcastle/default").exists(),
        "the default palace must not be created when the file names another"
    );
}

#[test]
fn the_environment_outranks_the_config_file() {
    let root = tempfile::tempdir().expect("tempdir");
    let (from_file, from_env) = (root.path().join("file"), root.path().join("env"));
    let config = root.path().join("custom.toml");
    std::fs::write(
        &config,
        format!("[palace]\npath = {:?}\n", from_file.display().to_string()),
    )
    .expect("write the config file");

    isolated(root.path())
        .env("MEMCASTLE_PALACE_PATH", &from_env)
        .arg("--config")
        .arg(&config)
        .arg("migrate")
        .assert()
        .success();

    assert!(from_env.join("db").is_dir());
    assert!(!from_file.exists());
}

#[test]
fn the_palace_flag_outranks_the_environment() {
    let root = tempfile::tempdir().expect("tempdir");
    let (from_env, from_flag) = (root.path().join("env"), root.path().join("flag"));

    isolated(root.path())
        .env("MEMCASTLE_PALACE_PATH", &from_env)
        .arg("--palace")
        .arg(&from_flag)
        .arg("migrate")
        .assert()
        .success();

    assert!(from_flag.join("db").is_dir());
    assert!(!from_env.exists());
}

#[test]
fn a_relative_palace_flag_is_refused_with_a_diagnostic_naming_the_fix() {
    let root = tempfile::tempdir().expect("tempdir");
    let assert = isolated(root.path())
        .args(["--palace", "relative/palace", "migrate"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("memcastle::config::invalid") && stderr.contains("absolute"),
        "{stderr}"
    );
}

#[test]
fn an_explicit_config_file_that_is_missing_is_an_error_not_a_silent_default() {
    let root = tempfile::tempdir().expect("tempdir");
    isolated(root.path())
        .arg("--config")
        .arg(root.path().join("missing.toml"))
        .arg("migrate")
        .assert()
        .failure();
    assert!(!root.path().join("data").exists());
}
