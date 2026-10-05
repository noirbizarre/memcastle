//! The integration manifest: `memcastle-integration.toml`.
//!
//! One small file next to an integration's sources says what it is, which MemCastle and which agent it works with, what
//! to copy where, and what to check afterwards. Anything an agent needs that a file list cannot express (registering
//! with `pi install`, writing an OpenCode plugin file) lives behind [`crate::integration::agent::Agent`], chosen by
//! `[agent] kind`.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::domain::{MAX_SOURCE_NAME_LEN, is_valid_source_name};
use crate::error::{Error, Result};

/// The manifest's file name, in each `integrations/<id>/` directory.
pub const MANIFEST_FILE: &str = "memcastle-integration.toml";

/// The manifest format this MemCastle reads.
pub const MANIFEST_FORMAT: u32 = 1;

/// Which agent an integration is for, and so which adapter installs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    /// The Pi coding agent.
    Pi,
    /// OpenCode.
    Opencode,
}

impl AgentKind {
    /// The agent's command-line program, which is also how it is detected.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Opencode => "opencode",
        }
    }
}

/// A parsed `memcastle-integration.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationManifest {
    /// The manifest format.
    pub format: u32,
    /// What the integration is.
    pub integration: IntegrationInfo,
    /// What it works with.
    pub compatibility: Compatibility,
    /// Which agent it is for.
    pub agent: AgentSpec,
    /// The files to install, relative to the integration's directory.
    #[serde(default, rename = "assets")]
    pub assets: Vec<AssetEntry>,
    /// The shared skills the integration reads.
    #[serde(default)]
    pub skills: SkillsSpec,
}

/// `[integration]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationInfo {
    /// The identifier: the name `memcastle integration install <id>` takes.
    pub id: String,
    /// The integration's own semantic version.
    pub version: String,
    /// One line saying what it does.
    pub description: String,
}

/// `[compatibility]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    /// The MemCastle versions it works with, as a semver requirement.
    pub memcastle: String,
    /// The agent versions it works with, as a semver requirement. Unset means any version.
    #[serde(default)]
    pub agent: Option<String>,
}

/// `[agent]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// The agent, which selects the adapter.
    pub kind: AgentKind,
    /// The file the agent loads, relative to the installed copy. It must exist once installed, and OpenCode's plugin
    /// shim points at it. Pi finds its entry through the `package.json` the installed copy carries, so it is optional
    /// there.
    #[serde(default)]
    pub entry: Option<String>,
}

/// One `[[assets]]` entry: a file or directory copied into the installed copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetEntry {
    /// Where it is, relative to the integration's directory.
    pub from: String,
    /// Where it goes, relative to the installed copy. `.` puts a directory's contents at the top.
    pub to: String,
}

/// `[skills]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsSpec {
    /// Whether the shared `skills/` of the assets root are copied to `skills/` in the installed copy.
    #[serde(default)]
    pub install: bool,
}

fn invalid(message: impl Into<String>) -> Error {
    Error::IntegrationManifestInvalid {
        message: message.into(),
    }
}

/// Parse and validate a manifest.
///
/// # Errors
///
/// [`Error::IntegrationManifestInvalid`] naming the first thing wrong.
pub fn parse(text: &str) -> Result<IntegrationManifest> {
    let manifest: IntegrationManifest =
        toml::from_str(text).map_err(|source| invalid(source.message().to_string()))?;
    validate(&manifest)?;
    Ok(manifest)
}

