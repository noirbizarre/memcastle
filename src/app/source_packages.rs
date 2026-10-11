//! Installing, enabling, disabling and removing mining sources (docs/adr/026).
//!
//! These change what code the daemon may run, not what the palace remembers, so like the token and the database
//! endpoint they are administrative: REST and the CLI only, with no MCP tool, and not gated by a memory mode (which
//! guards access to memory). What is gated is *consent*: a package that asks for permissions is installed only when
//! the caller names the digest of exactly those permissions, which the CLI obtains by showing them to the user.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::domain::{
    IndexedVersion, SourceOrigin, SourcePackageEvent, SourcePackageRecord, SourcePackageState,
    SourceState,
};
use crate::error::{Error, Result};
use crate::mining::AdapterInfo;
use crate::mining::bundled::Bundle;
use crate::mining::registry::{
    BUILTIN_NAMES, component_dir, describe_installed, lookup, unavailable_reason,
};
use crate::mining::wasm::WasmAdapter;
use crate::source::manifest::check_compatible;
use crate::source::package;

use super::AppServices;

/// What installing a source did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSource {
    /// The source as it is now.
    pub source: AdapterInfo,
    /// Whether it replaced an earlier install of the same name.
    pub replaced: bool,
}

/// Where an archive came from, which is kept with the installed record so an update knows where to look.
#[derive(Debug, Clone)]
pub(super) struct Upstream {
    pub origin: SourceOrigin,
    pub registry: Option<String>,
    pub archive_digest: Option<String>,
    pub signed_by: Option<String>,
}

impl Upstream {
    /// An archive from the user's own disk: it has no upstream.
    pub(super) fn local() -> Self {
        Self {
            origin: SourceOrigin::Package,
            registry: None,
            archive_digest: None,
            signed_by: None,
        }
    }
}

impl AppServices {
    /// Install the package in `archive`.
    ///
    /// `consent` is the digest of the permissions the user agreed to ([`Permissions::consent_digest`]); a package
    /// that asks for none needs none. With `enable`, the source is turned on once installed. Installing a name that
    /// is already installed replaces it and keeps its state, so an upgrade does not silently disable a source in use.
    ///
    /// # Errors
    ///
    /// [`Error::SourcePackageInvalid`] or [`Error::SourceManifestInvalid`] for a bad package,
    /// [`Error::SourceIncompatible`] when it cannot run here, [`Error::SourceConsentRequired`] without the right
    /// consent, and store or I/O errors.
    pub async fn install_source_package(
        &self,
        archive: Vec<u8>,
        consent: Option<&str>,
        enable: bool,
    ) -> Result<InstalledSource> {
        self.install_archive(archive, consent, enable, Upstream::local(), None)
            .await
    }

    /// Read `archive` the way [`Self::install_source_package`] does, and say what it asks for, without installing it.
    pub(super) async fn read_archive(archive: Vec<u8>) -> Result<package::SourcePackage> {
        tokio::task::spawn_blocking(move || package::inspect(&archive))
            .await
            .map_err(|e| Error::SourcePackageInvalid {
                message: format!("reading the package was interrupted: {e}"),
            })?
    }

    /// The one way a source gets installed, whatever it came from: the checks are the same for a file the user
    /// chose, a bundled package and a registry download, and only `upstream` differs.
    ///
    /// `expected` is what an index said the archive was; the package inside must agree.
    pub(super) async fn install_archive(
        &self,
        archive: Vec<u8>,
        consent: Option<&str>,
        enable: bool,
        upstream: Upstream,
        expected: Option<(&str, &IndexedVersion)>,
    ) -> Result<InstalledSource> {
        let package = Self::read_archive(archive).await?;
        if let Some((name, entry)) = expected {
            crate::distribution::verify_identity(&package, name, entry)?;
        }
        let name = package.manifest.source.name.clone();
        check_compatible(&package.manifest)?;

        let existing = self.store.get_source_package(&name).await?;
        if let Some(plugin) = existing.as_ref().and_then(|record| record.plugin.as_ref()) {
            return Err(Error::PluginBlocked {
                name: plugin.clone(),
                reason: format!(
                    "source `{name}` belongs to this plugin; update its parent plugin rather than installing a standalone source"
                ),
            });
        }
        if let Some(plugin) = self.store.get_plugin_source_owner(&name).await? {
            return Err(Error::PluginBlocked {
                name: plugin,
                reason: format!(
                    "source `{name}` was supplied by this plugin; reinstall the parent plugin rather than a standalone package"
                ),
            });
        }

        let permissions = package.manifest.permissions.normalized();
        let digest = permissions.consent_digest(&name);
        if !permissions.is_empty() && consent != Some(digest.as_str()) {
            return Err(Error::SourceConsentRequired {
                name,
                permissions: permissions.describe(),
                digest,
            });
        }

        // Loading is what proves the component is one this MemCastle can run, so a package that would fail at its
        // first mine fails here, where whoever installed it is looking.
        let manifest = package.manifest.clone();
        let bytes = package.component.clone();
        let mining = self.mining.clone();
        let credentials: std::sync::Arc<dyn crate::domain::AccessTokens> =
            std::sync::Arc::new(self.credentials.clone());
        tokio::task::spawn_blocking(move || {
            WasmAdapter::load_with(&manifest, &bytes, &mining, Some(credentials))
        })
        .await
        .map_err(|e| Error::SourceFailed {
            name: name.clone(),
            message: format!("loading was interrupted: {e}"),
        })??;

        let sources_dir = self.mining.sources_dir();
        package::install(&sources_dir, &package)?;
        let now = Utc::now();
        let mut state = existing
            .as_ref()
            .map_or(SourcePackageState::Installed, |record| record.state);
        if enable && let Ok(next) = state.apply(SourcePackageEvent::Enable) {
            state = next;
        }
        let record = SourcePackageRecord {
            name,
            state,
            digest: package.digest,
            manifest: package.manifest,
            installed_at: existing.as_ref().map_or(now, |record| record.installed_at),
            updated_at: now,
            origin: upstream.origin,
            registry: upstream.registry,
            archive_digest: upstream.archive_digest,
            signed_by: upstream.signed_by,
            plugin: None,
            generation: None,
        };
        self.store.save_source_package(&record).await?;
        Ok(InstalledSource {
            source: self
                .with_sign_in(describe_installed(&record, &self.mining))
                .await,
            replaced: existing.is_some(),
        })
    }

