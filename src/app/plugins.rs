//! Preflight plugin releases before any installation changes source or agent state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::distribution::plugin::PluginRegistry;
use crate::distribution::{GitHubApi, TrustPolicy};
use crate::domain::{
    PluginModuleKind, PluginRecord, SourceOrigin, SourcePackageRecord, SourcePackageState,
    sha256_hex,
};
use crate::error::{Error, Result};
use crate::mining::wasm::WasmAdapter;
use crate::plugin::package;

use super::AppServices;

/// Versioned metadata and explicit module permissions shown before an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPreview {
    /// Stable plugin name.
    pub id: String,
    /// Release version, independent of Core.
    pub version: String,
    /// Names and permission digests of source modules that need consent.
    pub source_consents: Vec<SourceConsent>,
    /// Historic legacy source IDs that need an explicit adoption decision before their cursors can be reused.
    pub adoptions: Vec<String>,
    /// Modules exposed by this release, without activating them.
    pub modules: Vec<crate::domain::PluginModule>,
}

/// A permission request belongs to one source module, never the whole provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConsent {
    /// Stable source ID.
    pub source: String,
    /// Permission digest needed at install.
    pub digest: String,
    /// What the host would grant.
    pub permissions: String,
    /// This source already belongs to the same plugin with precisely these permissions.
    pub already_granted: bool,
}

/// Reviewed plugin metadata available from configured catalogues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSearch {
    /// Packages that match the query, independently of their module activation state.
    pub plugins: Vec<crate::domain::IndexedPlugin>,
    /// An unavailable catalogue never hides other catalogue entries.
    pub warnings: Vec<String>,
}

/// Verified registry preview before the user agrees to any source module's permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRegistryPreview {
    /// What installing the package would make available.
    pub plugin: PluginPreview,
    /// Catalogue that supplied this release.
    pub registry: String,
    /// Verified archive digest.
    pub archive_digest: String,
    /// The accepted signature's key ID, if signed.
    pub signed_by: Option<String>,
}

