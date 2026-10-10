//! A curated plugin catalogue, distinct from the format-1 source index.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{IndexSignature, PluginManifest, PluginModuleKind, is_valid_source_name};

/// The version of the plugin catalogue schema.
pub const PLUGIN_INDEX_FORMAT: u32 = 1;

/// A direct GitHub repository URL, normalised to `owner/repository` without fetching it.
#[must_use]
pub fn plugin_repository_url(label: &str) -> Option<String> {
    let path = label
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    super::source_index::is_repository(path).then(|| path.to_string())
}

/// The catalogue is a discovery document, not a trust assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIndex {
    /// Versioned schema discriminator.
    pub format: u32,
    /// Human-readable name of this catalogue.
    pub name: String,
    /// Repository-scoped packages, never individual module archives.
    pub plugins: Vec<IndexedPlugin>,
}

/// One repository, with its reviewed module inventory for discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedPlugin {
    /// Stable installable ID.
    pub id: String,
    /// Human-readable summary.
    pub description: String,
    /// GitHub `owner/repository` whose release assets contain complete plugin packages.
    pub repository: String,
    /// Modules visible before an archive is installed.
    pub modules: Vec<IndexedPluginModule>,
    /// Versions in a static index; an empty list means GitHub releases are consulted.
    #[serde(default)]
    pub versions: Vec<IndexedPluginVersion>,
}

/// Public module identity/type; activation is never determined by this list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedPluginModule {
    /// Stable module ID.
    pub id: String,
    /// Source, agent integration or (later) embedding provider.
    #[serde(rename = "type")]
    pub kind: PluginModuleKind,
}

/// Verified artifact for one plugin release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedPluginVersion {
    /// Semver package release.
    pub version: String,
    /// Absolute URL, or URL relative to the catalogue.
    pub url: String,
    /// SHA-256 digest of the entire release archive.
    pub sha256: String,
    /// Optional ed25519 signature over the complete archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<IndexSignature>,
    /// Version-specific inventory, for older releases whose modules changed since publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modules: Option<Vec<IndexedPluginModule>>,
    /// Release deliberately withdrawn from automatic selection.
    #[serde(default)]
    pub yanked: bool,
}

impl PluginIndex {
    /// Parse and validate a catalogue before exposing any of its entries.
    ///
    /// # Errors
    ///
    /// A sentence naming the first malformed field.
    pub fn parse(text: &str) -> Result<Self, String> {
        let index: Self =
            serde_json::from_str(text).map_err(|e| format!("invalid plugin index: {e}"))?;
        index.validate()?;
        Ok(index)
    }

