//! The lifecycle: install, update, remove, and the state `list` reports.
//!
//! An installed integration is a copy under `<data>/memcastle/agents/<id>/` plus whatever the agent's adapter registered,
//! recorded in a receipt beside the files. The copy is the user's, not the package's: a package upgrade that replaces
//! `share/memcastle` cannot break an installed agent, and the receipt says exactly what removal must undo.
//!
//! Every operation converges on the same end state however many times it runs. Files are staged next to the final
//! directory and swapped in whole, and a failed registration puts the previous copy back, so there is no moment at which
//! a half-written integration is what the agent loads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

use crate::domain::sha256_hex;
use crate::error::{Error, Result};

use super::agent::{self, Agent, Locations, Registration, Runner, Unregistered};
use super::catalog::{Catalog, Shipped};
use super::manifest::{AgentKind, IntegrationManifest};

/// The receipt's file name, inside the installed copy.
pub const RECEIPT_FILE: &str = ".memcastle-install.json";

/// The receipt format this MemCastle writes and reads.
const RECEIPT_FORMAT: u32 = 1;

/// Everything an operation needs from the machine, passed in so tests can stand in for it.
pub struct Context<'a> {
    /// Where installed copies and the agents' configuration live.
    pub locations: &'a Locations,
    /// How agents' programs are run.
    pub runner: &'a dyn Runner,
    /// The running MemCastle's version, which a manifest's requirement is checked against.
    pub memcastle_version: Version,
}

impl<'a> Context<'a> {
    /// The context of the running process.
    ///
    /// # Panics
    ///
    /// Never in practice: `CARGO_PKG_VERSION` is a semantic version by Cargo's own rules.
    #[must_use]
    pub fn for_process(locations: &'a Locations, runner: &'a dyn Runner) -> Self {
        Self {
            locations,
            runner,
            memcastle_version: Version::parse(env!("CARGO_PKG_VERSION"))
                .expect("Cargo guarantees a semantic version"),
        }
    }

    fn agent(&self, kind: AgentKind) -> Box<dyn Agent + 'a> {
        agent::for_kind(kind, self.runner, self.locations)
    }

    /// `<agents>/<id>`.
    #[must_use]
    pub fn install_dir(&self, id: &str) -> PathBuf {
        self.locations.agents_dir.join(id)
    }
}

/// What was recorded when an integration was installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// The receipt format.
    pub format: u32,
    /// The integration.
    pub id: String,
    /// Its version when installed.
    pub version: String,
    /// Which agent it is for, so removal needs no manifest.
    pub agent: AgentKind,
    /// The MemCastle that installed it.
    pub memcastle_version: String,
    /// The agent version at installation, as the agent printed it.
    pub agent_version: Option<String>,
    /// The assets root it was copied from, and so whether that was a package or a checkout.
    pub assets_root: String,
    /// Every file copied, relative to the installed copy with `/` separators, and its SHA-256.
    pub files: BTreeMap<String, String>,
    /// The skills exposed, by name: the directories under `skills/` in the installed copy. A receipt written before
    /// skills were declared has none listed, and its `files` still say what was copied.
    #[serde(default)]
    pub skills: Vec<String>,
    /// What the agent was told.
    pub registration: Registration,
}

/// What an installation looks like, as `list` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Shipped, not installed.
    NotInstalled,
    /// Installed, and identical to what is shipped.
    Installed,
    /// Installed from a version, or a build, that is not the one shipped now.
    Outdated,
    /// Installed, but a file was changed or removed or the agent no longer knows it.
    Modified,
    /// Not installable here: this MemCastle or this agent is outside the supported range.
    Incompatible,
    /// Not installable here: the shipped files are not complete.
    Unavailable,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotInstalled => "not installed",
            Self::Installed => "installed",
            Self::Outdated => "outdated",
            Self::Modified => "modified",
            Self::Incompatible => "incompatible",
            Self::Unavailable => "unavailable",
        })
    }
}

/// One integration as `list` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// The integration.
    pub id: String,
    /// What it does.
    pub description: String,
    /// Which agent it is for.
    pub agent: AgentKind,
    /// The version shipped.
    pub shipped_version: String,
    /// The version installed, when one is.
    pub installed_version: Option<String>,
    /// Where it stands.
    pub state: State,
    /// The agent's version as it printed it, when the agent could be run.
    pub agent_version: Option<String>,
    /// The MemCastle versions it supports.
    pub memcastle_requirement: String,
    /// The agent versions it supports.
    pub agent_requirement: Option<String>,
    /// The skills the shipped integration exposes to its agent, by name.
    pub skills: Vec<String>,
    /// What is wrong, when something is.
    pub problems: Vec<String>,
}

/// What an operation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// It was not installed and now is.
    Installed,
    /// A different version or build was installed and now this one is.
    Updated,
    /// It was already exactly as it should be.
    Unchanged,
    /// It was installed and now is not.
    Removed,
    /// It was not installed, so there was nothing to do.
    AlreadyAbsent,
}

/// One thing an operation changed on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// What happened.
    pub kind: ChangeKind,
    /// To what.
    pub target: String,
}

/// What kind of change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Files were copied to a new place.
    Copied,
    /// Files were replaced by a newer copy.
    Replaced,
    /// The agent was told about the copy.
    Registered,
    /// The agent was told to forget the copy.
    Unregistered,
    /// Files were deleted.
    Deleted,
    /// Something was not MemCastle's, or could not be reached, and was left as it was.
    LeftAlone,
}

/// The result of install, update or remove.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    /// The integration.
    pub id: String,
    /// What was done.
    pub action: Action,
    /// The version now installed, or the one that was removed.
    pub version: Option<String>,
    /// The installed copy's directory.
    pub directory: String,
    /// Every change made, in order. Empty when nothing needed doing.
    pub changes: Vec<Change>,
}

fn change(kind: ChangeKind, target: impl Into<String>) -> Change {
    Change {
        kind,
        target: target.into(),
    }
}

/// Check the manifest's requirement on MemCastle and, given what the agent printed, on the agent.
///
/// # Errors
///
/// [`Error::IntegrationIncompatible`] naming the requirement that fails and the version found.
pub fn check_compatible(
    manifest: &IntegrationManifest,
    memcastle: &Version,
    agent: Option<&agent::AgentInfo>,
) -> Result<()> {
    let name = &manifest.integration.id;
    let incompatible = |reason: String| Error::IntegrationIncompatible {
        name: name.clone(),
        reason,
    };
    let required = VersionReq::parse(&manifest.compatibility.memcastle)
        .map_err(|e| incompatible(e.to_string()))?;
    if !required.matches(memcastle) {
        return Err(incompatible(format!(
            "it needs MemCastle {required}, and this is {memcastle}"
        )));
    }
    let (Some(requirement), Some(agent)) = (&manifest.compatibility.agent, agent) else {
        return Ok(());
    };
    let required = VersionReq::parse(requirement).map_err(|e| incompatible(e.to_string()))?;
    let program = manifest.agent.kind.program();
    match &agent.version {
        Some(found) if required.matches(found) => Ok(()),
        Some(found) => Err(incompatible(format!(
            "it needs {program} {required}, and this is {found}"
        ))),
        // Guessing would install something that may not load, which is the failure this check exists to prevent.
        None => Err(incompatible(format!(
            "it needs {program} {required}, and `{program} --version` printed `{}`, which holds no version",
            agent.raw
        ))),
    }
}