/// Whether `path` stays inside the directory it is relative to: no root, no drive, no `..`.
fn is_contained(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

/// Check every rule a parse cannot.
///
/// # Errors
///
/// [`Error::IntegrationManifestInvalid`] naming the first thing wrong.
pub fn validate(manifest: &IntegrationManifest) -> Result<()> {
    // A format this MemCastle does not know may mean something it would silently ignore, so it is refused whole.
    if manifest.format == 0 || manifest.format > MANIFEST_FORMAT {
        return Err(invalid(format!(
            "format {} is not one this MemCastle reads (it reads 1 to {MANIFEST_FORMAT}); upgrade MemCastle",
            manifest.format
        )));
    }
    let id = &manifest.integration.id;
    // The id names a directory and a command argument, so it takes the same restricted alphabet as a source name.
    if !is_valid_source_name(id) {
        return Err(invalid(format!(
            "integration.id `{id}` must be 1 to {MAX_SOURCE_NAME_LEN} lowercase letters, digits or `-`, and not start or end with `-`"
        )));
    }
    if semver::Version::parse(&manifest.integration.version).is_err() {
        return Err(invalid(format!(
            "integration.version `{}` is not a semantic version like `0.1.0`",
            manifest.integration.version
        )));
    }
    let description = manifest.integration.description.trim();
    if description.is_empty() || description.contains('\n') {
        return Err(invalid(
            "integration.description must be one non-empty line",
        ));
    }
    if semver::VersionReq::parse(&manifest.compatibility.memcastle).is_err() {
        return Err(invalid(format!(
            "compatibility.memcastle `{}` is not a version requirement like `>=0.2, <0.4`",
            manifest.compatibility.memcastle
        )));
    }
    if let Some(agent) = &manifest.compatibility.agent
        && semver::VersionReq::parse(agent).is_err()
    {
        return Err(invalid(format!(
            "compatibility.agent `{agent}` is not a version requirement like `>=1.18.29`"
        )));
    }
    if manifest.assets.is_empty() {
        return Err(invalid(
            "at least one [[assets]] entry is needed: an integration with no files installs nothing",
        ));
    }
    for asset in &manifest.assets {
        // A path that climbs out would let a manifest copy from, or write to, anywhere on the machine.
        if !is_contained(&asset.from) || asset.from == "." {
            return Err(invalid(format!(
                "assets.from `{}` must be a path inside the integration's directory",
                asset.from
            )));
        }
        if !is_contained(&asset.to) {
            return Err(invalid(format!(
                "assets.to `{}` must be a path inside the installed copy, or `.`",
                asset.to
            )));
        }
    }
    match &manifest.agent.entry {
        Some(entry) if !is_contained(entry) || entry == "." => {
            return Err(invalid(format!(
                "agent.entry `{entry}` must be a file inside the installed copy"
            )));
        }
        // OpenCode loads whatever file its plugin shim names, so without an entry there is nothing to name.
        None if manifest.agent.kind == AgentKind::Opencode => {
            return Err(invalid(
                "agent.entry is required for an OpenCode integration",
            ));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
format = 1

[integration]
id = "demo"
version = "0.1.0"
description = "demo integration"

[compatibility]
memcastle = ">=0.2"
agent = ">=1.0"

[agent]
kind = "pi"
entry = "extension.js"

[[assets]]
from = "dist"
to = "."

[skills]
install = true
"#;

    fn with(edit: impl Fn(&str) -> String) -> Result<IntegrationManifest> {
        parse(&edit(GOOD))
    }

    /// The refusal as text; every refusal here is a manifest error, which the `invalid manifest` prefix proves.
    fn message(result: Result<IntegrationManifest>) -> String {
        let text = result.unwrap_err().to_string();
        assert!(text.starts_with("invalid integration manifest: "), "{text}");
        text
    }

    #[test]
    fn a_complete_manifest_parses() {
        let manifest = parse(GOOD).unwrap();
        assert_eq!(manifest.integration.id, "demo");
        assert_eq!(manifest.agent.kind, AgentKind::Pi);
        assert!(manifest.skills.install);
        assert_eq!(manifest.assets[0].to, ".");
    }

    #[test]
    fn a_manifest_with_only_the_required_keys_defaults_the_rest() {
        let manifest = parse(
            r#"
format = 1
[integration]
id = "demo"
version = "1.0.0"
description = "d"
[compatibility]
memcastle = "*"
[agent]
kind = "opencode"
entry = "dist/index.js"
[[assets]]
from = "dist"
to = "dist"
"#,
        )
        .unwrap();
        assert!(!manifest.skills.install);
        assert_eq!(manifest.compatibility.agent, None);
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let text = with(|t| t.replace("[skills]", "[skils]"));
        assert!(message(text).contains("skils"));
    }

    #[test]
    fn an_unknown_agent_is_refused() {
        let text = with(|t| t.replace("kind = \"pi\"", "kind = \"vim\""));
        assert!(message(text).contains("vim"));
    }

    #[test]
    fn a_format_newer_than_this_memcastle_reads_is_refused_whole() {
        let text = with(|t| t.replace("format = 1", "format = 2"));
        assert!(message(text).contains("upgrade MemCastle"));
    }

    #[test]
    fn an_id_that_is_not_a_lowercase_name_is_refused() {
        for bad in ["Demo", "-demo", "de mo", "../x", ""] {
            let text = with(|t| t.replace("id = \"demo\"", &format!("id = \"{bad}\"")));
            assert!(message(text).contains("integration.id"), "{bad}");
        }
    }

    #[test]
    fn a_description_that_is_empty_or_spans_lines_is_refused() {
        for bad in ["", "two\\nlines"] {
            let text = with(|t| {
                t.replace(
                    "description = \"demo integration\"",
                    &format!("description = \"{bad}\""),
                )
            });
            assert!(message(text).contains("integration.description"), "{bad:?}");
        }
    }

    #[test]
    fn a_version_or_requirement_that_does_not_parse_is_refused() {
        let version = with(|t| t.replace("version = \"0.1.0\"", "version = \"one\""));
        assert!(message(version).contains("integration.version"));
        let memcastle = with(|t| t.replace(">=0.2", "newer"));
        assert!(message(memcastle).contains("compatibility.memcastle"));
        let agent = with(|t| t.replace(">=1.0", "recent"));
        assert!(message(agent).contains("compatibility.agent"));
    }

    #[test]
    fn an_asset_path_that_climbs_out_or_is_absolute_is_refused() {
        for bad in ["../x", "/etc", "a/../../b"] {
            let from = with(|t| t.replace("from = \"dist\"", &format!("from = \"{bad}\"")));
            assert!(message(from).contains("assets.from"), "{bad}");
            let to = with(|t| t.replace("to = \".\"", &format!("to = \"{bad}\"")));
            assert!(message(to).contains("assets.to"), "{bad}");
        }
    }

    #[test]
    fn a_manifest_that_installs_no_files_is_refused() {
        let text = with(|t| t.replace("[[assets]]\nfrom = \"dist\"\nto = \".\"\n", ""));
        assert!(message(text).contains("[[assets]]"));
    }

    #[test]
    fn an_entry_outside_the_installed_copy_is_refused() {
        let text = with(|t| t.replace("entry = \"extension.js\"", "entry = \"../x\""));
        assert!(message(text).contains("agent.entry"));
    }

    #[test]
    fn an_opencode_integration_without_an_entry_is_refused() {
        let text = with(|t| {
            t.replace(
                "kind = \"pi\"\nentry = \"extension.js\"",
                "kind = \"opencode\"",
            )
        });
        assert!(message(text).contains("agent.entry is required"));
    }
}