/// Stage a validated archive at a path no running adapter uses until the database points at it.
fn stage_generation(
    root: &Path,
    id: &str,
    digest: &str,
    manifest: &crate::domain::PluginManifest,
    files: &BTreeMap<String, Vec<u8>>,
    archive: &[u8],
) -> Result<(PathBuf, bool)> {
    let parent = root.join(id);
    std::fs::create_dir_all(&parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
    if parent.is_symlink() {
        return Err(Error::PluginPackageInvalid {
            message: format!("plugin directory {} is a symlink", parent.display()),
        });
    }
    let destination = parent.join(digest);
    if destination.exists() {
        if destination.is_symlink()
            || !std::fs::read(destination.join("package.tar.gz"))
                .is_ok_and(|existing| existing == archive)
            || files.iter().any(|(name, bytes)| {
                !std::fs::read(destination.join(name)).is_ok_and(|existing| existing == *bytes)
            })
        {
            return Err(Error::PluginPackageInvalid {
                message: format!(
                    "generation {} differs from the verified archive; remove the damaged files",
                    destination.display()
                ),
            });
        }
        return Ok((destination, false));
    }
    let staging = parent.join(format!(".stage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging).map_err(|e| Error::io(staging.display().to_string(), e))?;
    let outcome = (|| {
        let archive_path = staging.join("package.tar.gz");
        std::fs::write(&archive_path, archive)
            .map_err(|e| Error::io(archive_path.display().to_string(), e))?;
        for (name, bytes) in files {
            let path = staging.join(name);
            if let Some(directory) = path.parent() {
                std::fs::create_dir_all(directory)
                    .map_err(|e| Error::io(directory.display().to_string(), e))?;
            }
            std::fs::write(&path, bytes).map_err(|e| Error::io(path.display().to_string(), e))?;
        }
        for module in manifest
            .modules
            .iter()
            .filter(|module| module.kind == PluginModuleKind::Integration)
        {
            let prefix = format!("modules/{}/", module.id);
            for (name, bytes) in files.iter().filter(|(name, _)| name.starts_with(&prefix)) {
                let relative = name.strip_prefix(&prefix).expect("checked prefix");
                let target = staging.join("integrations").join(&module.id).join(relative);
                if let Some(directory) = target.parent() {
                    std::fs::create_dir_all(directory)
                        .map_err(|e| Error::io(directory.display().to_string(), e))?;
                }
                std::fs::write(&target, bytes)
                    .map_err(|e| Error::io(target.display().to_string(), e))?;
            }
        }
        std::fs::rename(&staging, &destination)
            .map_err(|e| Error::io(destination.display().to_string(), e))?;
        Ok((destination, true))
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    outcome
}

fn publish_current(root: &Path, id: &str, digest: &str) -> Result<()> {
    let parent = root.join(id);
    let temporary = parent.join(format!(".current-{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, digest)
        .map_err(|e| Error::io(temporary.display().to_string(), e))?;
    let current = parent.join("current");
    std::fs::rename(&temporary, &current).map_err(|e| Error::io(current.display().to_string(), e))
}

impl AppServices {
    /// Rebuild local integration pointers from the committed plugin ledger after an interrupted install.
    pub async fn recover_plugin_pointers(&self) -> Result<()> {
        for plugin in self.store.list_plugins().await? {
            let root = self.plugins.dir();
            let generation = root.join(&plugin.id).join(&plugin.generation);
            if generation.is_dir() {
                publish_current(&root, &plugin.id, &plugin.generation)?;
            }
        }
        Ok(())
    }
    /// Install a plugin by catalogue ID after verifying the selected release and its module inventory.
    ///
    /// # Errors
    ///
    /// An index, trust, integrity, compatibility or permission-consent error leaves the installation unchanged.
    pub async fn install_registry_plugin(
        &self,
        id: &str,
        selected: Option<&str>,
        registry: Option<&str>,
        consents: BTreeMap<String, String>,
        adopt_sources: Vec<String>,
        expected_digest: Option<&str>,
    ) -> Result<PluginRecord> {
        let preview = self.preview_registry_plugin(id, selected, registry).await?;
        if expected_digest.is_some_and(|digest| digest != preview.archive_digest) {
            return Err(Error::PluginIntegrity {
                name: id.to_string(),
                reason: "the selected release changed since it was previewed; preview again before agreeing".into(),
            });
        }
        let catalogue =
            PluginRegistry::load_location(&preview.registry, &GitHubApi::from_config(&self.mining))
                .await?;
        let indexed = catalogue
            .index
            .plugins
            .iter()
            .find(|plugin| plugin.id == id)
            .ok_or_else(|| Error::PluginIntegrity {
                name: id.to_string(),
                reason: "the plugin disappeared from the selected catalogue".into(),
            })?;
        let versions = catalogue
            .versions(indexed, &GitHubApi::from_config(&self.mining))
            .await?;
        let version = versions
            .iter()
            .find(|version| version.version == preview.plugin.version)
            .ok_or_else(|| Error::PluginIntegrity {
                name: id.to_string(),
                reason: "the selected release disappeared from its repository".into(),
            })?;
        let fetched = catalogue
            .fetch(indexed, version, &TrustPolicy::from_plugins(&self.plugins)?)
            .await?;
        if fetched.digest != preview.archive_digest {
            return Err(Error::PluginIntegrity {
                name: id.to_string(),
                reason: "the release changed while installing; preview it again".into(),
            });
        }
        self.install_plugin_archive(
            fetched.archive,
            consents,
            adopt_sources,
            Some(preview.registry),
            fetched.signed_by,
        )
        .await
    }
    /// Fetch and verify an explicitly requested registry release without installing it.
    ///
    /// A missing name is checked in the next catalogue; an entry that publishes mismatching bytes is an integrity
    /// error, never bypassed by trying a less trusted catalogue. No registry is contacted at daemon startup.
    pub async fn preview_registry_plugin(
        &self,
        id: &str,
        selected: Option<&str>,
        registry: Option<&str>,
    ) -> Result<PluginRegistryPreview> {
        let locations: Vec<&str> = registry.map_or_else(
            || self.plugins.registries.iter().map(String::as_str).collect(),
            |only| vec![only],
        );
        let github = GitHubApi::from_config(&self.mining);
        let trust = TrustPolicy::from_plugins(&self.plugins)?;
        let mut warnings = Vec::new();
        let mut loaded = false;
        for location in locations {
            let catalogue = match PluginRegistry::load_location(location, &github).await {
                Ok(catalogue) => catalogue,
                Err(error) => {
                    warnings.push(error.to_string());
                    continue;
                }
            };
            loaded = true;
            let Some(plugin) = catalogue
                .index
                .plugins
                .iter()
                .find(|plugin| plugin.id == id)
            else {
                continue;
            };
            let mut versions = catalogue.versions(plugin, &github).await?;
            versions.sort_by(|left, right| {
                semver::Version::parse(&right.version)
                    .expect("validated index version")
                    .cmp(&semver::Version::parse(&left.version).expect("validated index version"))
            });
            let mut incompatible = Vec::new();
            for version in versions.iter().filter(|version| {
                selected.map_or(!version.yanked, |selected| selected == version.version)
            }) {
                let fetched = catalogue.fetch(plugin, version, &trust).await?;
                let package = package::unpack(&fetched.archive)?;
                if PluginRegistry::repository_url(location).is_some() {
                    if package.manifest.plugin.id != plugin.id
                        || package.manifest.plugin.repository
                            != format!("https://github.com/{}", plugin.repository)
                    {
                        return Err(Error::PluginIntegrity {
                            name: id.to_string(),
                            reason: "release manifest identity/repository differs from the requested GitHub repository".into(),
                        });
                    }
                } else {
                    catalogue
                        .index
                        .verify(
                            plugin,
                            version,
                            &package.manifest,
                            versions
                                .iter()
                                .find(|candidate| !candidate.yanked)
                                .is_some_and(|current| current.version == version.version),
                        )
                        .map_err(|reason| Error::PluginIntegrity {
                            name: id.to_string(),
                            reason,
                        })?;
                }
                if package.manifest.plugin.version != version.version {
                    return Err(Error::PluginIntegrity {
                        name: id.to_string(),
                        reason: format!(
                            "catalogue version {} differs from archive version {}",
                            version.version, package.manifest.plugin.version
                        ),
                    });
                }
                let host =
                    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo version");
                let supports = semver::VersionReq::parse(&package.manifest.memcastle)
                    .map_err(|error| Error::PluginManifestInvalid {
                        message: error.to_string(),
                    })?
                    .matches(&host);
                if !supports {
                    incompatible.push(version.version.clone());
                    continue;
                }
                let preview = self.preview_plugin_archive(&fetched.archive).await?;
                return Ok(PluginRegistryPreview {
                    plugin: preview,
                    registry: location.to_string(),
                    archive_digest: fetched.digest,
                    signed_by: fetched.signed_by,
                });
            }
            return Err(Error::PluginNotInRegistry {
                name: id.to_string(),
                reason: format!(
                    "no compatible release matches {}; skipped incompatible versions: {} in {location}",
                    selected.unwrap_or("latest"),
                    incompatible.join(", ")
                ),
            });
        }
        if loaded || warnings.is_empty() {
            Err(Error::PluginNotInRegistry {
                name: id.to_string(),
                reason: "no configured catalogue lists it".into(),
            })
        } else {
            Err(Error::PluginRegistryUnavailable {
                location: registry.unwrap_or("configured catalogues").to_string(),
                reason: warnings.join("; "),
            })
        }
    }
    /// Inspect one installed package, including the modules it makes available.
    pub async fn show_plugin(&self, id: &str) -> Result<PluginRecord> {
        self.store
            .get_plugin(id)
            .await?
            .ok_or_else(|| Error::PluginNotFound {
                name: id.to_string(),
            })
    }

    /// Uninstall a package only when no source, integration, job or dependent plugin uses it.
    ///
    /// An uninstall never deletes source cursors, source documents or mined drawers. Dependencies, agent receipts
    /// and configured miners are checked before removing any component files or stored credential.
    ///
    /// # Errors
    ///
    /// [`Error::PluginBlocked`] names the first reference to resolve before retrying.
    pub async fn uninstall_plugin(&self, id: &str) -> Result<()> {
        let record = self.show_plugin(id).await?;
        for dependent in self.store.list_plugins().await? {
            if dependent.id != id
                && dependent
                    .manifest
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.id == id)
            {
                return Err(Error::PluginBlocked {
                    name: id.to_string(),
                    reason: format!("plugin `{}` still depends on it", dependent.id),
                });
            }
        }
        let source_names: Vec<String> = record
            .manifest
            .modules
            .iter()
            .filter(|module| module.kind == PluginModuleKind::Source)
            .map(|module| module.id.clone())
            .collect();
        for name in &source_names {
            if let Some(source) = self.store.get_source_package(name).await?
                && source.state == SourcePackageState::Enabled
            {
                return Err(Error::PluginBlocked {
                    name: id.to_string(),
                    reason: format!("source `{name}` is enabled; disable it first"),
                });
            }
        }
        for miner in self.miners.snapshot().await {
            if source_names.contains(&miner.source) {
                return Err(Error::PluginBlocked {
                    name: id.to_string(),
                    reason: format!(
                        "configured miner `{}` still references source `{}`",
                        miner.name, miner.source
                    ),
                });
            }
        }
        if self
            .store
            .active_mine_job_for_sources(&source_names)
            .await?
        {
            return Err(Error::PluginBlocked {
                name: id.to_string(),
                reason: "a queued, running or paused mining job still names one of its sources"
                    .to_string(),
            });
        }
        let agents = crate::config::paths::default_agents_dir();
        for module in record
            .manifest
            .modules
            .iter()
            .filter(|module| module.kind == PluginModuleKind::Integration)
        {
            if agents.join(&module.id).exists() {
                return Err(Error::PluginBlocked {
                    name: id.to_string(),
                    reason: format!(
                        "integration `{}` is installed for an agent; uninstall it first",
                        module.id
                    ),
                });
            }
        }
        for name in &source_names {
            self.forget_source_credential(name).await?;
        }
        self.store
            .delete_plugin_with_sources(id, &source_names)
            .await?;
        let dir = self.plugins.dir().join(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| Error::io(dir.display().to_string(), e))?;
        }
        Ok(())
    }
    /// Stage a complete plugin release, then publish all its source records in one transaction.
    ///
    /// The caller explicitly consents to the permissions of each source module. No source is enabled and no agent
    /// integration is installed by this operation; a failed transaction leaves only inert, unreferenced files.
    ///
    /// # Errors
    ///
    /// Refuses an incompatible module, missing consent, conflicting ownership or failed artifact/store write.
    pub async fn install_plugin_archive(
        &self,
        archive: Vec<u8>,
        consents: BTreeMap<String, String>,
        adopt_sources: Vec<String>,
        upstream: Option<String>,
        signed_by: Option<String>,
    ) -> Result<PluginRecord> {
        let preview = self.preview_plugin_archive(&archive).await?;
        let mut required = preview.adoptions.clone();
        required.sort();
        let mut accepted = adopt_sources;
        accepted.sort();
        if required != accepted {
            return Err(Error::PluginBlocked {
                name: preview.id.clone(),
                reason: format!(
                    "historic legacy source data needs explicit --adopt-source for: {}; requested: {}",
                    required.join(", "),
                    accepted.join(", ")
                ),
            });
        }
        for required in &preview.source_consents {
            if !required.already_granted && consents.get(&required.source) != Some(&required.digest)
            {
                return Err(Error::SourceConsentRequired {
                    name: required.source.clone(),
                    permissions: required.permissions.clone(),
                    digest: required.digest.clone(),
                });
            }
        }
        if let Some(name) = consents.keys().find(|name| {
            !preview
                .source_consents
                .iter()
                .any(|source| source.source == **name)
        }) {
            return Err(Error::invalid_input(
                "consents",
                format!("source `{name}` does not ask for consent in this release"),
            ));
        }
        let package = package::unpack(&archive)?;
        let digest = sha256_hex(&archive);
        let id = &package.manifest.plugin.id;
        let root = self.plugins.dir();
        let (generation, fresh) = stage_generation(
            &root,
            id,
            &digest,
            &package.manifest,
            &package.files,
            &archive,
        )?;
        let now = Utc::now();
        let previous = self.store.get_plugin(id).await?;
        let retired_sources: Vec<String> = previous
            .iter()
            .flat_map(|old| &old.manifest.modules)
            .filter(|old| {
                old.kind == PluginModuleKind::Source
                    && !package
                        .manifest
                        .modules
                        .iter()
                        .any(|current| current.id == old.id && current.kind == old.kind)
            })
            .map(|module| module.id.clone())
            .collect();
        let record = PluginRecord {
            id: id.clone(),
            manifest: package.manifest.clone(),
            digest: digest.clone(),
            generation: digest.clone(),
            upstream,
            signed_by,
            installed_at: previous.as_ref().map_or(now, |old| old.installed_at),
            updated_at: now,
        };
        let mut sources = Vec::new();
        for module in package
            .manifest
            .modules
            .iter()
            .filter(|module| module.kind == PluginModuleKind::Source)
        {
            let text = std::str::from_utf8(&package.files[&module.manifest])
                .expect("validated module manifest");
            let manifest =
                crate::source::manifest::parse(text, &crate::mining::registry::BUILTIN_NAMES)?;
            let old = self.store.get_source_package(&module.id).await?;
            sources.push(SourcePackageRecord {
                name: module.id.clone(),
                state: old
                    .as_ref()
                    .map_or(SourcePackageState::Installed, |record| record.state),
                digest: module.sha256.clone(),
                manifest,
                installed_at: old.as_ref().map_or(now, |record| record.installed_at),
                updated_at: now,
                origin: SourceOrigin::Plugin,
                registry: None,
                archive_digest: Some(digest.clone()),
                signed_by: record.signed_by.clone(),
                plugin: Some(id.clone()),
                generation: Some(digest.clone()),
            });
        }
        for name in &retired_sources {
            self.forget_source_credential(name).await?;
        }
        if let Err(error) = self
            .store
            .publish_plugin_with_sources(&record, &sources, &retired_sources)
            .await
        {
            if fresh {
                let _ = std::fs::remove_dir_all(&generation);
            }
            return Err(error);
        }
        // The local integration installer has no daemon dependency: publish a tiny on-disk pointer after the DB
        // switches. The daemon also reconciles it from the ledger at startup if a crash falls between these steps.
        publish_current(&root, id, &digest)?;
        Ok(record)
    }
    /// Search explicitly configured plugin registries without installing anything.
    pub async fn search_plugins(&self, query: &str, only: Option<&str>) -> PluginSearch {
        let mut plugins = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut warnings = Vec::new();
        let query = query.trim().to_ascii_lowercase();
        let locations: Vec<&str> = only.map_or_else(
            || self.plugins.registries.iter().map(String::as_str).collect(),
            |location| vec![location],
        );
        let github = GitHubApi::from_config(&self.mining);
        for location in locations {
            match PluginRegistry::load_location(location, &github).await {
                Ok(registry) => {
                    for plugin in registry.index.plugins {
                        if seen.insert(plugin.id.clone())
                            && (query.is_empty()
                                || plugin.id.contains(&query)
                                || plugin.description.to_ascii_lowercase().contains(&query)
                                || plugin
                                    .modules
                                    .iter()
                                    .any(|module| module.id.contains(&query)))
                        {
                            plugins.push(plugin);
                        }
                    }
                }
                Err(error) => warnings.push(format!("{location}: {error}")),
            }
        }
        PluginSearch { plugins, warnings }
    }
    /// List installed plugin packages, distinct from the enabled source list.
    ///
    /// # Errors
    ///
    /// A storage error when the installed ledger cannot be read.
    pub async fn list_plugins(&self) -> Result<Vec<PluginRecord>> {
        self.store.list_plugins().await
    }

    /// Validate a local or verified remote archive without modifying any installation state.
    ///
    /// # Errors
    ///
    /// Names an incompatible contract, missing dependency or module collision before writing files.
    pub async fn preview_plugin_archive(&self, archive: &[u8]) -> Result<PluginPreview> {
        let package = package::unpack(archive)?;
        let name = &package.manifest.plugin.id;
        let host = semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| {
            Error::PluginManifestInvalid {
                message: format!("host version: {e}"),
            }
        })?;
        let requirement = semver::VersionReq::parse(&package.manifest.memcastle).map_err(|e| {
            Error::PluginManifestInvalid {
                message: format!("host compatibility: {e}"),
            }
        })?;
        if !requirement.matches(&host) {
            return Err(Error::PluginBlocked {
                name: name.clone(),
                reason: format!(
                    "this release needs MemCastle {}, but the daemon runs {host}",
                    package.manifest.memcastle
                ),
            });
        }
        let installed = self.store.list_plugins().await?;
        if let Some(previous) = installed.iter().find(|previous| previous.id == *name) {
            let prior = semver::Version::parse(&previous.manifest.plugin.version).map_err(|e| {
                Error::PluginManifestInvalid {
                    message: e.to_string(),
                }
            })?;
            let incoming =
                semver::Version::parse(&package.manifest.plugin.version).map_err(|e| {
                    Error::PluginManifestInvalid {
                        message: e.to_string(),
                    }
                })?;
            if incoming < prior {
                return Err(Error::PluginBlocked {
                    name: name.clone(),
                    reason: format!(
                        "downgrading from {prior} to {incoming} can invalidate configured modules and cursors; choose a newer release"
                    ),
                });
            }
            if incoming == prior && previous.digest != sha256_hex(archive) {
                return Err(Error::PluginIntegrity {
                    name: name.clone(),
                    reason: format!(
                        "version {incoming} is already installed with different bytes; publish a new version instead"
                    ),
                });
            }
            for module in &previous.manifest.modules {
                if !package
                    .manifest
                    .modules
                    .iter()
                    .any(|candidate| candidate.id == module.id && candidate.kind == module.kind)
                {
                    match module.kind {
                        PluginModuleKind::Source => {
                            if self
                                .store
                                .get_source_package(&module.id)
                                .await?
                                .is_some_and(|source| source.state == SourcePackageState::Enabled)
                            {
                                return Err(Error::PluginBlocked {
                                    name: name.clone(),
                                    reason: format!(
                                        "removed source `{}` is enabled; disable it first",
                                        module.id
                                    ),
                                });
                            }
                            if let Some(miner) = self
                                .miners
                                .snapshot()
                                .await
                                .iter()
                                .find(|miner| miner.source == module.id)
                            {
                                return Err(Error::PluginBlocked {
                                    name: name.clone(),
                                    reason: format!(
                                        "removed source `{}` is still configured by miner `{}`",
                                        module.id, miner.name
                                    ),
                                });
                            }
                            if self
                                .store
                                .active_mine_job_for_sources(std::slice::from_ref(&module.id))
                                .await?
                            {
                                return Err(Error::PluginBlocked {
                                    name: name.clone(),
                                    reason: format!(
                                        "removed source `{}` still has an active job",
                                        module.id
                                    ),
                                });
                            }
                        }
                        PluginModuleKind::Integration => {
                            if crate::config::paths::default_agents_dir()
                                .join(&module.id)
                                .exists()
                            {
                                return Err(Error::PluginBlocked {
                                    name: name.clone(),
                                    reason: format!(
                                        "removed integration `{}` is still installed for an agent",
                                        module.id
                                    ),
                                });
                            }
                        }
                        PluginModuleKind::EmbeddingProvider => {
                            // #256 adds selection state; today this module is metadata only.
                        }
                    }
                }
            }
            let new_version =
                semver::Version::parse(&package.manifest.plugin.version).map_err(|e| {
                    Error::PluginManifestInvalid {
                        message: e.to_string(),
                    }
                })?;
            for dependent in &installed {
                for dependency in dependent
                    .manifest
                    .dependencies
                    .iter()
                    .filter(|dependency| dependency.id == *name)
                {
                    let required = semver::VersionReq::parse(&dependency.version).map_err(|e| {
                        Error::PluginManifestInvalid {
                            message: e.to_string(),
                        }
                    })?;
                    if !required.matches(&new_version) {
                        return Err(Error::PluginBlocked {
                            name: name.clone(),
                            reason: format!(
                                "installed plugin `{}` needs version {}; update it first",
                                dependent.id, dependency.version
                            ),
                        });
                    }
                }
            }
        }
        for dependency in &package.manifest.dependencies {
            let Some(other) = installed
                .iter()
                .find(|installed| installed.id == dependency.id)
            else {
                if dependency.optional {
                    continue;
                }
                return Err(Error::PluginBlocked {
                    name: name.clone(),
                    reason: format!(
                        "install dependency `{}` ({}) first",
                        dependency.id, dependency.version,
                    ),
                });
            };
            let requirement = semver::VersionReq::parse(&dependency.version).map_err(|e| {
                Error::PluginManifestInvalid {
                    message: e.to_string(),
                }
            })?;
            let version = semver::Version::parse(&other.manifest.plugin.version).map_err(|e| {
                Error::PluginManifestInvalid {
                    message: e.to_string(),
                }
            })?;
            if !requirement.matches(&version) {
                return Err(Error::PluginBlocked {
                    name: name.clone(),
                    reason: format!(
                        "dependency `{}` is {version}; this release needs {}",
                        dependency.id, dependency.version
                    ),
                });
            }
        }
        if let Some(cycle) = package.manifest.dependency_cycle(&installed) {
            return Err(Error::PluginBlocked {
                name: name.clone(),
                reason: format!(
                    "plugin dependencies would form a cycle: {}",
                    cycle.join(" → ")
                ),
            });
        }
        let mut source_consents = Vec::new();
        let mut adoptions = Vec::new();
        for module in &package.manifest.modules {
            if installed.iter().any(|other| {
                other.id != *name
                    && other
                        .manifest
                        .modules
                        .iter()
                        .any(|owned| owned.id == module.id)
            }) {
                return Err(Error::PluginBlocked {
                    name: name.clone(),
                    reason: format!(
                        "module `{}` is already owned by another installed plugin",
                        module.id
                    ),
                });
            }
            match module.kind {
                PluginModuleKind::Source => {
                    if self.store.get_source_package(&module.id).await?.is_some()
                        && !installed.iter().any(|owner| {
                            owner.id == *name
                                && owner
                                    .manifest
                                    .modules
                                    .iter()
                                    .any(|previous| previous.id == module.id)
                        })
                    {
                        return Err(Error::PluginBlocked {
                            name: name.clone(),
                            reason: format!(
                                "source `{}` is already installed from a legacy package or another plugin; remove or migrate it explicitly",
                                module.id
                            ),
                        });
                    }
                    if let Some(owner) = self.store.get_plugin_source_owner(&module.id).await?
                        && owner != *name
                    {
                        return Err(Error::PluginBlocked {
                            name: name.clone(),
                            reason: format!(
                                "source `{}` and its retained data belong to plugin `{owner}`",
                                module.id
                            ),
                        });
                    }
                    if self
                        .store
                        .get_plugin_source_owner(&module.id)
                        .await?
                        .is_none()
                        && self.store.has_mined_source_name(&module.id).await?
                    {
                        adoptions.push(module.id.clone());
                    }
                    let bytes = package
                        .files
                        .get(&module.manifest)
                        .expect("archive checked module manifest");
                    let source = crate::source::manifest::parse(
                        std::str::from_utf8(bytes).expect("archive checked UTF-8"),
                        &crate::mining::registry::BUILTIN_NAMES,
                    )?;
                    crate::source::manifest::check_compatible(&source)?;
                    let permissions = source.permissions.normalized();
                    if !permissions.is_empty() {
                        source_consents.push(SourceConsent {
                            source: module.id.clone(),
                            digest: permissions.consent_digest(&module.id),
                            permissions: permissions.describe(),
                            already_granted: self
                                .store
                                .get_source_package(&module.id)
                                .await?
                                .is_some_and(|old| {
                                    old.plugin.as_deref() == Some(name)
                                        && old.manifest.permissions.normalized() == permissions
                                }),
                        });
                    }
                    // Prove that every component can load before any one module is made available.
                    let component = package
                        .files
                        .get(&module.entry)
                        .expect("archive checked component");
                    let mining = self.mining.clone();
                    let credentials: std::sync::Arc<dyn crate::domain::AccessTokens> =
                        std::sync::Arc::new(self.credentials.clone());
                    let component = component.clone();
                    tokio::task::spawn_blocking(move || {
                        WasmAdapter::load_with(&source, &component, &mining, Some(credentials))
                    })
                    .await
                    .map_err(|e| Error::PluginPackageInvalid {
                        message: format!("module load interrupted: {e}"),
                    })?
                    .map_err(|error| Error::PluginPackageInvalid {
                        message: format!("source module `{}` cannot load: {error}", module.id),
                    })?;
                }
                PluginModuleKind::Integration => {
                    // The archive validator checks its own manifest and every referenced asset.
                }
                PluginModuleKind::EmbeddingProvider => {
                    // Its runtime belongs to #256; a manifest must not make it executable today.
                }
            }
        }
        Ok(PluginPreview {
            id: name.clone(),
            version: package.manifest.plugin.version,
            source_consents,
            adoptions,
            modules: package.manifest.modules,
        })
    }
}