/// The files an installation would write: destination (relative, `/`-separated) to source.
type Plan = BTreeMap<String, PathBuf>;

fn relative_slashes(path: &Path) -> String {
    path.components()
        // `to = "."` joins to `./file`; the `.` is not part of the name, and left in it the entry would never match.
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every file under `dir` as (path relative to `dir`, absolute path).
fn walk(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries =
            std::fs::read_dir(&current).map_err(|e| Error::io(current.display().to_string(), e))?;
        for entry in entries {
            let path = entry
                .map_err(|e| Error::io(current.display().to_string(), e))?
                .path();
            // `metadata` follows a link, so a symlinked file is copied as the file it points at.
            let meta =
                std::fs::metadata(&path).map_err(|e| Error::io(path.display().to_string(), e))?;
            if meta.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                found.push((relative.to_path_buf(), path.clone()));
            }
        }
    }
    found.sort();
    Ok(found)
}

fn place(plan: &mut Plan, id: &str, destination: String, source: PathBuf) -> Result<()> {
    if destination == RECEIPT_FILE {
        return Err(Error::IntegrationManifestInvalid {
            message: format!(
                "integration `{id}` installs a file named {RECEIPT_FILE}, which is the receipt's"
            ),
        });
    }
    // Two sources for one destination would make the result depend on copy order.
    if plan.insert(destination.clone(), source).is_some() {
        return Err(Error::IntegrationManifestInvalid {
            message: format!("integration `{id}` installs two different files to `{destination}`"),
        });
    }
    Ok(())
}

/// Work out which files an installation writes, and check that all of them exist.
fn plan_files(shipped: &Shipped, catalog: &Catalog) -> Result<Plan> {
    let id = shipped.id();
    let mut plan = Plan::new();
    for asset in &shipped.manifest.assets {
        let from = shipped.dir.join(&asset.from);
        if !from.exists() {
            return Err(Error::IntegrationAssetsMissing {
                message: format!(
                    "{} is missing from integration `{id}`; in a checkout, run `mise run integrations:build` first, \
                     and a package that lacks it is incomplete",
                    from.display()
                ),
            });
        }
        let to = Path::new(&asset.to);
        if from.is_dir() {
            for (relative, source) in walk(&from)? {
                place(&mut plan, id, relative_slashes(&to.join(relative)), source)?;
            }
        } else {
            // A file copied "to ." keeps its own name.
            let destination = if asset.to == "." {
                relative_slashes(Path::new(from.file_name().unwrap_or_default()))
            } else {
                relative_slashes(to)
            };
            place(&mut plan, id, destination, from)?;
        }
    }
    // Only the skills the manifest names are installed: a shared skill is read from the assets root where it is, a
    // local one from the integration's own directory, and `skills/README.md` (a working document) is never a skill.
    for skill in &shipped.manifest.skills {
        let (from, whose) = if skill.local {
            (shipped.dir.join("skills").join(&skill.name), "its own")
        } else {
            (catalog.skills_dir().join(&skill.name), "the shared")
        };
        // A directory without a `SKILL.md` is not a skill a client would list, so installing it would claim an exposure
        // that does not happen.
        if !from.join("SKILL.md").is_file() {
            return Err(Error::IntegrationAssetsMissing {
                message: format!(
                    "integration `{id}` exposes the skill `{}` from {whose} `skills/`, and {} holds no SKILL.md; \
                     a package that lacks it is incomplete",
                    skill.name,
                    from.display()
                ),
            });
        }
        for (relative, source) in walk(&from)? {
            place(
                &mut plan,
                id,
                format!("skills/{}/{}", skill.name, relative_slashes(&relative)),
                source,
            )?;
        }
    }
    if let Some(entry) = &shipped.manifest.agent.entry
        && !plan.contains_key(entry)
    {
        return Err(Error::IntegrationManifestInvalid {
            message: format!(
                "agent.entry `{entry}` of integration `{id}` is not among the files it installs"
            ),
        });
    }
    Ok(plan)
}

/// The names of the skills `manifest` exposes, in a stable order.
fn skill_names(manifest: &IntegrationManifest) -> Vec<String> {
    let mut names: Vec<String> = manifest.skills.iter().map(|s| s.name.clone()).collect();
    names.sort();
    names
}

fn digests(plan: &Plan) -> Result<BTreeMap<String, String>> {
    plan.iter()
        .map(|(destination, source)| {
            let bytes =
                std::fs::read(source).map_err(|e| Error::io(source.display().to_string(), e))?;
            Ok((destination.clone(), sha256_hex(&bytes)))
        })
        .collect()
}

fn read_receipt(directory: &Path) -> Result<Option<Receipt>> {
    let file = directory.join(RECEIPT_FILE);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(file.display().to_string(), e)),
    };
    // A receipt that cannot be read means MemCastle cannot tell what it installed; `remove` and a reinstall by hand
    // are the way out, and the message says so through the error's help.
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| Error::IntegrationValidationFailed {
            name: directory
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            message: format!("{} is not a valid receipt ({e})", file.display()),
        })
}

/// The files of the installed copy that differ from the receipt, as human-readable problems.
fn drift(directory: &Path, receipt: &Receipt) -> Vec<String> {
    let mut problems = Vec::new();
    for (relative, digest) in &receipt.files {
        match std::fs::read(directory.join(relative)) {
            Ok(bytes) if sha256_hex(&bytes) == *digest => {}
            Ok(_) => problems.push(format!("{relative} was changed after installation")),
            Err(_) => problems.push(format!("{relative} is missing")),
        }
    }
    problems
}

/// Where one integration stands.
///
/// # Errors
///
/// Only a failure to read the installed copy's directory; everything about the integration itself is part of the
/// answer and not an error.
pub fn inspect(shipped: &Shipped, catalog: &Catalog, ctx: &Context<'_>) -> Result<Status> {
    let manifest = &shipped.manifest;
    let id = shipped.id();
    let directory = ctx.install_dir(id);
    let agent = ctx.agent(manifest.agent.kind);
    let detected = agent.detect(id).ok();
    let mut status = Status {
        id: id.to_string(),
        description: manifest.integration.description.clone(),
        agent: manifest.agent.kind,
        shipped_version: manifest.integration.version.clone(),
        installed_version: None,
        state: State::NotInstalled,
        agent_version: detected.as_ref().map(|d| d.raw.clone()),
        memcastle_requirement: manifest.compatibility.memcastle.clone(),
        agent_requirement: manifest.compatibility.agent.clone(),
        skills: skill_names(manifest),
        problems: Vec::new(),
    };
    let receipt = match read_receipt(&directory) {
        Ok(receipt) => receipt,
        Err(error) => {
            status.state = State::Modified;
            status.problems.push(error.to_string());
            return Ok(status);
        }
    };
    status.installed_version = receipt.as_ref().map(|r| r.version.clone());

    let plan = match plan_files(shipped, catalog).and_then(|plan| digests(&plan)) {
        Ok(digests) => digests,
        Err(error) => {
            status.state = State::Unavailable;
            status.problems.push(error.to_string());
            return Ok(status);
        }
    };
    // Checked whether or not the integration is installed: an upgrade of MemCastle or of the agent can put an installed
    // copy outside what the shipped integration supports, and `update` would then refuse. `list` should say so first.
    if let Err(error) = check_compatible(manifest, &ctx.memcastle_version, detected.as_ref()) {
        status.state = State::Incompatible;
        status.problems.push(error.to_string());
        return Ok(status);
    }
    let Some(receipt) = receipt else {
        return Ok(status);
    };

    if receipt.version != manifest.integration.version || receipt.files != plan {
        status.state = State::Outdated;
        status
            .problems
            .push(if receipt.version == manifest.integration.version {
                "the shipped files differ from the installed ones".to_string()
            } else {
                format!(
                    "version {} is installed, and {} is shipped",
                    receipt.version, manifest.integration.version
                )
            });
        return Ok(status);
    }
    let mut problems = drift(&directory, &receipt);
    match agent.is_registered(id, &receipt.registration) {
        Ok(true) => {}
        Ok(false) => problems.push(format!(
            "{} is not registered with the agent",
            receipt.registration.target
        )),
        Err(error) => problems.push(error.to_string()),
    }
    if problems.is_empty() {
        status.state = State::Installed;
    } else {
        status.state = State::Modified;
        status.problems = problems;
    }
    Ok(status)
}

