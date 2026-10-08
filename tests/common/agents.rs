//! A machine of its own for the `memcastle integration` tests: a home, fake Pi, OpenCode and Claude programs on `PATH`,
//! and the XDG directories the real binary resolves everything from, so no test touches the developer's own agents.
//!
//! The fake agents answer `--version`; Pi keeps a package list and Claude keeps a plugin list, which is everything the
//! adapters use.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;

/// Stand-in agent commands, with state files rather than an actual agent configuration.
const FAKE_AGENT: &str = r#"#!/bin/sh
case "$1" in
  --version)
    if [ -n "$FAKE_AGENT_VERSION" ]; then echo "$FAKE_AGENT_VERSION";
    elif [ "${0##*/}" = "claude" ]; then echo "2.1.83";
    else echo "1.18.34"; fi ;;
  install) echo "$2" >> "$FAKE_AGENT_STATE" ;;
  remove) grep -vxF "$2" "$FAKE_AGENT_STATE" > "$FAKE_AGENT_STATE.tmp"; mv "$FAKE_AGENT_STATE.tmp" "$FAKE_AGENT_STATE"; true ;;
  list) echo "User packages:"; while read -r p; do echo "  $p"; echo "    $p"; done < "$FAKE_AGENT_STATE" ;;
  plugin)
    case "$2" in
      list) cat "$FAKE_CLAUDE_STATE" ;;
      install)
        for target; do :; done
        grep -qxF "$target" "$FAKE_CLAUDE_STATE" || echo "$target" >> "$FAKE_CLAUDE_STATE" ;;
      uninstall) grep -vxF "$3" "$FAKE_CLAUDE_STATE" > "$FAKE_CLAUDE_STATE.tmp"; mv "$FAKE_CLAUDE_STATE.tmp" "$FAKE_CLAUDE_STATE"; true ;;
      marketplace) true ;;
      *) echo "unknown plugin command $2" >&2; exit 2 ;;
    esac ;;
  *) echo "unknown command $1" >&2; exit 2 ;;
esac
"#;

/// A throwaway machine.
pub struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    /// A home, three fake agents and empty Pi and Claude registries.
    pub fn new() -> Self {
        let machine = Self {
            root: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(machine.path("bin")).unwrap();
        for program in ["pi", "opencode", "claude"] {
            let file = machine.path("bin").join(program);
            std::fs::write(&file, FAKE_AGENT).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(machine.path("pi-settings"), "").unwrap();
        std::fs::write(machine.path("claude-plugins"), "").unwrap();
        machine
    }

    /// `relative` under the machine's root.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    /// `memcastle` with the machine's environment and nothing of the developer's own.
    pub fn memcastle(&self) -> Command {
        let mut command = Command::cargo_bin("memcastle").unwrap();
        self.environment(&mut command);
        command
    }

    /// Give `command` the machine's environment.
    pub fn environment(&self, command: &mut Command) {
        command
            .env_clear()
            .env("HOME", self.path("home"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("FAKE_AGENT_STATE", self.path("pi-settings"))
            .env("FAKE_CLAUDE_STATE", self.path("claude-plugins"))
            .env("NO_COLOR", "1");
    }

    /// The packages Pi has been told about.
    pub fn pi_packages(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("pi-settings"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The plugins Claude has been told about.
    pub fn claude_plugins(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("claude-plugins"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// OpenCode's plugin shim.
    pub fn plugin_file(&self) -> PathBuf {
        self.path("config/opencode/plugins/memcastle.ts")
    }

    /// Where the installed copy of `id` lives.
    pub fn installed(&self, id: &str) -> PathBuf {
        self.path("data/memcastle/agents").join(id)
    }
}
