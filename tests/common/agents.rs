//! A machine of its own for the `memcastle integration` tests: a home, fake `pi` and `opencode` programs on `PATH`,
//! and the XDG directories the real binary resolves everything from, so no test touches the developer's own agents.
//!
//! The fake agents answer `--version`, and `pi` also keeps a list of packages in a file that plays Pi's `settings.json`
//! (`install`, `remove`, `list`), which is everything the adapters use.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;

/// A stand-in agent: `--version`, and the package commands over `$FAKE_AGENT_STATE`.
const FAKE_AGENT: &str = r#"#!/bin/sh
state="$FAKE_AGENT_STATE"
case "$1" in
  --version) echo "$FAKE_AGENT_VERSION" ;;
  install) echo "$2" >> "$state" ;;
  remove) grep -vxF "$2" "$state" > "$state.tmp"; mv "$state.tmp" "$state"; true ;;
  list) echo "User packages:"; while read -r p; do echo "  $p"; echo "    $p"; done < "$state" ;;
  *) echo "unknown command $1" >&2; exit 2 ;;
esac
"#;

/// A throwaway machine.
pub struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    /// A home, both fake agents (version 1.18.34) and an empty Pi settings file.
    pub fn new() -> Self {
        let machine = Self {
            root: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(machine.path("bin")).unwrap();
        for program in ["pi", "opencode"] {
            let file = machine.path("bin").join(program);
            std::fs::write(&file, FAKE_AGENT).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(machine.path("pi-settings"), "").unwrap();
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
            .env("FAKE_AGENT_VERSION", "1.18.34")
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

    /// OpenCode's plugin shim.
    pub fn plugin_file(&self) -> PathBuf {
        self.path("config/opencode/plugins/memcastle.ts")
    }

    /// Where the installed copy of `id` lives.
    pub fn installed(&self, id: &str) -> PathBuf {
        self.path("data/memcastle/agents").join(id)
    }
}
