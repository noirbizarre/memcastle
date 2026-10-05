//! What differs between agents: detecting one, registering an integration with it, and undoing that.
//!
//! The manifest says what to copy; this says how the agent learns about the copy. Each adapter does that through the
//! agent's own mechanism and never edits the agent's settings file itself, so everything else the user has configured
//! stays exactly as it was:
//!
//! - Pi owns `settings.json`, so the adapter calls `pi install <dir>`, `pi remove <dir>` and `pi list`.
//! - OpenCode loads every file in its `plugins/` directory, so the adapter writes one file there, marked as MemCastle's,
//!   and removes only a file that carries the mark. It never touches `opencode.json`, and never adds an `mcp.memcastle`
//!   entry, which would show every tool twice.
//!
//! Running a program goes through [`Runner`] so the adapters are tested without an agent installed.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::config::paths::{self, XdgDir};
use crate::error::{Error, Result};

use super::manifest::{AgentKind, IntegrationManifest};

/// The text that marks a file as written by MemCastle, so removal never deletes one the user wrote.
const OPENCODE_MARKER: &str = "memcastle-integration: managed";

/// The file name of the OpenCode plugin shim.
const OPENCODE_PLUGIN_FILE: &str = "memcastle.ts";

/// Where the machine keeps what installation touches.
#[derive(Debug, Clone)]
pub struct Locations {
    /// Where installed copies live: `<data>/memcastle/agents`.
    pub agents_dir: PathBuf,
    /// OpenCode's configuration directory, whose `plugins/` it loads.
    pub opencode_config_dir: PathBuf,
}

impl Locations {
    /// The locations of the running process.
    #[must_use]
    pub fn from_process() -> Self {
        let lookup = |name: &str| std::env::var(name).ok();
        let home = dirs::home_dir();
        // OpenCode's own override comes first, as it does for OpenCode: the plugin must land where it will look.
        let opencode_config_dir = lookup("OPENCODE_CONFIG_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| {
                paths::base(XdgDir::Config, lookup, home.as_deref())
                    .map(|base| base.join("opencode"))
            })
            .unwrap_or_else(|| PathBuf::from("opencode"));
        Self {
            agents_dir: paths::default_agents_dir(),
            opencode_config_dir,
        }
    }
}

/// Runs a program and returns what it printed. The seam that lets tests stand in for an agent.
pub trait Runner {
    /// Run `program` with `args`.
    ///
    /// # Errors
    ///
    /// The I/O error when the program cannot be started; `NotFound` means it is not installed.
    fn run(&self, program: &str, args: &[&OsStr]) -> std::io::Result<Output>;
}

/// Runs programs found on `PATH`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[&OsStr]) -> std::io::Result<Output> {
        std::process::Command::new(program)
            .args(args)
            // The agent must never wait for a keystroke the CLI cannot give it.
            .stdin(std::process::Stdio::null())
            .output()
    }
}

/// What detection learned about an agent.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    /// What `--version` printed, trimmed.
    pub raw: String,
    /// The version in it, when there is one.
    pub version: Option<Version>,
}

/// What registration did, so that removal can undo exactly that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    /// How the agent was told: `pi-package` or `opencode-plugin`.
    pub method: String,
    /// What was registered: Pi's package directory, or OpenCode's plugin file.
    pub target: String,
    /// The file OpenCode's plugin shim re-exports. Pi finds its entry through the package, so it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// What undoing a registration found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unregistered {
    /// The registration was there and is gone.
    Removed,
    /// There was nothing to undo.
    NotRegistered,
    /// Something is at the target that MemCastle did not write, and it was left alone.
    LeftAlone,
}

/// One agent's way of taking an integration.
pub trait Agent {
    /// Run the agent's version command.
    ///
    /// # Errors
    ///
    /// [`Error::IntegrationAgentNotFound`] when the agent cannot be run.
    fn detect(&self, name: &str) -> Result<AgentInfo>;

    /// What registering the installed copy at `dir` would register; changes nothing.
    fn registration(&self, dir: &Path, manifest: &IntegrationManifest) -> Registration;

