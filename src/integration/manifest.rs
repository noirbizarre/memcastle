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
    /// Claude Code.
    #[serde(rename = "claude-code")]
    ClaudeCode,
    /// OpenAI Codex.
    Codex,
}

impl AgentKind {
    /// The agent's command-line program, which is also how it is detected.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Opencode => "opencode",
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
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
    /// The skills the integration exposes to its agent, shared or its own.
    #[serde(default, rename = "skills")]
    pub skills: Vec<SkillEntry>,
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

/// One `[[skills]]` entry: a skill copied to `skills/<name>/` in the installed copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    /// The skill's name, which is also its directory name.
    pub name: String,
    /// Where it comes from: `false` is the shared `skills/<name>/` of the assets root, referenced and never duplicated in
    /// the integration's directory; `true` is `skills/<name>/` of the integration's own directory, for a skill only
    /// this integration needs.
    #[serde(default)]
    pub local: bool,
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
    // The format-1 manifests of #200 had a `[skills] install = true` table. Serde's own complaint about a table where a
    // list is expected does not say what to write instead, so the old shape is recognised and named.
    if let Ok(toml::Value::Table(table)) = toml::from_str::<toml::Value>(text)
        && table.get("skills").is_some_and(toml::Value::is_table)
    {
        return Err(invalid(
            "`[skills]` is no longer a table: list each skill the integration exposes as a `[[skills]]` entry with a `name` \
             (and `local = true` for one in the integration's own `skills/` directory)",
        ));
    }
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
    let mut seen = std::collections::BTreeSet::new();
    for skill in &manifest.skills {
        // The name becomes a directory under the installed copy, so it takes the alphabet a skill directory has and
        // cannot climb out.
        if !is_valid_source_name(&skill.name) {
            return Err(invalid(format!(
                "skills.name `{}` must be 1 to {MAX_SOURCE_NAME_LEN} lowercase letters, digits or `-`, and not start or end with `-`",
                skill.name
            )));
        }
        // Two entries for one name would install one of them over the other, in an order nobody chose.
        if !seen.insert(skill.name.as_str()) {
            return Err(invalid(format!(
                "skills.name `{}` is listed twice; a skill is installed once",
                skill.name
            )));
        }
    }
    match &manifest.agent.entry {
        Some(entry) if !is_contained(entry) || entry == "." => {
            return Err(invalid(format!(
                "agent.entry `{entry}` must be a file inside the installed copy"
            )));
        }
        // Plugin marketplaces need an index to register; Pi discovers its entry through package metadata.
        None if matches!(
            manifest.agent.kind,
            AgentKind::Opencode | AgentKind::ClaudeCode | AgentKind::Codex
        ) =>
        {
            return Err(invalid(
                "agent.entry is required for an OpenCode, Claude Code or Codex integration",
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

[[skills]]
name = "wake-up"

[[skills]]
name = "pi-notes"
local = true
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
        assert_eq!(manifest.skills.len(), 2);
        assert_eq!(manifest.skills[0].name, "wake-up");
        // Shared is the default: a skill is local only when the manifest says so.
        assert!(!manifest.skills[0].local);
        assert!(manifest.skills[1].local);
        assert_eq!(manifest.assets[0].to, ".");
    }

    #[test]
    fn a_skill_name_that_is_not_a_directory_name_is_refused() {
        for bad in ["Wake", "-wake", "../x", "a/b", ""] {
            let text = with(|t| t.replace("name = \"wake-up\"", &format!("name = \"{bad}\"")));
            assert!(message(text).contains("skills.name"), "{bad}");
        }
    }

    #[test]
    fn a_skill_listed_twice_is_refused() {
        let text = with(|t| t.replace("name = \"pi-notes\"\nlocal = true", "name = \"wake-up\""));
        assert!(message(text).contains("listed twice"));
    }

    #[test]
    fn the_old_skills_table_is_refused_with_the_form_to_write_instead() {
        let text = with(|t| {
            t.replace(
                "[[skills]]\nname = \"wake-up\"\n\n[[skills]]\nname = \"pi-notes\"\nlocal = true\n",
                "[skills]\ninstall = true\n",
            )
        });
        assert!(message(text).contains("[[skills]]"));
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
        assert!(manifest.skills.is_empty());
        assert_eq!(manifest.compatibility.agent, None);
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let text = with(|t| t.replace("local = true", "locl = true"));
        assert!(message(text).contains("locl"));
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

    #[test]
    fn a_codex_integration_needs_the_local_marketplace_catalog_it_registers() {
        let parsed = with(|text| text.replace("kind = \"pi\"", "kind = \"codex\"")).unwrap();
        assert_eq!(parsed.agent.kind.program(), "codex");
        let missing = with(|text| {
            text.replace(
                "kind = \"pi\"\nentry = \"extension.js\"",
                "kind = \"codex\"",
            )
        });
        assert!(message(missing).contains("agent.entry is required"));
    }
}