fn copy_into(plan: &Plan, destination: &Path) -> Result<()> {
    for (relative, source) in plan {
        let target = destination.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(parent.display().to_string(), e))?;
        }
        std::fs::copy(source, &target).map_err(|e| Error::io(target.display().to_string(), e))?;
    }
    Ok(())
}

fn remove_dir(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path.display().to_string(), e)),
    }
}

/// Install `shipped`, or bring an existing installation up to it.
///
/// Running it again changes nothing and says so.
///
/// # Errors
///
/// [`Error::IntegrationIncompatible`], [`Error::IntegrationAgentNotFound`], [`Error::IntegrationAssetsMissing`],
/// [`Error::IntegrationConflict`], [`Error::IntegrationRegistrationFailed`] or
/// [`Error::IntegrationValidationFailed`]; all but the last leave the machine as it was.
pub fn install(shipped: &Shipped, catalog: &Catalog, ctx: &Context<'_>) -> Result<Outcome> {
    let manifest = &shipped.manifest;
    let id = shipped.id();
    let agent = ctx.agent(manifest.agent.kind);

    // Everything that can refuse comes before the first write, so a refusal leaves nothing behind.
    let info = agent.detect(id)?;
    check_compatible(manifest, &ctx.memcastle_version, Some(&info))?;
    let plan = plan_files(shipped, catalog)?;
    let files = digests(&plan)?;

    let directory = ctx.install_dir(id);
    let previous = read_receipt(&directory).unwrap_or(None);
    let registration = agent.registration(&directory, manifest);
    if let Some(receipt) = &previous
        && receipt.version == manifest.integration.version
        && receipt.files == files
        && receipt.registration == registration
        && drift(&directory, receipt).is_empty()
        && agent.is_registered(id, &registration)?
    {
        return Ok(Outcome {
            id: id.to_string(),
            action: Action::Unchanged,
            version: Some(receipt.version.clone()),
            directory: directory.display().to_string(),
            changes: Vec::new(),
        });
    }

    let receipt = Receipt {
        format: RECEIPT_FORMAT,
        id: id.to_string(),
        version: manifest.integration.version.clone(),
        agent: manifest.agent.kind,
        memcastle_version: ctx.memcastle_version.to_string(),
        agent_version: Some(info.raw.clone()),
        assets_root: catalog.root().display().to_string(),
        files,
        skills: skill_names(manifest),
        registration: registration.clone(),
    };

    let parent = &ctx.locations.agents_dir;
    std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
    let staging = parent.join(format!(".{id}.staging"));
    let backup = parent.join(format!(".{id}.previous"));
    // Leftovers of an interrupted run would be mistaken for content.
    remove_dir(&staging)?;
    remove_dir(&backup)?;
    let staged = (|| {
        std::fs::create_dir_all(&staging)
            .map_err(|e| Error::io(staging.display().to_string(), e))?;
        copy_into(&plan, &staging)?;
        let text = serde_json::to_string_pretty(&receipt)
            .map_err(|e| Error::serialization("integration receipt", e))?;
        std::fs::write(staging.join(RECEIPT_FILE), text)
            .map_err(|e| Error::io(staging.join(RECEIPT_FILE).display().to_string(), e))
    })();
    if let Err(error) = staged {
        remove_dir(&staging)?;
        return Err(error);
    }

    let had_directory = directory.exists();
    if had_directory {
        std::fs::rename(&directory, &backup)
            .map_err(|e| Error::io(directory.display().to_string(), e))?;
    }
    std::fs::rename(&staging, &directory)
        .map_err(|e| Error::io(directory.display().to_string(), e))?;

    if let Err(error) = agent.register(id, &registration) {
        // Put things back as found: the previous copy if there was one, otherwise nothing.
        remove_dir(&directory)?;
        if had_directory {
            std::fs::rename(&backup, &directory)
                .map_err(|e| Error::io(directory.display().to_string(), e))?;
        }
        return Err(error);
    }
    remove_dir(&backup)?;

    if let Err(message) = validate(manifest, &directory, &*agent, id, &registration) {
        return Err(Error::IntegrationValidationFailed {
            name: id.to_string(),
            message,
        });
    }

    let replaced = previous.is_some();
    let mut changes = vec![change(
        if replaced {
            ChangeKind::Replaced
        } else {
            ChangeKind::Copied
        },
        format!("{} ({} files)", directory.display(), plan.len()),
    )];
    changes.push(change(ChangeKind::Registered, registration.target.clone()));
    Ok(Outcome {
        id: id.to_string(),
        action: if replaced {
            Action::Updated
        } else {
            Action::Installed
        },
        version: Some(manifest.integration.version.clone()),
        directory: directory.display().to_string(),
        changes,
    })
}

/// Check the installation the way the agent will meet it.
fn validate(
    manifest: &IntegrationManifest,
    directory: &Path,
    agent: &dyn Agent,
    id: &str,
    registration: &Registration,
) -> std::result::Result<(), String> {
    if let Some(entry) = &manifest.agent.entry
        && !directory.join(entry).is_file()
    {
        return Err(format!(
            "{} is missing from the installed copy",
            directory.join(entry).display()
        ));
    }
    match agent.is_registered(id, registration) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("the agent does not list {}", registration.target)),
        Err(error) => Err(error.to_string()),
    }
}

/// Bring an installed integration up to what is shipped.
///
/// # Errors
///
/// [`Error::IntegrationNotInstalled`] when it was never installed here (`install` is the command for that), and
/// otherwise those of [`install`].
pub fn update(shipped: &Shipped, catalog: &Catalog, ctx: &Context<'_>) -> Result<Outcome> {
    let directory = ctx.install_dir(shipped.id());
    if read_receipt(&directory).unwrap_or(None).is_none() {
        return Err(Error::IntegrationNotInstalled {
            name: shipped.id().to_string(),
        });
    }
    install(shipped, catalog, ctx)
}

