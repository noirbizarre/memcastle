//! CLI smoke tests: the binary parses its documented commands and behaves
//! sensibly with no daemon around.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

#[test]
fn help_lists_the_top_level_commands() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("serve"))
        .stdout(contains("daemon"))
        .stdout(contains("mine"))
        .stdout(contains("audit"))
        .stdout(contains("repair"))
        .stdout(contains("  job "))
        .stdout(contains("recall"))
        .stdout(contains("  note "))
        .stdout(contains("wake-up"));
}

#[test]
fn doctor_help_lists_the_offline_diagnostics_command() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["doctor", "--help"])
        .assert()
        .success()
        .stdout(contains("Diagnose local configuration"));
}

#[test]
fn doctor_reports_invalid_config_without_printing_secret_values() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "[embeddings]\nprovider = 'secret-redaction-canary'\n",
    )
    .unwrap();
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .args(["doctor", "--config", config.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\": \"error\""), "{stdout}");
    assert!(stdout.contains("\"status\": \"skipped\""), "{stdout}");
    assert!(!stdout.contains("secret-redaction-canary"), "{stdout}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-redaction-canary"));
}

#[test]
fn doctor_reports_malformed_toml_without_echoing_the_line() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "[auth]\ntoken = 'secret-redaction-canary\n").unwrap();
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .args(["doctor", "--config", config.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\": \"error\""), "{stdout}");
    assert!(!stdout.contains("secret-redaction-canary"), "{stdout}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-redaction-canary"));
}

#[test]
fn doctor_reports_a_missing_explicit_assets_directory_as_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let palace = temp.path().join("untouched");
    let assets = temp.path().join("missing-assets");
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[palace]\npath = {:?}\n[server]\nport = 0\n[assets]\ndir = {:?}\n",
            palace.display().to_string(),
            assets.display().to_string()
        ),
    )
    .unwrap();
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .env_remove("MEMCASTLE_ASSETS_DIR")
        .args(["doctor", "--config", config.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("\"check\": \"assets.dir\""), "{stdout}");
    assert!(
        stdout.contains("Required directory is missing."),
        "{stdout}"
    );
    assert!(!palace.exists());
    assert!(!assets.exists());
}

#[test]
fn doctor_offline_warns_but_succeeds_without_creating_the_palace() {
    let temp = tempfile::tempdir().unwrap();
    let palace = temp.path().join("untouched");
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[palace]\npath = {:?}\n[server]\nport = 0\n",
            palace.display().to_string()
        ),
    )
    .unwrap();
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .args(["doctor", "--config", config.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\": \"warning\""), "{stdout}");
    assert!(stdout.contains("\"status\": \"skipped\""), "{stdout}");
    assert!(!palace.exists());
}

#[test]
fn mine_help_shows_the_source_first_grammar_and_no_longer_offers_the_old_flags() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["mine", "--help"])
        .assert()
        .success()
        .stdout(contains("memcastle mine <SOURCE>"))
        .stdout(contains(
            "memcastle mine opencode since=2026-09 dir=/path/to/workspace",
        ))
        .stdout(contains("memcastle mine /some/path"))
        .stdout(contains("--source").not())
        .stdout(contains("--locator").not());
}

#[test]
fn serve_help_says_it_runs_in_the_foreground() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(contains("foreground"));
}

#[test]
fn daemon_help_lists_start_stop_and_restart() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["daemon", "--help"])
        .assert()
        .success()
        .stdout(contains("start"))
        .stdout(contains("stop"))
        .stdout(contains("restart"));
}

#[test]
fn the_old_top_level_stop_and_restart_are_unknown_commands() {
    for word in ["stop", "restart"] {
        Command::cargo_bin("memcastle")
            .unwrap()
            .arg(word)
            .assert()
            .failure();
    }
}

#[test]
fn there_is_no_help_subcommand_because_the_flag_already_does_the_job() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .arg("help")
        .assert()
        .failure()
        .stderr(contains("unrecognized subcommand 'help'"));
}

#[test]
fn command_groups_do_not_list_a_help_entry_either() {
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .args(["job", "--help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Matching the indented entry, not the word: `--help` itself is listed.
    assert!(!stdout.contains("\n  help "), "{stdout}");
    assert!(stdout.contains("\n  list "), "{stdout}");
}

#[test]
fn piped_help_is_plain_text_without_escape_codes() {
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .env_remove("CLICOLOR_FORCE")
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{1b}'));
}

#[test]
fn forcing_colour_colours_help_and_no_color_wins_over_a_terminal_default() {
    let forced = Command::cargo_bin("memcastle")
        .unwrap()
        .env_remove("NO_COLOR")
        .env("CLICOLOR_FORCE", "1")
        .arg("--help")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&forced.stdout).contains("\u{1b}["));

    let opted_out = Command::cargo_bin("memcastle")
        .unwrap()
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .arg("--help")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&opted_out.stdout).contains('\u{1b}'));
}

#[test]
fn completions_print_a_script_for_every_supported_shell() {
    for (shell, marker) in [
        ("bash", "_memcastle"),
        ("zsh", "#compdef memcastle"),
        ("fish", "complete -c memcastle"),
        ("powershell", "memcastle"),
        ("elvish", "memcastle"),
    ] {
        Command::cargo_bin("memcastle")
            .unwrap()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(contains(marker));
    }
}

#[test]
fn completions_offer_the_subcommands_and_the_enumerated_values() {
    let output = Command::cargo_bin("memcastle")
        .unwrap()
        .args(["completions", "fish"])
        .output()
        .unwrap();
    let script = String::from_utf8_lossy(&output.stdout);
    assert!(script.contains("completions"), "{script}");
    // `--status` and `--mode` are enumerated, so tab completion can offer them.
    assert!(script.contains("cancelled"), "--status values: {script}");
    assert!(script.contains("read_only"), "--mode values: {script}");
    assert!(
        !script.contains("-a \"help\""),
        "no help subcommand: {script}"
    );
}