    /// Tell the agent about the installed copy.
    ///
    /// # Errors
    ///
    /// [`Error::IntegrationConflict`] when something not MemCastle's is in the way and
    /// [`Error::IntegrationRegistrationFailed`] when the agent refuses.
    fn register(&self, name: &str, registration: &Registration) -> Result<()>;

    /// Undo [`Agent::register`].
    ///
    /// # Errors
    ///
    /// [`Error::IntegrationRegistrationFailed`] when the agent refuses.
    fn unregister(&self, name: &str, registration: &Registration) -> Result<Unregistered>;

    /// Whether the agent currently knows the installed copy.
    ///
    /// # Errors
    ///
    /// [`Error::IntegrationAgentNotFound`] when the agent cannot be asked.
    fn is_registered(&self, name: &str, registration: &Registration) -> Result<bool>;
}

/// The adapter for `kind`.
#[must_use]
pub fn for_kind<'a>(
    kind: AgentKind,
    runner: &'a dyn Runner,
    locations: &Locations,
) -> Box<dyn Agent + 'a> {
    match kind {
        AgentKind::Pi => Box::new(Pi { runner }),
        AgentKind::Opencode => Box::new(OpenCode {
            runner,
            plugins_dir: locations.opencode_config_dir.join("plugins"),
        }),
    }
}

/// The version in an agent's `--version` output: the first word that is one, with a leading `v` dropped and a missing
/// patch number read as zero (`1.18` is `1.18.0`).
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(|word| {
        let word = word.trim_start_matches('v');
        Version::parse(word).ok().or_else(|| {
            let parts: Vec<&str> = word.split('.').collect();
            let numeric = parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            (parts.len() == 2 && numeric)
                .then(|| Version::parse(&format!("{word}.0")).ok())
                .flatten()
        })
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// The first non-empty thing a failed program said, for an error message.
fn failure_text(output: &Output) -> String {
    let stderr = text(&output.stderr);
    let said = if stderr.is_empty() {
        text(&output.stdout)
    } else {
        stderr
    };
    if said.is_empty() {
        format!("it exited with {}", output.status)
    } else {
        said
    }
}

fn detect_with(runner: &dyn Runner, kind: AgentKind, name: &str) -> Result<AgentInfo> {
    let program = kind.program();
    let missing = |message: String| Error::IntegrationAgentNotFound {
        name: name.to_string(),
        message,
    };
    let output = runner
        .run(program, &[OsStr::new("--version")])
        .map_err(|e| missing(format!("`{program} --version` could not run ({e})")))?;
    if !output.status.success() {
        return Err(missing(format!(
            "`{program} --version` failed: {}",
            failure_text(&output)
        )));
    }
    let raw = text(&output.stdout);
    Ok(AgentInfo {
        version: parse_version(&raw),
        raw,
    })
}

/// Pi, through its own package commands.
struct Pi<'a> {
    runner: &'a dyn Runner,
}

impl Agent for Pi<'_> {
    fn detect(&self, name: &str) -> Result<AgentInfo> {
        detect_with(self.runner, AgentKind::Pi, name)
    }

    fn registration(&self, dir: &Path, _manifest: &IntegrationManifest) -> Registration {
        Registration {
            method: "pi-package".to_string(),
            target: dir.display().to_string(),
            entry: None,
        }
    }

    fn register(&self, name: &str, registration: &Registration) -> Result<()> {
        let failed = |message: String| Error::IntegrationRegistrationFailed {
            name: name.to_string(),
            message,
        };
        let output = self
            .runner
            .run(
                "pi",
                &[OsStr::new("install"), OsStr::new(&registration.target)],
            )
            .map_err(|e| failed(format!("`pi install` could not run ({e})")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(failed(format!(
                "`pi install` failed: {}",
                failure_text(&output)
            )))
        }
    }

    fn unregister(&self, name: &str, registration: &Registration) -> Result<Unregistered> {
        // `pi remove` of a package Pi does not know is an error of its own; asking first keeps removal idempotent.
        if !self.is_registered(name, registration)? {
            return Ok(Unregistered::NotRegistered);
        }
        let output = self
            .runner
            .run(
                "pi",
                &[OsStr::new("remove"), OsStr::new(&registration.target)],
            )
            .map_err(|e| Error::IntegrationRegistrationFailed {
                name: name.to_string(),
                message: format!("`pi remove` could not run ({e})"),
            })?;
        if output.status.success() {
            Ok(Unregistered::Removed)
        } else {
            Err(Error::IntegrationRegistrationFailed {
                name: name.to_string(),
                message: format!("`pi remove` failed: {}", failure_text(&output)),
            })
        }
    }

    fn is_registered(&self, name: &str, registration: &Registration) -> Result<bool> {
        let output = self.runner.run("pi", &[OsStr::new("list")]).map_err(|e| {
            Error::IntegrationAgentNotFound {
                name: name.to_string(),
                message: format!("`pi list` could not run ({e})"),
            }
        })?;
        if !output.status.success() {
            return Err(Error::IntegrationAgentNotFound {
                name: name.to_string(),
                message: format!("`pi list` failed: {}", failure_text(&output)),
            });
        }
        // A whole line, trimmed: a package at `/a/pi` must not be mistaken for one at `/a/pi-other`.
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == registration.target))
    }
}