/// Remove an installed integration: forget it in the agent, then delete the copy.
///
/// It needs no manifest, so an integration can be removed after the package that shipped it is gone. Removing what is
/// not installed succeeds and says so.
///
/// # Errors
///
/// [`Error::IntegrationRegistrationFailed`] when the agent refuses to forget it, with the files left in place so that
/// the removal can be retried.
pub fn remove(id: &str, ctx: &Context<'_>) -> Result<Outcome> {
    // The id becomes a path that is deleted; `../..` must never get that far. The manifest rule is the only alphabet an
    // installed id can have.
    if !crate::domain::is_valid_source_name(id) {
        return Err(Error::IntegrationNotFound {
            name: id.to_string(),
            root: ctx.locations.agents_dir.display().to_string(),
        });
    }
    let directory = ctx.install_dir(id);
    // An unreadable receipt must not make removal impossible, or the error that reports it would point at a command
    // that cannot run. The directory is MemCastle's own (`<data>/memcastle/agents/<id>`), so it is deleted, and what
    // the agent was told cannot be known and is reported.
    let Some(receipt) = read_receipt(&directory).unwrap_or(None) else {
        let mut changes = Vec::new();
        if directory.exists() {
            remove_dir(&directory)?;
            changes.push(change(ChangeKind::Deleted, directory.display().to_string()));
            changes.push(change(
                ChangeKind::LeftAlone,
                "whatever the agent was told: there is no readable receipt to say what it was"
                    .to_string(),
            ));
        }
        return Ok(Outcome {
            id: id.to_string(),
            action: if changes.is_empty() {
                Action::AlreadyAbsent
            } else {
                Action::Removed
            },
            version: None,
            directory: directory.display().to_string(),
            changes,
        });
    };
    let agent = ctx.agent(receipt.agent);
    let mut changes = Vec::new();
    match agent.unregister(id, &receipt.registration) {
        Ok(Unregistered::Removed) => {
            changes.push(change(
                ChangeKind::Unregistered,
                receipt.registration.target.clone(),
            ));
        }
        Ok(Unregistered::NotRegistered) => {}
        Ok(Unregistered::LeftAlone) => changes.push(change(
            ChangeKind::LeftAlone,
            format!(
                "{} was not written by MemCastle",
                receipt.registration.target
            ),
        )),
        // The agent is gone, so there is nothing left for it to forget; the files should not outlive it.
        Err(Error::IntegrationAgentNotFound { message, .. }) => changes.push(change(
            ChangeKind::LeftAlone,
            format!(
                "{} (the agent could not be asked: {message})",
                receipt.registration.target
            ),
        )),
        Err(error) => return Err(error),
    }
    remove_dir(&directory)?;
    changes.push(change(ChangeKind::Deleted, directory.display().to_string()));
    Ok(Outcome {
        id: id.to_string(),
        action: Action::Removed,
        version: Some(receipt.version),
        directory: directory.display().to_string(),
        changes,
    })
}