    /// One source, built in or installed, as `memcastle source list` describes it.
    ///
    /// # Errors
    ///
    /// [`Error::SourceNotFound`] when there is no such source, and [`Error::ModeForbidden`] when `mode` forbids
    /// reads.
    pub async fn show_source(
        &self,
        name: &str,
        mode: crate::domain::MemoryMode,
    ) -> Result<AdapterInfo> {
        Self::require_read(mode, "source_show")?;
        if let Some(builtin) = crate::mining::registry::builtin_adapters()
            .into_iter()
            .find(|source| source.name == name)
        {
            return Ok(builtin);
        }
        let record = self.installed(name).await?;
        Ok(self
            .with_sign_in(describe_installed(&record, &self.mining))
            .await)
    }

    /// Turn the installed source `name` on or off.
    ///
    /// Idempotent: enabling an enabled source answers with it, unchanged. A source that is unavailable cannot be
    /// enabled, because it would not run; the error says why.
    ///
    /// A bundled source asks for no consent: it is as trusted as the MemCastle that ships it, so turning it on is the
    /// whole decision, and its permissions are shown by `memcastle source show`. (A package from a file or a registry
    /// was consented to when it was installed.)
    ///
    /// # Errors
    ///
    /// [`Error::SourceBuiltin`] for a built-in source, [`Error::SourceNotFound`], [`Error::SourceNotEnabled`] when
    /// enabling a source that is unavailable, and store errors.
    pub async fn set_source_enabled(&self, name: &str, enabled: bool) -> Result<AdapterInfo> {
        let record = self.installed(name).await?;
        let event = if enabled {
            SourcePackageEvent::Enable
        } else {
            SourcePackageEvent::Disable
        };
        let dir = component_dir(&record, &self.mining);
        if enabled && let Some(reason) = unavailable_reason(&record, &dir) {
            return Err(Error::SourceNotEnabled {
                name: record.name,
                state: format!("{}: {reason}", SourceState::Unavailable),
            });
        }
        let Ok(next) = record.state.apply(event) else {
            // Already where the event would put it: nothing to change.
            return Ok(self
                .with_sign_in(describe_installed(&record, &self.mining))
                .await);
        };
        let record = SourcePackageRecord {
            state: next,
            updated_at: Utc::now(),
            ..record
        };
        if record.origin == SourceOrigin::Bundled {
            // A bundled source has no row until the user first decides about it, and updating a row that is not there
            // would silently keep it `installed`; the upsert writes the state and nothing the bundle supplies later.
            self.store.save_source_package(&record).await?;
        } else {
            self.store
                .set_source_package_state(name, next, record.updated_at)
                .await?;
        }
        Ok(self
            .with_sign_in(describe_installed(&record, &self.mining))
            .await)
    }

    /// Remove the installed source `name`: its row and its files. What it mined stays in the palace.
    ///
    /// A bundled source cannot be removed, since it comes with MemCastle and would be back with the next start;
    /// disabling it is what turns it off. A package file installed over a bundled source can be removed, which brings
    /// the bundled one back.
    ///
    /// # Errors
    ///
    /// [`Error::SourceBuiltin`], [`Error::SourceBundled`], [`Error::SourceNotFound`], and store or I/O errors.
    pub async fn remove_source_package(&self, name: &str) -> Result<()> {
        let record = self.installed(name).await?;
        if let Some(plugin) = &record.plugin {
            return Err(Error::PluginBlocked {
                name: plugin.clone(),
                reason: format!(
                    "source `{name}` belongs to this plugin; disable it and uninstall the plugin instead"
                ),
            });
        }
        if record.origin == SourceOrigin::Bundled && self.is_bundled(name) {
            return Err(Error::SourceBundled {
                name: name.to_string(),
                action: "removed".to_string(),
            });
        }
        self.store.delete_source_package(name).await?;
        // A credential belongs to the installed source: one left behind would be picked up by whatever is installed
        // under the same name next, which may be a different publisher's.
        self.forget_source_credential(name).await?;
        package::remove(&self.mining.sources_dir(), name)
    }

    /// Whether this installation ships a source called `name`.
    pub(super) fn is_bundled(&self, name: &str) -> bool {
        Bundle::find(&self.mining).is_some_and(|bundle| bundle.contains(name))
    }

    /// The installed source `name`, refusing built-in names with their own error.
    ///
    /// A bundled source is installed from the start, so it is found here without ever having been installed.
    pub(super) async fn installed(&self, name: &str) -> Result<SourcePackageRecord> {
        if BUILTIN_NAMES.contains(&name) {
            return Err(Error::SourceBuiltin {
                name: name.to_string(),
            });
        }
        lookup(&self.store, &self.mining, name)
            .await?
            .ok_or_else(|| Error::SourceNotFound {
                name: name.to_string(),
            })
    }
}