/// OpenCode, through one marked file in its plugins directory.
struct OpenCode<'a> {
    runner: &'a dyn Runner,
    plugins_dir: PathBuf,
}

impl OpenCode<'_> {
    fn plugin_file(&self) -> PathBuf {
        self.plugins_dir.join(OPENCODE_PLUGIN_FILE)
    }

    /// What the shim says: a one-line re-export, so the plugin runs from the installed copy and a MemCastle upgrade
    /// that replaces the copy needs no change here.
    fn shim(entry: &str) -> String {
        format!(
            "// {OPENCODE_MARKER}. Written by `memcastle integration install opencode`; \
             `memcastle integration remove opencode` deletes it.\nexport {{ default }} from {}\n",
            serde_json::Value::String(entry.to_string())
        )
    }
}

impl Agent for OpenCode<'_> {
    fn detect(&self, name: &str) -> Result<AgentInfo> {
        detect_with(self.runner, AgentKind::Opencode, name)
    }

    fn registration(&self, dir: &Path, manifest: &IntegrationManifest) -> Registration {
        // The manifest requires an entry for OpenCode, so the fallback only guards a hand-built manifest.
        let entry = manifest.agent.entry.as_deref().unwrap_or("index.js");
        Registration {
            method: "opencode-plugin".to_string(),
            target: self.plugin_file().display().to_string(),
            entry: Some(dir.join(entry).display().to_string()),
        }
    }

    fn register(&self, name: &str, registration: &Registration) -> Result<()> {
        let file = PathBuf::from(&registration.target);
        let entry = registration.entry.clone().unwrap_or_default();
        match std::fs::read_to_string(&file) {
            Ok(existing) if !existing.contains(OPENCODE_MARKER) => {
                return Err(Error::IntegrationConflict {
                    name: name.to_string(),
                    path: file.display().to_string(),
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(file.display().to_string(), e)),
        }
        std::fs::create_dir_all(&self.plugins_dir)
            .map_err(|e| Error::io(self.plugins_dir.display().to_string(), e))?;
        std::fs::write(&file, Self::shim(&entry))
            .map_err(|e| Error::io(file.display().to_string(), e))
    }

    fn unregister(&self, _name: &str, registration: &Registration) -> Result<Unregistered> {
        let file = PathBuf::from(&registration.target);
        match std::fs::read_to_string(&file) {
            Ok(existing) if existing.contains(OPENCODE_MARKER) => {
                std::fs::remove_file(&file)
                    .map_err(|e| Error::io(file.display().to_string(), e))?;
                Ok(Unregistered::Removed)
            }
            Ok(_) => Ok(Unregistered::LeftAlone),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Unregistered::NotRegistered),
            Err(e) => Err(Error::io(file.display().to_string(), e)),
        }
    }

    fn is_registered(&self, _name: &str, registration: &Registration) -> Result<bool> {
        let file = PathBuf::from(&registration.target);
        let expected = Self::shim(registration.entry.as_deref().unwrap_or_default());
        // Compared whole, so a shim that points at an older location counts as not registered and is rewritten.
        Ok(std::fs::read_to_string(&file).is_ok_and(|existing| existing == expected))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A stand-in for the agents' programs, shared with the lifecycle tests.

    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use super::{OsStr, Runner};

    /// Behaves like `pi` (a list of packages) and `opencode` (a version), and records every call.
    pub(crate) struct FakeAgents {
        pub pi_version: &'static str,
        pub opencode_version: &'static str,
        pub installed: RefCell<Vec<String>>,
        pub calls: RefCell<Vec<String>>,
        /// Make `pi install` fail with this message.
        pub refuse_install: RefCell<Option<&'static str>>,
        /// Pretend the program is not on `PATH`.
        pub missing: RefCell<bool>,
    }

    impl FakeAgents {
        pub(crate) fn new() -> Self {
            Self {
                pi_version: "1.0.1",
                opencode_version: "1.18.34",
                installed: RefCell::new(Vec::new()),
                calls: RefCell::new(Vec::new()),
                refuse_install: RefCell::new(None),
                missing: RefCell::new(false),
            }
        }
    }

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    impl Runner for FakeAgents {
        fn run(&self, program: &str, args: &[&OsStr]) -> std::io::Result<Output> {
            let args: Vec<String> = args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            self.calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            if *self.missing.borrow() {
                return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
            }
            Ok(match (program, args.first().map(String::as_str)) {
                ("pi", Some("--version")) => output(0, &format!("{}\n", self.pi_version), ""),
                ("opencode", Some("--version")) => {
                    output(0, &format!("{}\n", self.opencode_version), "")
                }
                ("pi", Some("install")) => {
                    if let Some(message) = *self.refuse_install.borrow() {
                        return Ok(output(1, "", message));
                    }
                    let mut installed = self.installed.borrow_mut();
                    if !installed.contains(&args[1]) {
                        installed.push(args[1].clone());
                    }
                    output(0, "", "")
                }
                ("pi", Some("remove")) => {
                    self.installed.borrow_mut().retain(|p| *p != args[1]);
                    output(0, "", "")
                }
                ("pi", Some("list")) => {
                    let listing: String = self
                        .installed
                        .borrow()
                        .iter()
                        .map(|p| format!("  {p}\n    {p}\n"))
                        .collect();
                    output(0, &format!("User packages:\n{listing}"), "")
                }
                _ => output(2, "", "unknown command"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeAgents;
    use super::*;
    use crate::integration::manifest;

    fn manifest_for(kind: &str) -> IntegrationManifest {
        manifest::parse(&format!(
            r#"format = 1
[integration]
id = "demo"
version = "0.1.0"
description = "demo"
[compatibility]
memcastle = "*"
[agent]
kind = "{kind}"
entry = "index.js"
[[assets]]
from = "dist"
to = "."
"#
        ))
        .unwrap()
    }

    fn locations(root: &Path) -> Locations {
        Locations {
            agents_dir: root.join("agents"),
            opencode_config_dir: root.join("opencode"),
        }
    }

    #[test]
    fn a_version_is_read_from_the_usual_ways_an_agent_prints_it() {
        let cases = [
            ("1.0.1", "1.0.1"),
            ("v1.18.34\n", "1.18.34"),
            ("pi coding agent 0.99.2", "0.99.2"),
            ("opencode 1.18", "1.18.0"),
        ];
        for (output, expected) in cases {
            assert_eq!(
                parse_version(output).unwrap().to_string(),
                expected,
                "{output}"
            );
        }
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn an_agent_that_cannot_run_is_reported_with_the_command_that_failed() {
        let fake = FakeAgents::new();
        *fake.missing.borrow_mut() = true;
        let agent = for_kind(AgentKind::Pi, &fake, &locations(Path::new("/x")));

        let error = agent.detect("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationAgentNotFound { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("pi --version"), "{error}");
    }

    #[test]
    fn pi_is_registered_and_unregistered_through_its_own_commands() {
        let fake = FakeAgents::new();
        let agent = for_kind(AgentKind::Pi, &fake, &locations(Path::new("/x")));
        let registration = agent.registration(Path::new("/data/agents/pi"), &manifest_for("pi"));

        assert!(!agent.is_registered("pi", &registration).unwrap());
        agent.register("pi", &registration).unwrap();
        assert!(agent.is_registered("pi", &registration).unwrap());
        assert_eq!(
            agent.unregister("pi", &registration).unwrap(),
            Unregistered::Removed
        );
        assert_eq!(
            agent.unregister("pi", &registration).unwrap(),
            Unregistered::NotRegistered
        );
        // Nothing but Pi's own commands ran: its settings file is never touched.
        assert!(
            fake.calls.borrow().iter().all(|c| c.starts_with("pi ")),
            "{:?}",
            fake.calls
        );
    }

    #[test]
    fn a_pi_package_whose_path_only_starts_the_same_is_not_mistaken_for_ours() {
        let fake = FakeAgents::new();
        fake.installed
            .borrow_mut()
            .push("/data/agents/pi-other".to_string());
        let agent = for_kind(AgentKind::Pi, &fake, &locations(Path::new("/x")));
        let registration = agent.registration(Path::new("/data/agents/pi"), &manifest_for("pi"));

        assert!(!agent.is_registered("pi", &registration).unwrap());
    }

    #[test]
    fn a_refusal_from_pi_is_reported_with_what_pi_said() {
        let fake = FakeAgents::new();
        *fake.refuse_install.borrow_mut() = Some("no package.json");
        let agent = for_kind(AgentKind::Pi, &fake, &locations(Path::new("/x")));
        let registration = agent.registration(Path::new("/data/agents/pi"), &manifest_for("pi"));

        let error = agent.register("pi", &registration).unwrap_err();

        assert!(
            matches!(error, Error::IntegrationRegistrationFailed { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("no package.json"), "{error}");
    }

    #[test]
    fn opencode_gets_one_marked_plugin_file_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeAgents::new();
        let agent = for_kind(AgentKind::Opencode, &fake, &locations(root.path()));
        let registration = agent.registration(
            Path::new("/data/agents/opencode"),
            &manifest_for("opencode"),
        );

        agent.register("opencode", &registration).unwrap();

        let file = root.path().join("opencode/plugins/memcastle.ts");
        let written = std::fs::read_to_string(&file).unwrap();
        assert!(written.contains(OPENCODE_MARKER));
        assert!(
            written.contains("export { default } from \"/data/agents/opencode/index.js\""),
            "{written}"
        );
        assert!(agent.is_registered("opencode", &registration).unwrap());
        // No opencode.json, so no `mcp.memcastle` entry and no other setting of the user's was touched.
        assert!(!root.path().join("opencode/opencode.json").exists());
        assert_eq!(
            agent.unregister("opencode", &registration).unwrap(),
            Unregistered::Removed
        );
        assert!(!file.exists());
    }

    #[test]
    fn a_plugin_file_the_user_wrote_is_never_overwritten_or_deleted() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeAgents::new();
        let agent = for_kind(AgentKind::Opencode, &fake, &locations(root.path()));
        let registration = agent.registration(Path::new("/d/opencode"), &manifest_for("opencode"));
        let file = root.path().join("opencode/plugins/memcastle.ts");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "export default () => ({})\n").unwrap();

        let error = agent.register("opencode", &registration).unwrap_err();

        assert!(
            matches!(error, Error::IntegrationConflict { .. }),
            "{error}"
        );
        assert_eq!(
            agent.unregister("opencode", &registration).unwrap(),
            Unregistered::LeftAlone
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "export default () => ({})\n"
        );
    }

    #[test]
    fn a_shim_pointing_at_another_location_does_not_count_as_registered() {
        let root = tempfile::tempdir().unwrap();
        let fake = FakeAgents::new();
        let agent = for_kind(AgentKind::Opencode, &fake, &locations(root.path()));
        let old = agent.registration(Path::new("/old/opencode"), &manifest_for("opencode"));
        let new = agent.registration(Path::new("/new/opencode"), &manifest_for("opencode"));
        agent.register("opencode", &old).unwrap();

        assert!(!agent.is_registered("opencode", &new).unwrap());
        // And registering again over our own marked file is how it is moved.
        agent.register("opencode", &new).unwrap();
        assert!(agent.is_registered("opencode", &new).unwrap());
    }
}