// Needs the fake agents, which are Unix-only (see `agent::fake`).
#[cfg(all(test, unix))]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::assets::AssetSource;
    use crate::integration::agent::fake::FakeAgents;
    use crate::integration::manifest::MANIFEST_FILE;

    /// A machine: an assets root with the `pi` and `opencode` integrations, and the user's own directories.
    struct Machine {
        root: tempfile::TempDir,
        fake: FakeAgents,
        locations: Locations,
        version: RefCell<Version>,
    }

    fn manifest(id: &str, kind: &str, version: &str, memcastle: &str, agent: &str) -> String {
        format!(
            r#"format = 1
[integration]
id = "{id}"
version = "{version}"
description = "the {id} integration"
[compatibility]
memcastle = "{memcastle}"
agent = "{agent}"
[agent]
kind = "{kind}"
entry = "dist/index.js"
[[assets]]
from = "dist"
to = "dist"
[[skills]]
name = "wake-up"
"#
        )
    }

    impl Machine {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let assets = root.path().join("assets");
            for (id, kind) in [("pi", "pi"), ("opencode", "opencode")] {
                let dir = assets.join("integrations").join(id);
                std::fs::create_dir_all(dir.join("dist")).unwrap();
                std::fs::write(dir.join("dist/index.js"), "export default {}\n").unwrap();
                std::fs::write(dir.join("dist/package.json"), "{}").unwrap();
                std::fs::write(
                    dir.join(MANIFEST_FILE),
                    manifest(id, kind, "0.1.0", ">=0.1", ">=1.0"),
                )
                .unwrap();
            }
            std::fs::create_dir_all(assets.join("skills/wake-up")).unwrap();
            std::fs::write(assets.join("skills/wake-up/SKILL.md"), "# wake\n").unwrap();
            std::fs::write(assets.join("skills/README.md"), "working notes\n").unwrap();
            // Shared but declared by no manifest: an integration installs the skills it names and no others.
            std::fs::create_dir_all(assets.join("skills/diary")).unwrap();
            std::fs::write(assets.join("skills/diary/SKILL.md"), "# diary\n").unwrap();
            let locations = Locations {
                agents_dir: root.path().join("data/agents"),
                opencode_config_dir: root.path().join("config/opencode"),
            };
            Self {
                root,
                fake: FakeAgents::new(),
                locations,
                version: RefCell::new(Version::parse("0.2.0").unwrap()),
            }
        }

        fn assets(&self) -> PathBuf {
            self.root.path().join("assets")
        }

        fn catalog(&self) -> Catalog {
            Catalog::read(self.assets(), AssetSource::Override(self.assets())).unwrap()
        }

        fn ctx(&self) -> Context<'_> {
            Context {
                locations: &self.locations,
                runner: &self.fake,
                memcastle_version: self.version.borrow().clone(),
            }
        }

        fn install(&self, id: &str) -> Result<Outcome> {
            let catalog = self.catalog();
            install(catalog.get(id)?, &catalog, &self.ctx())
        }

        fn update(&self, id: &str) -> Result<Outcome> {
            let catalog = self.catalog();
            update(catalog.get(id)?, &catalog, &self.ctx())
        }

        fn status(&self, id: &str) -> Status {
            let catalog = self.catalog();
            inspect(catalog.get(id).unwrap(), &catalog, &self.ctx()).unwrap()
        }

        fn plugin_file(&self) -> PathBuf {
            self.root
                .path()
                .join("config/opencode/plugins/memcastle.ts")
        }

        fn rewrite_manifest(&self, id: &str, text: String) {
            std::fs::write(
                self.assets()
                    .join("integrations")
                    .join(id)
                    .join(MANIFEST_FILE),
                text,
            )
            .unwrap();
        }
    }

    fn plan_error(machine: &Machine, id: &str) -> Error {
        machine.install(id).unwrap_err()
    }

    #[test]
    fn the_context_of_the_process_checks_against_the_version_this_binary_was_built_as() {
        let machine = Machine::new();

        let ctx = Context::for_process(&machine.locations, &machine.fake);

        assert_eq!(ctx.memcastle_version.to_string(), env!("CARGO_PKG_VERSION"));
        assert_eq!(
            ctx.install_dir("pi"),
            machine.locations.agents_dir.join("pi")
        );
    }

    #[test]
    fn every_state_has_the_words_list_prints() {
        let words: Vec<_> = [
            State::NotInstalled,
            State::Installed,
            State::Outdated,
            State::Modified,
            State::Incompatible,
            State::Unavailable,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();

        assert_eq!(
            words,
            [
                "not installed",
                "installed",
                "outdated",
                "modified",
                "incompatible",
                "unavailable"
            ]
        );
    }

    #[test]
    fn an_integration_with_no_agent_requirement_accepts_any_agent_and_one_with_a_requirement_needs_a_readable_version()
     {
        let open =
            manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0").replace("agent = \">=1.0\"\n", "");
        let open = crate::integration::manifest::parse(&open).unwrap();
        let gibberish = agent::AgentInfo {
            raw: "no numbers".to_string(),
            version: None,
        };
        let running = Version::parse("0.2.0").unwrap();
        assert!(check_compatible(&open, &running, Some(&gibberish)).is_ok());

        let strict =
            crate::integration::manifest::parse(&manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0"))
                .unwrap();
        let error = check_compatible(&strict, &running, Some(&gibberish)).unwrap_err();
        assert!(error.to_string().contains("holds no version"), "{error}");
        assert!(error.to_string().contains("no numbers"), "{error}");
    }

    #[test]
    fn a_file_asset_installs_under_its_own_name_or_the_name_the_manifest_gives() {
        let machine = Machine::new();
        let extra = machine.assets().join("integrations/pi/dist/extra.json");
        std::fs::write(&extra, "{}").unwrap();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            + "[[assets]]\nfrom = \"dist/extra.json\"\nto = \".\"\n[[assets]]\nfrom = \"dist/package.json\"\nto = \"conf/pkg.json\"\n";
        machine.rewrite_manifest("pi", text);

        machine.install("pi").unwrap();

        let dir = machine.locations.agents_dir.join("pi");
        assert!(dir.join("extra.json").is_file() && dir.join("conf/pkg.json").is_file());
    }

    #[test]
    fn two_assets_that_land_on_one_file_are_refused_rather_than_resolved_by_copy_order() {
        let machine = Machine::new();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            + "[[assets]]\nfrom = \"dist/package.json\"\nto = \"dist/index.js\"\n";
        machine.rewrite_manifest("pi", text);

        let error = plan_error(&machine, "pi");

        assert!(
            error
                .to_string()
                .contains("two different files to `dist/index.js`"),
            "{error}"
        );
    }

    #[test]
    fn an_asset_that_would_overwrite_the_receipt_is_refused() {
        let machine = Machine::new();
        std::fs::write(
            machine
                .assets()
                .join("integrations/pi/dist/.memcastle-install.json"),
            "{}",
        )
        .unwrap();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            + "[[assets]]\nfrom = \"dist/package.json\"\nto = \".memcastle-install.json\"\n";
        machine.rewrite_manifest("pi", text);

        let error = plan_error(&machine, "pi");

        assert!(
            error.to_string().contains("which is the receipt's"),
            "{error}"
        );
    }

    #[test]
    fn a_shared_skill_the_manifest_names_must_exist_in_the_shared_skills_directory() {
        let machine = Machine::new();
        std::fs::remove_dir_all(machine.assets().join("skills")).unwrap();

        let error = plan_error(&machine, "pi");

        assert!(
            matches!(error, Error::IntegrationAssetsMissing { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("`wake-up`"), "{error}");
        assert!(error.to_string().contains("the shared"), "{error}");
    }

    #[test]
    fn a_skill_directory_without_a_skill_file_is_refused_before_anything_is_written() {
        let machine = Machine::new();
        std::fs::remove_file(machine.assets().join("skills/wake-up/SKILL.md")).unwrap();
        std::fs::write(machine.assets().join("skills/wake-up/notes.md"), "x").unwrap();

        let error = plan_error(&machine, "pi");

        assert!(error.to_string().contains("no SKILL.md"), "{error}");
        assert!(!machine.locations.agents_dir.join("pi").exists());
    }

    #[test]
    fn only_the_skills_a_manifest_names_are_installed() {
        let machine = Machine::new();

        machine.install("pi").unwrap();

        let skills = machine.locations.agents_dir.join("pi/skills");
        assert!(skills.join("wake-up/SKILL.md").is_file());
        // `diary` is shared and present in the assets root, but this integration does not expose it.
        assert!(!skills.join("diary").exists());
    }

    #[test]
    fn an_integration_with_no_skills_declared_installs_none_and_needs_no_skills_directory() {
        let machine = Machine::new();
        std::fs::remove_dir_all(machine.assets().join("skills")).unwrap();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            .replace("[[skills]]\nname = \"wake-up\"\n", "");
        machine.rewrite_manifest("pi", text);

        machine.install("pi").unwrap();

        assert!(!machine.locations.agents_dir.join("pi/skills").exists());
        assert!(machine.status("pi").skills.is_empty());
    }

    #[test]
    fn a_local_skill_ships_from_the_integrations_own_directory() {
        let machine = Machine::new();
        let local = machine.assets().join("integrations/pi/skills/pi-notes");
        std::fs::create_dir_all(local.join("references")).unwrap();
        std::fs::write(local.join("SKILL.md"), "# notes\n").unwrap();
        std::fs::write(local.join("references/more.md"), "more\n").unwrap();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            + "[[skills]]\nname = \"pi-notes\"\nlocal = true\n";
        machine.rewrite_manifest("pi", text);

        machine.install("pi").unwrap();

        let skills = machine.locations.agents_dir.join("pi/skills");
        assert!(skills.join("pi-notes/SKILL.md").is_file());
        assert!(skills.join("pi-notes/references/more.md").is_file());
        // It belongs to `pi` alone: the other integration did not name it.
        machine.install("opencode").unwrap();
        assert!(
            !machine
                .locations
                .agents_dir
                .join("opencode/skills/pi-notes")
                .exists()
        );
    }

    #[test]
    fn a_local_skill_is_not_looked_for_among_the_shared_ones_and_the_reverse() {
        let machine = Machine::new();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            .replace("name = \"wake-up\"", "name = \"wake-up\"\nlocal = true");
        machine.rewrite_manifest("pi", text);

        let error = plan_error(&machine, "pi");

        assert!(error.to_string().contains("its own"), "{error}");
    }

    #[test]
    fn a_local_and_a_shared_skill_of_one_name_cannot_both_be_installed() {
        // The manifest parser refuses a repeated name, so the clash is only reachable from a skill file that an asset
        // also lands on: `[[assets]]` writing into `skills/wake-up/` must not silently override the shared skill.
        let machine = Machine::new();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            + "[[assets]]\nfrom = \"dist/package.json\"\nto = \"skills/wake-up/SKILL.md\"\n";
        machine.rewrite_manifest("pi", text);

        let error = plan_error(&machine, "pi");

        assert!(error.to_string().contains("two different files"), "{error}");
    }

    #[test]
    fn dropping_a_skill_from_the_manifest_removes_it_on_update_and_leaves_the_users_own_skills() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        // A skill the user keeps in a client location is never inside the installed copy, so it cannot be touched.
        let own = machine.root.path().join("home/.agents/skills/mine");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("SKILL.md"), "# mine\n").unwrap();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            .replace("[[skills]]\nname = \"wake-up\"\n", "");
        machine.rewrite_manifest("pi", text);

        let outcome = machine.update("pi").unwrap();

        assert_eq!(outcome.action, Action::Updated);
        assert!(
            !machine
                .locations
                .agents_dir
                .join("pi/skills/wake-up")
                .exists()
        );
        assert!(own.join("SKILL.md").is_file());
    }

    #[test]
    fn a_change_to_a_shipped_skill_makes_the_installation_outdated_until_updated() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        std::fs::write(
            machine.assets().join("skills/wake-up/SKILL.md"),
            "# newer\n",
        )
        .unwrap();

        assert_eq!(machine.status("pi").state, State::Outdated);
        machine.update("pi").unwrap();

        assert_eq!(machine.status("pi").state, State::Installed);
    }

    #[test]
    fn a_listing_names_the_skills_each_integration_exposes() {
        let machine = Machine::new();

        assert_eq!(machine.status("pi").skills, ["wake-up"]);
    }

    #[test]
    fn a_failure_while_copying_removes_the_staging_directory_and_touches_nothing_installed() {
        // `a` is a file in the plan and `a/b` is below it: the second copy cannot create its directory.
        let machine = Machine::new();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            + "[[assets]]\nfrom = \"dist/package.json\"\nto = \"a\"\n[[assets]]\nfrom = \"dist/index.js\"\nto = \"a/b\"\n";
        machine.rewrite_manifest("pi", text);

        let error = plan_error(&machine, "pi");

        assert!(matches!(error, Error::Io { .. }), "{error}");
        assert!(!machine.locations.agents_dir.join(".pi.staging").exists());
        assert!(!machine.locations.agents_dir.join("pi").exists());
        assert!(machine.fake.installed.borrow().is_empty());
    }

    #[test]
    fn an_agent_that_accepts_the_registration_but_does_not_list_it_fails_validation() {
        let machine = Machine::new();
        *machine.fake.forget_installs.borrow_mut() = true;

        let error = plan_error(&machine, "pi");

        assert!(
            matches!(error, Error::IntegrationValidationFailed { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("does not list"), "{error}");
    }

    #[test]
    fn validation_names_a_missing_entry_and_an_agent_that_cannot_be_asked() {
        let machine = Machine::new();
        let catalog = machine.catalog();
        let manifest = &catalog.get("pi").unwrap().manifest;
        let agent = agent::for_kind(AgentKind::Pi, &machine.fake, &machine.locations);
        let registration = agent.registration(Path::new("/d/pi"), manifest);
        let empty = machine.root.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let missing = validate(manifest, &empty, &*agent, "pi", &registration).unwrap_err();
        assert!(
            missing.contains("is missing from the installed copy"),
            "{missing}"
        );

        std::fs::create_dir_all(empty.join("dist")).unwrap();
        std::fs::write(empty.join("dist/index.js"), "x").unwrap();
        *machine.fake.missing.borrow_mut() = true;
        let unreachable = validate(manifest, &empty, &*agent, "pi", &registration).unwrap_err();
        assert!(unreachable.contains("could not run"), "{unreachable}");
    }

    #[test]
    fn a_receipt_path_that_is_not_readable_is_reported_by_list_and_by_install() {
        let machine = Machine::new();
        let dir = machine.locations.agents_dir.join("pi");
        std::fs::create_dir_all(dir.join(RECEIPT_FILE)).unwrap();

        let status = machine.status("pi");

        assert_eq!(status.state, State::Modified);
        assert!(status.problems[0].contains(RECEIPT_FILE), "{status:?}");
        // And installing treats it as no previous installation, replacing the junk.
        assert_eq!(machine.install("pi").unwrap().action, Action::Installed);
    }

    #[test]
    fn a_deleted_file_is_reported_missing_and_a_rebuilt_bundle_as_outdated_at_the_same_version() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        std::fs::remove_file(machine.locations.agents_dir.join("pi/dist/index.js")).unwrap();
        let status = machine.status("pi");
        assert_eq!(status.state, State::Modified);
        assert!(
            status.problems[0].contains("dist/index.js is missing"),
            "{status:?}"
        );

        machine.install("pi").unwrap();
        std::fs::write(
            machine.assets().join("integrations/pi/dist/index.js"),
            "export default { v: 3 }\n",
        )
        .unwrap();
        let status = machine.status("pi");
        assert_eq!(status.state, State::Outdated);
        assert!(
            status.problems[0].contains("shipped files differ"),
            "{status:?}"
        );
    }

    #[test]
    fn list_reports_an_agent_that_cannot_be_asked_instead_of_failing() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        *machine.fake.missing.borrow_mut() = true;

        let status = machine.status("pi");

        assert_eq!(status.state, State::Modified);
        assert_eq!(status.agent_version, None);
        assert!(
            status.problems.iter().any(|p| p.contains("could not run")),
            "{status:?}"
        );
    }

    #[test]
    fn removing_leaves_a_plugin_file_the_user_replaced_and_says_so() {
        let machine = Machine::new();
        machine.install("opencode").unwrap();
        std::fs::write(machine.plugin_file(), "// mine now\n").unwrap();

        let outcome = remove("opencode", &machine.ctx()).unwrap();

        assert_eq!(outcome.action, Action::Removed);
        assert!(
            outcome
                .changes
                .iter()
                .any(|c| c.kind == ChangeKind::LeftAlone),
            "{outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(machine.plugin_file()).unwrap(),
            "// mine now\n"
        );
        assert!(!machine.locations.agents_dir.join("opencode").exists());
    }

    #[test]
    fn an_agent_that_refuses_to_forget_the_copy_keeps_the_files_so_removal_can_be_retried() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        *machine.fake.exit_on.borrow_mut() = Some(("remove", "busy"));

        let error = remove("pi", &machine.ctx()).unwrap_err();

        assert!(
            matches!(error, Error::IntegrationRegistrationFailed { .. }),
            "{error}"
        );
        assert!(machine.locations.agents_dir.join("pi").is_dir());
        *machine.fake.exit_on.borrow_mut() = None;
        assert_eq!(
            remove("pi", &machine.ctx()).unwrap().action,
            Action::Removed
        );
    }

    #[test]
    fn a_directory_without_a_receipt_is_deleted_by_remove_and_nothing_is_reported_when_absent() {
        let machine = Machine::new();
        let stray = machine.locations.agents_dir.join("pi");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("junk"), "x").unwrap();

        let outcome = remove("pi", &machine.ctx()).unwrap();

        assert_eq!(outcome.action, Action::Removed);
        assert!(!stray.exists());
        assert_eq!(
            remove("pi", &machine.ctx()).unwrap().action,
            Action::AlreadyAbsent
        );
    }

    #[test]
    fn deleting_a_directory_that_is_not_one_is_an_error_with_its_path() {
        let machine = Machine::new();
        let file = machine.root.path().join("a-file");
        std::fs::write(&file, "x").unwrap();

        let result = remove_dir(&file);

        // Some platforms delete a plain file here and others refuse; what must hold is that a refusal names the path.
        if let Err(error) = result {
            assert!(error.to_string().contains("a-file"), "{error}");
        }
        assert!(remove_dir(&machine.root.path().join("never-existed")).is_ok());
    }

    #[test]
    fn installing_twice_changes_nothing_registers_with_pi_and_reports_what_changed() {
        let machine = Machine::new();

        let outcome = machine.install("pi").unwrap();

        assert_eq!(outcome.action, Action::Installed);
        let dir = machine.locations.agents_dir.join("pi");
        assert!(dir.join("dist/index.js").is_file());
        assert!(dir.join("skills/wake-up/SKILL.md").is_file());
        // The repository's own notes in skills/ are not skills.
        assert!(!dir.join("skills/README.md").exists());
        assert!(dir.join(RECEIPT_FILE).is_file());
        assert_eq!(
            machine.fake.installed.borrow().as_slice(),
            [dir.display().to_string()]
        );
        let kinds: Vec<_> = outcome.changes.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ChangeKind::Copied, ChangeKind::Registered]);
        assert_eq!(machine.status("pi").state, State::Installed);
    }

    #[test]
    fn assets_installed_to_the_top_of_the_copy_keep_their_plain_names() {
        let machine = Machine::new();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            .replace("to = \"dist\"", "to = \".\"")
            .replace("entry = \"dist/index.js\"", "entry = \"index.js\"");
        machine.rewrite_manifest("pi", text);

        machine.install("pi").unwrap();

        let dir = machine.locations.agents_dir.join("pi");
        assert!(dir.join("index.js").is_file() && dir.join("package.json").is_file());
        let receipt = read_receipt(&dir).unwrap().unwrap();
        assert!(
            receipt.files.contains_key("index.js"),
            "{:?}",
            receipt.files
        );
    }

    #[test]
    fn installing_twice_changes_nothing_the_second_time_and_says_so() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        let before =
            std::fs::read(machine.locations.agents_dir.join("pi").join(RECEIPT_FILE)).unwrap();
        machine.fake.calls.borrow_mut().clear();

        let outcome = machine.install("pi").unwrap();

        assert_eq!(outcome.action, Action::Unchanged);
        assert!(outcome.changes.is_empty());
        let after =
            std::fs::read(machine.locations.agents_dir.join("pi").join(RECEIPT_FILE)).unwrap();
        assert_eq!(before, after);
        // No `pi install` the second time: only questions.
        assert!(
            machine
                .fake
                .calls
                .borrow()
                .iter()
                .all(|c| !c.starts_with("pi install")),
            "{:?}",
            machine.fake.calls
        );
    }

    #[test]
    fn a_new_version_replaces_the_old_one_through_update() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        machine.rewrite_manifest("pi", manifest("pi", "pi", "0.2.0", ">=0.1", ">=1.0"));
        assert_eq!(machine.status("pi").state, State::Outdated);

        let outcome = machine.update("pi").unwrap();

        assert_eq!(outcome.action, Action::Updated);
        assert_eq!(outcome.version.as_deref(), Some("0.2.0"));
        assert_eq!(machine.status("pi").state, State::Installed);
        assert_eq!(machine.update("pi").unwrap().action, Action::Unchanged);
    }

    #[test]
    fn a_rebuilt_integration_with_the_same_version_is_still_an_update() {
        // A development checkout never bumps the version between builds; the content is what has to be tracked.
        let machine = Machine::new();
        machine.install("pi").unwrap();
        std::fs::write(
            machine.assets().join("integrations/pi/dist/index.js"),
            "export default { v: 2 }\n",
        )
        .unwrap();

        let outcome = machine.update("pi").unwrap();

        assert_eq!(outcome.action, Action::Updated);
        let installed = machine.locations.agents_dir.join("pi/dist/index.js");
        assert_eq!(
            std::fs::read_to_string(installed).unwrap(),
            "export default { v: 2 }\n"
        );
    }

    #[test]
    fn updating_what_was_never_installed_points_at_install() {
        let machine = Machine::new();

        let error = machine.update("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationNotInstalled { .. }),
            "{error}"
        );
    }

    #[test]
    fn removing_forgets_the_registration_deletes_the_copy_and_is_idempotent() {
        let machine = Machine::new();
        machine.install("pi").unwrap();

        let outcome = remove("pi", &machine.ctx()).unwrap();

        assert_eq!(outcome.action, Action::Removed);
        assert!(!machine.locations.agents_dir.join("pi").exists());
        assert!(machine.fake.installed.borrow().is_empty());
        let kinds: Vec<_> = outcome.changes.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [ChangeKind::Unregistered, ChangeKind::Deleted]);
        assert_eq!(
            remove("pi", &machine.ctx()).unwrap().action,
            Action::AlreadyAbsent
        );
    }

    #[test]
    fn a_receipt_that_cannot_be_read_does_not_make_the_copy_impossible_to_remove() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        std::fs::write(
            machine.locations.agents_dir.join("pi").join(RECEIPT_FILE),
            "{ not json",
        )
        .unwrap();
        assert_eq!(machine.status("pi").state, State::Modified);

        let outcome = remove("pi", &machine.ctx()).unwrap();

        assert_eq!(outcome.action, Action::Removed);
        assert!(!machine.locations.agents_dir.join("pi").exists());
        assert!(
            outcome
                .changes
                .iter()
                .any(|c| c.kind == ChangeKind::LeftAlone)
        );
    }

    #[test]
    fn an_id_that_is_a_path_can_never_delete_outside_the_installed_copies() {
        let machine = Machine::new();
        let victim = machine.root.path().join("data/precious");
        std::fs::create_dir_all(&victim).unwrap();

        let error = remove("../precious", &machine.ctx()).unwrap_err();

        assert!(
            matches!(error, Error::IntegrationNotFound { .. }),
            "{error}"
        );
        assert!(victim.is_dir());
    }

    #[test]
    fn removing_needs_neither_the_package_nor_the_agent() {
        let machine = Machine::new();
        machine.install("opencode").unwrap();
        std::fs::remove_dir_all(machine.assets()).unwrap();

        let outcome = remove("opencode", &machine.ctx()).unwrap();

        assert_eq!(outcome.action, Action::Removed);
        assert!(!machine.plugin_file().exists());
        assert!(!machine.locations.agents_dir.join("opencode").exists());
    }

    #[test]
    fn removing_pi_after_pi_itself_is_gone_still_deletes_the_files() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        *machine.fake.missing.borrow_mut() = true;

        let outcome = remove("pi", &machine.ctx()).unwrap();

        assert_eq!(outcome.action, Action::Removed);
        assert!(
            outcome
                .changes
                .iter()
                .any(|c| c.kind == ChangeKind::LeftAlone)
        );
        assert!(!machine.locations.agents_dir.join("pi").exists());
    }

    #[test]
    fn opencode_installs_one_shim_and_leaves_the_users_other_plugins_and_config_alone() {
        let machine = Machine::new();
        let config = machine.root.path().join("config/opencode");
        std::fs::create_dir_all(config.join("plugins")).unwrap();
        std::fs::write(
            config.join("plugins/mine.ts"),
            "export default () => ({})\n",
        )
        .unwrap();
        std::fs::write(config.join("opencode.json"), "{\"theme\":\"dark\"}\n").unwrap();

        machine.install("opencode").unwrap();

        assert!(machine.plugin_file().is_file());
        assert_eq!(
            std::fs::read_to_string(config.join("opencode.json")).unwrap(),
            "{\"theme\":\"dark\"}\n"
        );
        remove("opencode", &machine.ctx()).unwrap();
        assert_eq!(
            std::fs::read_to_string(config.join("plugins/mine.ts")).unwrap(),
            "export default () => ({})\n"
        );
        assert_eq!(
            std::fs::read_to_string(config.join("opencode.json")).unwrap(),
            "{\"theme\":\"dark\"}\n"
        );
    }

    #[test]
    fn pis_other_packages_are_preserved() {
        let machine = Machine::new();
        machine
            .fake
            .installed
            .borrow_mut()
            .push("npm:other-package".to_string());

        machine.install("pi").unwrap();
        remove("pi", &machine.ctx()).unwrap();

        assert_eq!(
            machine.fake.installed.borrow().as_slice(),
            ["npm:other-package"]
        );
    }

    #[test]
    fn a_memcastle_outside_the_supported_range_is_refused_before_anything_is_written() {
        let machine = Machine::new();
        *machine.version.borrow_mut() = Version::parse("0.0.9").unwrap();

        let error = machine.install("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationIncompatible { .. }),
            "{error}"
        );
        assert!(error.to_string().contains(">=0.1"), "{error}");
        assert!(!machine.locations.agents_dir.exists());
        assert!(machine.fake.installed.borrow().is_empty());
        assert_eq!(machine.status("pi").state, State::Incompatible);
    }

    #[test]
    fn an_installed_integration_that_a_newer_memcastle_no_longer_supports_is_listed_incompatible() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        *machine.version.borrow_mut() = Version::parse("0.0.9").unwrap();

        let status = machine.status("pi");

        assert_eq!(status.state, State::Incompatible);
        assert_eq!(status.installed_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn an_agent_older_than_the_supported_range_is_refused_with_both_versions() {
        let mut machine = Machine::new();
        machine.fake.pi_version = "0.9.0";

        let error = machine.install("pi").unwrap_err();

        let message = error.to_string();
        assert!(
            matches!(error, Error::IntegrationIncompatible { .. }),
            "{message}"
        );
        assert!(
            message.contains(">=1.0") && message.contains("0.9.0"),
            "{message}"
        );
        assert!(!machine.locations.agents_dir.exists());
        machine.fake.pi_version = "1.0.1";
        assert!(machine.install("pi").is_ok());
    }

    #[test]
    fn a_missing_agent_is_reported_before_anything_is_written() {
        let machine = Machine::new();
        *machine.fake.missing.borrow_mut() = true;

        let error = machine.install("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationAgentNotFound { .. }),
            "{error}"
        );
        assert!(!machine.locations.agents_dir.exists());
    }

    #[test]
    fn a_failed_registration_leaves_no_copy_behind() {
        let machine = Machine::new();
        *machine.fake.refuse_install.borrow_mut() = Some("nope");

        let error = machine.install("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationRegistrationFailed { .. }),
            "{error}"
        );
        assert!(!machine.locations.agents_dir.join("pi").exists());
        assert!(!machine.locations.agents_dir.join(".pi.staging").exists());
    }

    #[test]
    fn a_failed_registration_during_an_update_restores_the_previous_copy() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        std::fs::write(
            machine.assets().join("integrations/pi/dist/index.js"),
            "export default { v: 2 }\n",
        )
        .unwrap();
        *machine.fake.refuse_install.borrow_mut() = Some("nope");

        machine.update("pi").unwrap_err();

        let installed = machine.locations.agents_dir.join("pi/dist/index.js");
        assert_eq!(
            std::fs::read_to_string(installed).unwrap(),
            "export default {}\n"
        );
        assert!(!machine.locations.agents_dir.join(".pi.previous").exists());
    }

    #[test]
    fn a_plugin_file_the_user_wrote_stops_the_install_and_survives_it() {
        let machine = Machine::new();
        std::fs::create_dir_all(machine.plugin_file().parent().unwrap()).unwrap();
        std::fs::write(machine.plugin_file(), "// mine\n").unwrap();

        let error = machine.install("opencode").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationConflict { .. }),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(machine.plugin_file()).unwrap(),
            "// mine\n"
        );
        assert!(!machine.locations.agents_dir.join("opencode").exists());
    }

    #[test]
    fn a_file_changed_after_installation_is_reported_and_repaired_by_installing_again() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        let file = machine.locations.agents_dir.join("pi/dist/index.js");
        std::fs::write(&file, "tampered").unwrap();

        let status = machine.status("pi");
        assert_eq!(status.state, State::Modified);
        assert!(status.problems[0].contains("dist/index.js"), "{status:?}");

        assert_eq!(machine.install("pi").unwrap().action, Action::Updated);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "export default {}\n"
        );
        assert_eq!(machine.status("pi").state, State::Installed);
    }

    #[test]
    fn an_integration_the_agent_forgot_is_reported_modified_and_registered_again_by_install() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        machine.fake.installed.borrow_mut().clear();
        assert_eq!(machine.status("pi").state, State::Modified);

        machine.install("pi").unwrap();

        assert_eq!(machine.fake.installed.borrow().len(), 1);
        assert_eq!(machine.status("pi").state, State::Installed);
    }

    #[test]
    fn a_checkout_that_was_not_built_says_what_to_run() {
        let machine = Machine::new();
        std::fs::remove_dir_all(machine.assets().join("integrations/pi/dist")).unwrap();

        let error = machine.install("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationAssetsMissing { .. }),
            "{error}"
        );
        assert!(
            error.to_string().contains("mise run integrations:build"),
            "{error}"
        );
        assert_eq!(machine.status("pi").state, State::Unavailable);
    }

    #[test]
    fn a_manifest_whose_entry_is_not_among_its_files_is_refused() {
        let machine = Machine::new();
        let text = manifest("pi", "pi", "0.1.0", ">=0.1", ">=1.0")
            .replace("dist/index.js", "dist/main.js");
        machine.rewrite_manifest("pi", text);

        let error = machine.install("pi").unwrap_err();

        assert!(
            matches!(error, Error::IntegrationManifestInvalid { .. }),
            "{error}"
        );
    }

    #[test]
    fn the_receipt_records_what_was_installed_from_where() {
        let machine = Machine::new();
        machine.install("opencode").unwrap();

        let receipt = read_receipt(&machine.locations.agents_dir.join("opencode"))
            .unwrap()
            .unwrap();

        assert_eq!(receipt.id, "opencode");
        assert_eq!(receipt.agent, AgentKind::Opencode);
        assert_eq!(receipt.assets_root, machine.assets().display().to_string());
        assert_eq!(receipt.memcastle_version, "0.2.0");
        assert_eq!(receipt.agent_version.as_deref(), Some("1.18.34"));
        assert!(receipt.files.contains_key("dist/index.js"));
        assert!(receipt.files.contains_key("skills/wake-up/SKILL.md"));
        assert_eq!(receipt.skills, ["wake-up"]);
        assert_eq!(
            receipt.registration.target,
            machine.plugin_file().display().to_string()
        );
    }

    #[test]
    fn a_receipt_written_before_skills_were_declared_still_reads() {
        let machine = Machine::new();
        machine.install("pi").unwrap();
        let dir = machine.locations.agents_dir.join("pi");
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(RECEIPT_FILE)).unwrap())
                .unwrap();
        value.as_object_mut().unwrap().remove("skills");
        std::fs::write(dir.join(RECEIPT_FILE), value.to_string()).unwrap();

        let receipt = read_receipt(&dir).unwrap().unwrap();

        assert!(receipt.skills.is_empty());
        // Its files still say what was copied, so removal and drift detection work as before.
        assert!(receipt.files.contains_key("skills/wake-up/SKILL.md"));
    }

    #[test]
    fn the_listing_names_how_the_assets_were_chosen_whatever_the_source() {
        let machine = Machine::new();
        for (source, expected) in [
            (AssetSource::Override(machine.assets()), "override"),
            (AssetSource::Installed(machine.assets()), "installed"),
            (AssetSource::Embedded, "embedded"),
        ] {
            let catalog = Catalog::read(machine.assets(), source).unwrap();

            let report = crate::integration::render::list(&catalog, &machine.ctx()).unwrap();

            assert_eq!(report.assets_source, expected);
            assert_eq!(report.integrations.len(), 2);
        }
    }

    #[test]
    fn a_list_of_a_machine_with_nothing_installed_shows_every_integration_as_available() {
        let machine = Machine::new();
        let catalog = machine.catalog();

        let states: Vec<_> = catalog
            .integrations()
            .iter()
            .map(|s| inspect(s, &catalog, &machine.ctx()).unwrap().state)
            .collect();

        assert_eq!(states, [State::NotInstalled, State::NotInstalled]);
    }
}
