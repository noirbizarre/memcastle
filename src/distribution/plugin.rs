//! Plugin discovery and artifact integrity checks over the existing location/trust transport.

use crate::domain::{
    IndexedPlugin, IndexedPluginVersion, PluginIndex, is_valid_source_name, sha256_hex,
};
use crate::error::{Error, Result};

use super::{GitHubApi, Location, MAX_ARCHIVE_BYTES, MAX_INDEX_BYTES, TrustPolicy, github};

/// Plugin index filename, intentionally different from the format-1 source registry.
pub const INDEX_FILE: &str = "plugins.json";

/// A fetched plugin version with a verified archive digest.
pub struct FetchedPlugin {
    /// Complete plugin archive; package validation is the application layer's responsibility.
    pub archive: Vec<u8>,
    /// SHA-256 asserted by the index or the GitHub release asset.
    pub digest: String,
    /// Verified signer ID, when a configured trust key signed the release.
    pub signed_by: Option<String>,
}

/// One catalogue with versions resolved from GitHub releases if necessary.
pub struct PluginRegistry {
    /// The location this index was loaded from.
    pub label: String,
    /// Validated plugin metadata.
    pub index: PluginIndex,
    base: Location,
}

fn unavailable(label: &str, message: impl Into<String>) -> Error {
    Error::PluginRegistryUnavailable {
        location: label.to_string(),
        reason: message.into(),
    }
}

impl PluginRegistry {
    /// Whether `label` is an HTTPS GitHub repository URL rather than a catalogue.
    pub fn repository_url(label: &str) -> Option<String> {
        crate::domain::plugin_repository_url(label)
    }

    /// Discover a single plugin repository directly, without adding it to a curated catalogue.
    ///
    /// Its GitHub release asset digest is still mandatory, but the repository is not a trust endorsement.
    pub async fn load_location(label: &str, github_api: &GitHubApi<'_>) -> Result<Self> {
        let Some(repository) = Self::repository_url(label) else {
            return Self::load(label).await;
        };
        let releases = super::read_releases(github_api, &repository)
            .await
            .map_err(|e| unavailable(label, e))?;
        let mut names = std::collections::HashSet::new();
        for release in releases
            .iter()
            .filter(|release| !release.draft && !release.prerelease)
        {
            for asset in &release.assets {
                let Some(stem) = asset.name.strip_suffix(".tar.gz") else {
                    continue;
                };
                if let Some((id, _version)) = stem.match_indices('-').find_map(|(at, _)| {
                    let (id, suffix) = stem.split_at(at);
                    let version = suffix.strip_prefix('-')?;
                    (is_valid_source_name(id) && semver::Version::parse(version).is_ok())
                        .then_some((id, version))
                }) {
                    names.insert(id.to_string());
                }
            }
        }
        if names.len() != 1 {
            return Err(unavailable(
                label,
                format!(
                    "expected release assets for exactly one plugin ID, found {}",
                    names.len()
                ),
            ));
        }
        let name = names.into_iter().next().expect("checked one ID");
        let (versions, _) = github::versions_from_releases(&name, &releases);
        let versions: Vec<_> = versions
            .into_iter()
            .map(|version| IndexedPluginVersion {
                version: version.version,
                url: version.url,
                sha256: version.sha256,
                signature: None,
                modules: None,
                yanked: version.yanked,
            })
            .collect();
        let base = Location::parse(label).map_err(|e| unavailable(label, e))?;
        Ok(Self {
            label: label.to_string(),
            index: PluginIndex {
                format: crate::domain::PLUGIN_INDEX_FORMAT,
                name: format!("Direct repository {repository}"),
                plugins: vec![IndexedPlugin {
                    id: name,
                    description: format!("Plugin from {repository}"),
                    repository,
                    modules: Vec::new(),
                    versions,
                }],
            },
            base,
        })
    }
    /// Load plugin entries without fetching code; static catalogues and GitHub release lists are supported.
    ///
    /// # Errors
    ///
    /// Fails when the index cannot be read or violates its versioned schema.
    pub async fn load(label: &str) -> Result<Self> {
        let base = Location::parse(label)
            .map_err(|e| unavailable(label, e))?
            .as_index(INDEX_FILE);
        let bytes = base
            .read(MAX_INDEX_BYTES)
            .await
            .map_err(|e| unavailable(label, e))?;
        let text = String::from_utf8(bytes).map_err(|_| unavailable(label, "not UTF-8"))?;
        let index = PluginIndex::parse(&text).map_err(|e| unavailable(label, e))?;
        Ok(Self {
            label: label.to_string(),
            index,
            base,
        })
    }