    /// Reject ambiguous identities and malformed versions.
    ///
    /// # Errors
    ///
    /// A sentence naming the first malformed field.
    pub fn validate(&self) -> Result<(), String> {
        if self.format != PLUGIN_INDEX_FORMAT {
            return Err(format!(
                "plugin index format {} is unsupported",
                self.format
            ));
        }
        let mut ids = HashSet::new();
        for plugin in &self.plugins {
            if !is_valid_source_name(&plugin.id) || !ids.insert(&plugin.id) {
                return Err(format!("invalid or duplicated plugin ID `{}`", plugin.id));
            }
            if plugin.description.trim().is_empty()
                || !super::source_index::is_repository(&plugin.repository)
            {
                return Err(format!(
                    "plugin `{}` needs a description and GitHub owner/repository",
                    plugin.id
                ));
            }
            let mut modules = HashSet::new();
            for module in &plugin.modules {
                if !is_valid_source_name(&module.id) || !modules.insert(&module.id) {
                    return Err(format!(
                        "plugin `{}` has invalid or duplicated module `{}`",
                        plugin.id, module.id
                    ));
                }
            }
            let mut versions = HashSet::new();
            for version in &plugin.versions {
                let parsed = semver::Version::parse(&version.version).map_err(|e| {
                    format!("plugin `{}` version `{}`: {e}", plugin.id, version.version)
                })?;
                if !versions.insert(parsed) || version.url.is_empty() {
                    return Err(format!(
                        "plugin `{}` has a duplicate version or empty URL",
                        plugin.id
                    ));
                }
                if version.sha256.len() != 64
                    || !version
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(format!(
                        "plugin `{}` version `{}` has an invalid SHA-256",
                        plugin.id, version.version
                    ));
                }
                if let Some(modules) = &version.modules {
                    let mut ids = HashSet::new();
                    for module in modules {
                        if !is_valid_source_name(&module.id) || !ids.insert(&module.id) {
                            return Err(format!(
                                "plugin `{}` version `{}` has invalid or duplicated module `{}`",
                                plugin.id, version.version, module.id
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Refuse an archive substituted for the one whose modules were reviewed.
    ///
    /// # Errors
    ///
    /// Names the identity or module metadata that differs from the catalogue.
    pub fn verify(
        &self,
        plugin: &IndexedPlugin,
        version: &IndexedPluginVersion,
        manifest: &PluginManifest,
        current: bool,
    ) -> Result<(), String> {
        if manifest.plugin.id != plugin.id
            || manifest.plugin.repository != format!("https://github.com/{}", plugin.repository)
        {
            return Err(format!(
                "plugin `{}` archive identity/repository differs from the catalogue",
                plugin.id
            ));
        }
        // The root inventory describes the current release. An older GitHub release has no per-version metadata
        // until a curator adds it, so its own verified archive remains the authority for that older inventory.
        if let Some(modules) = version
            .modules
            .as_deref()
            .or_else(|| current.then_some(plugin.modules.as_slice()))
        {
            let declared: HashSet<_> = manifest
                .modules
                .iter()
                .map(|module| (&module.id, module.kind))
                .collect();
            let indexed: HashSet<_> = modules
                .iter()
                .map(|module| (&module.id, module.kind))
                .collect();
            if declared != indexed {
                return Err(format!(
                    "plugin `{}` archive modules differ from the catalogue; review its module inventory",
                    plugin.id
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalogue_cannot_publish_the_same_module_twice() {
        let index = r#"{"format":1,"name":"test","plugins":[{"id":"acme","description":"test","repository":"acme/acme","modules":[{"id":"alpha","type":"source"},{"id":"alpha","type":"integration"}]}]}"#;
        assert!(
            PluginIndex::parse(index)
                .unwrap_err()
                .contains("duplicated module")
        );
    }

    #[test]
    fn an_empty_catalogue_does_not_change_the_legacy_source_registry_contract() {
        let text = include_str!("../../docs/plugins.json");
        let index = PluginIndex::parse(text).unwrap();
        assert!(index.plugins.is_empty());
        assert!(serde_json::from_str::<super::super::SourceIndex>(text).is_err());
    }

    #[test]
    fn a_pinned_older_release_uses_its_own_module_inventory() {
        let index = PluginIndex::parse(
            &serde_json::json!({
                "format": 1, "name": "example", "plugins": [{
                    "id": "acme", "description": "demo", "repository": "acme/acme",
                    "modules": [{"id": "new-source", "type": "source"}],
                    "versions": [{
                        "version": "0.1.0", "url": "acme-0.1.0.tar.gz", "sha256": "a".repeat(64),
                        "modules": [{"id": "old-source", "type": "source"}],
                    }],
                }],
            })
            .to_string(),
        )
        .unwrap();
        let manifest: PluginManifest = toml::from_str(&format!(
            "format = 1\nmemcastle = '>=0.4'\n[plugin]\nid = 'acme'\nversion = '0.1.0'\nprovider = 'acme'\ndescription = 'demo'\nrepository = 'https://github.com/acme/acme'\nlicense = 'MIT'\n[[modules]]\nid = 'old-source'\ntype = 'source'\nversion = '0.1.0'\nmanifest = 'modules/old-source/memcastle-source.toml'\nmanifest_sha256 = '{}'\nentry = 'modules/old-source/source.wasm'\nsha256 = '{}'\n", "b".repeat(64), "c".repeat(64)
        )).unwrap();
        assert!(
            index
                .verify(
                    &index.plugins[0],
                    &index.plugins[0].versions[0],
                    &manifest,
                    false
                )
                .is_ok()
        );
        let mut github_release = index.plugins[0].versions[0].clone();
        github_release.modules = None;
        assert!(
            index
                .verify(&index.plugins[0], &github_release, &manifest, false)
                .is_ok()
        );
        assert!(
            index
                .verify(&index.plugins[0], &github_release, &manifest, true)
                .is_err()
        );
    }
}
