//! Versioned plugin identity and module inventory; no filesystem or network access.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

/// The root manifest's supported format.
pub const PLUGIN_FORMAT: u32 = 1;

/// One repository/release is one plugin, regardless of how many modules it provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Format of `plugin.toml`.
    pub format: u32,
    /// Stable identity and publisher-provided metadata.
    pub plugin: PluginInfo,
    /// Semver requirement for the host.
    pub memcastle: String,
    /// Other installed plugins required before activation.
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
    /// Shared provider configuration shape, never secret values or source instance state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_config_schema: Option<Value>,
    /// Public shared authentication metadata; actual tokens stay in the credential store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<PluginAuthentication>,
    /// Independently selectable capabilities; an empty plugin is valid.
    #[serde(default)]
    pub modules: Vec<PluginModule>,
}

/// Identity and provenance of an installable plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInfo {
    /// Stable package identifier.
    pub id: String,
    /// Independent semver release.
    pub version: String,
    /// Shared implementation family, which is not a module ID.
    pub provider: String,
    /// Human-readable summary.
    pub description: String,
    /// Repository URL for tracing a release to its publisher.
    pub repository: String,
    /// SPDX license identifier.
    pub license: String,
}

/// A separately versioned plugin prerequisite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDependency {
    /// Stable plugin ID.
    pub id: String,
    /// Compatible versions.
    pub version: String,
    /// A missing optional plugin does not prevent this plugin from installing.
    #[serde(default)]
    pub optional: bool,
}

/// Shared login metadata. Individual modules still decide whether they use this login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginAuthentication {
    /// The public authentication scheme (e.g. `oauth_device` or `oauth_browser`).
    pub kind: String,
    /// A credential reference, never a token or client secret.
    pub credential: String,
}

/// Which contract a module implements; embedding execution is provided by #256.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginModuleKind {
    /// Existing source WIT contract.
    Source,
    /// Existing agent integration contract.
    Integration,
    /// Reserved for the separate embedding provider runtime.
    EmbeddingProvider,
}

/// A module's stable identity and location inside one plugin archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginModule {
    /// Globally stable module ID; miners/integrations keep referring to this across releases.
    pub id: String,
    /// Independent capability, not a second installable package.
    #[serde(rename = "type")]
    pub kind: PluginModuleKind,
    /// Module implementation version, independent of the package release.
    pub version: String,
    /// Path to its module-specific manifest relative to the package root.
    pub manifest: String,
    /// SHA-256 of the module-specific manifest.
    pub manifest_sha256: String,
    /// Path to the WASM component for a source, or an integration directory's entry file.
    pub entry: String,
    /// SHA-256 of the entry file.
    pub sha256: String,
    /// An optional module is discoverable but need not run on every installation.
    #[serde(default)]
    pub optional: bool,
    /// Host contract requirement, if this kind has a versioned contract.
    #[serde(default)]
    pub contract: Option<String>,
    /// Optional JSON Schema for this module's instance settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,
}

/// One installed package and the immutable artifact generation it currently uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRecord {
    /// Stable plugin ID.
    pub id: String,
    /// Fully validated parent contract.
    pub manifest: PluginManifest,
    /// Archive digest; the archive was verified before its artifacts were staged.
    pub digest: String,
    /// Generation directory, relative to the configured plugin root.
    pub generation: String,
    /// Registry URL or repository location used for subsequent update checks.
    pub upstream: Option<String>,
    /// Trusted key ID, when a registry release carried a verified signature.
    pub signed_by: Option<String>,
    /// First install time.
    pub installed_at: DateTime<Utc>,
    /// Most recent replacement time.
    pub updated_at: DateTime<Utc>,
}

impl PluginManifest {
    /// Whether replacing this plugin would introduce a cycle through already installed packages.
    #[must_use]
    pub fn dependency_cycle(&self, installed: &[PluginRecord]) -> Option<Vec<String>> {
        fn walk(
            id: &str,
            target: &str,
            installed: &[PluginRecord],
            seen: &mut HashSet<String>,
            path: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            if id == target {
                path.push(id.to_string());
                return Some(path.clone());
            }
            if !seen.insert(id.to_string()) {
                return None;
            }
            let found = installed.iter().find(|record| record.id == id)?;
            path.push(id.to_string());
            for dependency in &found.manifest.dependencies {
                if let Some(cycle) = walk(&dependency.id, target, installed, seen, path) {
                    return Some(cycle);
                }
            }
            path.pop();
            None
        }

        for dependency in &self.dependencies {
            let mut seen = HashSet::new();
            let mut path = vec![self.plugin.id.clone()];
            if let Some(cycle) = walk(
                &dependency.id,
                &self.plugin.id,
                installed,
                &mut seen,
                &mut path,
            ) {
                return Some(cycle);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, dependencies: &[&str]) -> PluginManifest {
        PluginManifest {
            format: 1,
            plugin: PluginInfo {
                id: id.into(),
                version: "0.1.0".into(),
                provider: id.into(),
                description: id.into(),
                repository: format!("https://github.com/example/{id}"),
                license: "MIT".into(),
            },
            memcastle: ">=0.4".into(),
            dependencies: dependencies
                .iter()
                .map(|id| PluginDependency {
                    id: (*id).to_string(),
                    version: ">=0.1.0".into(),
                    optional: false,
                })
                .collect(),
            shared_config_schema: None,
            authentication: None,
            modules: Vec::new(),
        }
    }

    #[test]
    fn a_plugin_update_cannot_complete_a_dependency_cycle() {
        let now = Utc::now();
        let installed = vec![PluginRecord {
            id: "second".into(),
            manifest: manifest("second", &["first"]),
            digest: String::new(),
            generation: String::new(),
            upstream: None,
            signed_by: None,
            installed_at: now,
            updated_at: now,
        }];
        assert_eq!(
            manifest("first", &["second"]).dependency_cycle(&installed),
            Some(vec!["first".into(), "second".into(), "first".into()])
        );
        assert_eq!(manifest("first", &[]).dependency_cycle(&installed), None);
    }
}