#[test]
fn completions_do_not_need_a_valid_configuration() {
    // Installing tab completion must work even when the config is what is broken.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "this is = [not valid").unwrap();
    Command::cargo_bin("memcastle")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .args(["completions", "bash"])
        .assert()
        .success();
}

#[test]
fn an_unknown_shell_is_rejected_listing_the_supported_ones() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .args(["completions", "tcsh"])
        .assert()
        .failure()
        .stderr(contains("bash").and(contains("zsh")));
}

#[test]
fn a_piped_status_report_is_json_with_no_escape_codes_even_when_colour_is_forced() {
    // Colour decorates the pretty rendering, which only a terminal gets: forcing it must not
    // put escape codes into what a pipe reads. The coloured rendering itself is held by the
    // `client::status` unit tests, since a test process has no terminal to run it in.
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let forced = status_without_a_daemon(&state, &palace)
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    let forced = String::from_utf8_lossy(&forced.stdout);
    assert!(!forced.contains('\u{1b}'), "{forced:?}");
    serde_json::from_str::<serde_json::Value>(&forced).expect("a piped status is JSON");
}

/// A `memcastle status` with no daemon, pointed at an empty palace and a
/// state directory of its own so the developer's real registry is never read.
fn status_without_a_daemon(state: &tempfile::TempDir, palace: &tempfile::TempDir) -> Command {
    let mut command = Command::cargo_bin("memcastle").unwrap();
    command
        .env("MEMCASTLE_PALACE_PATH", palace.path())
        .env("XDG_STATE_HOME", state.path())
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "1")
        .arg("status");
    command
}

#[test]
fn status_without_a_reachable_daemon_reports_not_running_and_exits_3() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    status_without_a_daemon(&state, &palace)
        .assert()
        // 3, not 1: a script must be able to tell "stopped" from "broken".
        .code(3)
        .stdout(contains("\"running\": false"))
        .stdout(contains("127.0.0.1:1"));
}

#[test]
fn daemon_stop_without_a_daemon_fails_and_points_at_daemon_start() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    Command::cargo_bin("memcastle")
        .unwrap()
        .env("MEMCASTLE_PALACE_PATH", palace.path())
        .env("XDG_STATE_HOME", state.path())
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "1")
        .args(["daemon", "stop"])
        .assert()
        .failure()
        .stderr(contains("memcastle::client::not_running"))
        .stderr(contains("daemon start"));
}

#[test]
fn status_json_without_a_daemon_says_running_false_and_still_names_the_palace() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let output = status_without_a_daemon(&state, &palace)
        .arg("--json")
        .assert()
        .code(3)
        .get_output()
        .stdout
        .clone();

    let view: serde_json::Value = serde_json::from_slice(&output).expect("status as JSON");
    assert_eq!(view["running"], false);
    assert_eq!(view["endpoint"], "http://127.0.0.1:1");
    assert_eq!(view["endpoint_source"], "config");
    assert_eq!(view["registry"]["state"], "absent");
    assert_eq!(view["palace_path"], palace.path().display().to_string());
    assert!(view["daemon"].is_null());
}

// Liveness is only probed on Unix; elsewhere a registry file is trusted as live.
#[cfg(unix)]
#[test]
fn status_diagnoses_a_registry_file_left_behind_by_a_killed_daemon() {
    let (state, palace) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // A PID that cannot be alive: above any Linux `pid_max` (2^22).
    let dead_pid = 4_194_305_u32;
    let registry = memcastle::server::lifecycle::RuntimeInfo {
        pid: dead_pid,
        bind_addr: "127.0.0.1:1".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        version: "0.0.0".to_string(),
    };
    // Written through the same path logic the binary uses, under the
    // subprocess's `XDG_STATE_HOME`, so it lands where `status` will look.
    let file = registry_file_under(state.path(), palace.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, serde_json::to_string(&registry).unwrap()).unwrap();

    status_without_a_daemon(&state, &palace)
        .assert()
        .code(3)
        .stdout(contains("\"state\": \"stale\""))
        .stdout(contains(format!("\"pid\": {dead_pid}")));
}

#[cfg(unix)]
/// Where `memcastle` will look for `palace`'s registry when its
/// `XDG_STATE_HOME` is `state` — see `server::lifecycle::registry_path`.
fn registry_file_under(state: &std::path::Path, palace: &std::path::Path) -> std::path::PathBuf {
    let canonical = std::fs::canonicalize(palace).unwrap();
    let digest = memcastle::domain::sha256_hex(canonical.display().to_string().as_bytes());
    state
        .join("memcastle")
        .join("run")
        .join(&digest[..16])
        .join("daemon.json")
}

#[test]
fn jobs_show_rejects_a_malformed_job_id_before_ever_reaching_the_network() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .env("MEMCASTLE_BIND", "127.0.0.1")
        .env("MEMCASTLE_PORT", "1")
        .args(["job", "show", "not-a-uuid"])
        .assert()
        .failure()
        // The diagnostic code, not just a non-zero exit: a malformed id must
        // be reported as such, not as a job that "was not found".
        .stderr(contains("memcastle::jobs::id_invalid"));
}

#[test]
fn a_reserved_but_unimplemented_command_says_so_instead_of_blaming_the_config() {
    Command::cargo_bin("memcastle")
        .unwrap()
        .arg("maintenance")
        .assert()
        .failure()
        .stderr(contains("memcastle::cli::not_implemented"));
}