    /// Fill in the versions of a repository entry using GitHub's digest-bearing release assets.
    ///
    /// # Errors
    ///
    /// Fails when the release API cannot be read; other catalogue entries remain usable.
    pub async fn versions(
        &self,
        plugin: &IndexedPlugin,
        github_api: &GitHubApi<'_>,
    ) -> Result<Vec<IndexedPluginVersion>> {
        if !plugin.versions.is_empty() {
            return Ok(plugin.versions.clone());
        }
        let releases = super::read_releases(github_api, &plugin.repository)
            .await
            .map_err(|e| unavailable(&plugin.repository, e))?;
        let (versions, _skipped) = github::versions_from_releases(&plugin.id, &releases);
        Ok(versions
            .into_iter()
            .map(|version| IndexedPluginVersion {
                version: version.version,
                url: version.url,
                sha256: version.sha256,
                signature: None,
                modules: None,
                yanked: version.yanked,
            })
            .collect())
    }

    /// Download a selected release; never return bytes that differ from its published digest.
    ///
    /// # Errors
    ///
    /// Fails when the location, digest or trust policy is not satisfied.
    pub async fn fetch(
        &self,
        plugin: &IndexedPlugin,
        version: &IndexedPluginVersion,
        trust: &TrustPolicy,
    ) -> Result<FetchedPlugin> {
        let location = self
            .base
            .join(&version.url)
            .map_err(|e| unavailable(&self.label, e))?;
        let archive = location
            .read(MAX_ARCHIVE_BYTES)
            .await
            .map_err(|e| unavailable(&self.label, e))?;
        let digest = sha256_hex(&archive);
        if digest != version.sha256 {
            return Err(Error::PluginIntegrity {
                name: plugin.id.clone(),
                reason: format!(
                    "{location} has SHA-256 {digest}, catalogue published {}",
                    version.sha256
                ),
            });
        }
        let signed_by = trust
            .check(&plugin.id, &archive, version.signature.as_ref())
            .map_err(|error| match error {
                Error::SourceUntrusted { name, message } => Error::PluginUntrusted {
                    name,
                    reason: message,
                },
                other => other,
            })?;
        Ok(FetchedPlugin {
            archive,
            digest,
            signed_by,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PluginsConfig, TrustMode};
    use crate::source::signing;

    #[tokio::test]
    async fn a_custom_directory_catalogue_is_read_without_touching_the_source_index() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("plugins.json"),
            include_bytes!("../../docs/plugins.json"),
        )
        .unwrap();
        let registry = PluginRegistry::load(directory.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(registry.index.plugins.is_empty());
        assert_eq!(registry.index.format, 1);
    }

    #[tokio::test]
    async fn a_plugin_release_needs_the_published_digest_and_the_trusted_signature() {
        let directory = tempfile::tempdir().unwrap();
        let manifest = b"format = 1\nmemcastle = '>=0.4'\n[plugin]\nid = 'example'\nversion = '0.1.0'\nprovider = 'example'\ndescription = 'example'\nrepository = 'https://github.com/example/example'\nlicense = 'MIT'\n";
        let archive = crate::plugin::package::pack(&std::collections::BTreeMap::from([(
            "plugin.toml".to_string(),
            manifest.to_vec(),
        )]))
        .unwrap();
        let key = signing::generate().unwrap();
        let signature = signing::sign(&key, &archive);
        let name = "example-0.1.0.tar.gz";
        std::fs::write(directory.path().join(name), &archive).unwrap();
        let index = serde_json::json!({
            "format": 1, "name": "signed", "plugins": [{
                "id": "example", "description": "example", "repository": "example/example", "modules": [],
                "versions": [{"version": "0.1.0", "url": name, "sha256": sha256_hex(&archive), "signature": signature}],
            }],
        });
        std::fs::write(directory.path().join("plugins.json"), index.to_string()).unwrap();
        let registry = PluginRegistry::load(directory.path().to_str().unwrap())
            .await
            .unwrap();
        let policy = TrustPolicy::from_plugins(&PluginsConfig {
            trust: TrustMode::Required,
            trusted_keys: vec![signing::public_key_text(&key.verifying_key())],
            ..PluginsConfig::default()
        })
        .unwrap();
        let package = &registry.index.plugins[0];
        let version = &package.versions[0];
        let fetched = registry.fetch(package, version, &policy).await.unwrap();
        assert_eq!(
            fetched.signed_by,
            Some(signing::key_id(&key.verifying_key()))
        );
        let mut unsigned = version.clone();
        unsigned.signature = None;
        assert!(matches!(
            registry.fetch(package, &unsigned, &policy).await,
            Err(Error::PluginUntrusted { .. })
        ));
        std::fs::write(directory.path().join(name), b"tampered").unwrap();
        assert!(matches!(
            registry.fetch(package, version, &policy).await,
            Err(Error::PluginIntegrity { .. })
        ));
    }
}
