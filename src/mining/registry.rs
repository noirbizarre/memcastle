//! Which sources exist, and how a provider name becomes an adapter.
//!
//! There are two kinds of source behind the one [`SourceAdapter`] contract (docs/adr/026): the adapters compiled
//! into MemCastle (`directory`), and the packages a user installed, each a WebAssembly component. A
//! provider name is looked up here and nowhere else; the pipeline is handed an [`AnySource`] and cannot tell which
//! kind it got.
//!
//! The registry reads which packages are installed and in what state, but never writes: installing, enabling and
//! removing are `crate::app`'s, and the source and document records are the pipeline's.

use std::path::Path;

use crate::config::MiningConfig;
use crate::domain::{
    Cursor, Permissions, RawDocument, SourceCapabilities, SourcePackageRecord, SourcePackageState,
    SourceRef, SourceState, sha256_hex,
};
use crate::error::{Error, Result};
use crate::source::manifest::check_compatible;
use crate::source::package::read_installed_component;
use crate::store::SurrealStore;

use super::adapter::{Discovery, SourceAdapter};
use super::adapters::directory::{self, DirectoryAdapter};
use super::wasm::WasmAdapter;
use super::{ProviderInfo, SourceOrigin};

/// The names an installed source may not take: they are the built-in adapters', and a package called `directory`
/// would otherwise silently replace what `memcastle mine <path>` means.
pub const BUILTIN_NAMES: [&str; 1] = [directory::PROVIDER];

/// A source, whichever kind it is.
///
/// A closed enum rather than a `dyn` object: the adapter trait's `impl Future` returns keep dispatch static, and
/// there are exactly two kinds.
pub enum AnySource {
    /// The `directory` adapter.
    Directory(DirectoryAdapter),
    /// An installed WebAssembly source.
    Wasm(WasmAdapter),
}

impl SourceAdapter for AnySource {
    fn provider(&self) -> &str {
        match self {
            Self::Directory(a) => a.provider(),
            Self::Wasm(a) => a.provider(),
        }
    }

    fn description(&self) -> &str {
        match self {
            Self::Directory(a) => a.description(),
            Self::Wasm(a) => a.description(),
        }
    }

    fn capabilities(&self) -> SourceCapabilities {
        match self {
            Self::Directory(a) => a.capabilities(),
            Self::Wasm(a) => a.capabilities(),
        }
    }

    fn identify(&self, locator: Option<&str>) -> Result<SourceRef> {
        match self {
            Self::Directory(a) => a.identify(locator),
            Self::Wasm(a) => a.identify(locator),
        }
    }

    fn default_wing(&self, source: &SourceRef) -> String {
        match self {
            Self::Directory(a) => a.default_wing(source),
            Self::Wasm(a) => a.default_wing(source),
        }
    }

    fn default_room(&self) -> &str {
        match self {
            Self::Directory(a) => a.default_room(),
            Self::Wasm(a) => a.default_room(),
        }
    }

    async fn discover(
        &self,
        source: &SourceRef,
        cursor: &Cursor,
        limit: usize,
    ) -> Result<Discovery> {
        match self {
            Self::Directory(a) => a.discover(source, cursor, limit).await,
            Self::Wasm(a) => a.discover(source, cursor, limit).await,
        }
    }

    async fn read(
        &self,
        source: &SourceRef,
        candidate: &crate::domain::Candidate,
    ) -> Result<Option<RawDocument>> {
        match self {
            Self::Directory(a) => a.read(source, candidate).await,
            Self::Wasm(a) => a.read(source, candidate).await,
        }
    }

    fn normalize(&self, raw: &RawDocument) -> Result<crate::domain::CanonicalDocument> {
        match self {
            Self::Directory(a) => a.normalize(raw),
            Self::Wasm(a) => a.normalize(raw),
        }
    }
}

fn describe(adapter: &impl SourceAdapter) -> ProviderInfo {
    ProviderInfo {
        name: adapter.provider().to_string(),
        description: adapter.description().to_string(),
        capabilities: adapter.capabilities(),
        origin: SourceOrigin::Builtin,
        version: None,
        state: SourceState::Enabled,
        unavailable_reason: None,
        permissions: Permissions::default(),
    }
}

/// The adapters compiled into MemCastle, in the order they are listed.
#[must_use]
pub fn builtin_providers() -> Vec<ProviderInfo> {
    // Capabilities and descriptions do not depend on configuration, so default-configured adapters answer.
    let directory = DirectoryAdapter::new(0);
    vec![describe(&directory)]
}

/// Why an installed source cannot run here, or `None` when it can.
///
/// Computed on every look rather than stored: it depends on the files on disk and on the running MemCastle, either of
/// which can change without the database knowing.
#[must_use]
pub fn unavailable_reason(record: &SourcePackageRecord, sources_dir: &Path) -> Option<String> {
    if let Err(error) = check_compatible(&record.manifest) {
        return Some(match error {
            Error::SourceIncompatible { reason, .. } => reason,
            other => other.to_string(),
        });
    }
    match read_installed_component(sources_dir, &record.name) {
        Err(_) => Some(format!(
            "its component file is missing from {}; install the package again",
            sources_dir.join(&record.name).display()
        )),
        Ok(bytes) if sha256_hex(&bytes) != record.digest => Some(
            "its component file no longer matches what was installed; install the package again"
                .to_string(),
        ),
        Ok(_) => None,
    }
}

/// How an installed source is described to users.
#[must_use]
pub fn describe_package(record: &SourcePackageRecord, sources_dir: &Path) -> ProviderInfo {
    let unavailable_reason = unavailable_reason(record, sources_dir);
    ProviderInfo {
        name: record.name.clone(),
        description: record.manifest.source.description.clone(),
        capabilities: record.manifest.capabilities,
        origin: SourceOrigin::Package,
        version: Some(record.manifest.source.version.clone()),
        state: if unavailable_reason.is_some() {
            SourceState::Unavailable
        } else {
            record.state.into()
        },
        unavailable_reason,
        permissions: record.manifest.permissions.normalized(),
    }
}

/// Every source this daemon knows: the built-in ones, then the installed ones by name, each with its state.
///
/// # Errors
///
/// A store error when the installed sources cannot be read.
pub async fn providers(store: &SurrealStore, mining: &MiningConfig) -> Result<Vec<ProviderInfo>> {
    let dir = mining.sources_dir();
    let mut all = builtin_providers();
    for record in store.list_source_packages().await? {
        all.push(describe_package(&record, &dir));
    }
    Ok(all)
}

/// Check that `provider` can be mined right now, without loading it.
///
/// What submission uses to refuse a job up front, so that a typo or a disabled source is a 4xx at the request and not
/// a job that fails once it starts.
///
/// # Errors
///
/// [`Error::InvalidInput`] naming the known sources when there is no such source, and [`Error::SourceNotEnabled`]
/// when it exists but is disabled or unavailable.
pub async fn ensure_minable(
    store: &SurrealStore,
    mining: &MiningConfig,
    provider: &str,
) -> Result<()> {
    if BUILTIN_NAMES.contains(&provider) {
        return Ok(());
    }
    match store.get_source_package(provider).await? {
        Some(record) => minable_package(&record, &mining.sources_dir()),
        None => Err(unknown(store, mining, provider).await),
    }
}

fn minable_package(record: &SourcePackageRecord, sources_dir: &Path) -> Result<()> {
    if let Some(reason) = unavailable_reason(record, sources_dir) {
        return Err(Error::SourceNotEnabled {
            name: record.name.clone(),
            state: format!("unavailable: {reason}"),
        });
    }
    match record.state {
        SourcePackageState::Enabled => Ok(()),
        other => Err(Error::SourceNotEnabled {
            name: record.name.clone(),
            state: SourceState::from(other).to_string(),
        }),
    }
}

async fn unknown(store: &SurrealStore, mining: &MiningConfig, provider: &str) -> Error {
    let known = providers(store, mining)
        .await
        .map(|all| {
            all.into_iter()
                .map(|p| p.name)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|_| BUILTIN_NAMES.join(", "));
    Error::invalid_input(
        "source",
        format!(
            "unknown source `{provider}`; known sources: {known} (install one with `memcastle source install <package>`)"
        ),
    )
}

/// The adapter for `provider`: a built-in one, or an installed source that is enabled and intact.
///
/// # Errors
///
/// As [`ensure_minable`], and [`Error::SourceIncompatible`] or [`Error::SourceFailed`] when the component does not
/// load.
pub async fn resolve(
    store: &SurrealStore,
    mining: &MiningConfig,
    provider: &str,
) -> Result<AnySource> {
    match provider {
        directory::PROVIDER => Ok(AnySource::Directory(DirectoryAdapter::new(
            mining.max_file_bytes,
        ))),
        other => {
            let Some(record) = store.get_source_package(other).await? else {
                return Err(unknown(store, mining, other).await);
            };
            let dir = mining.sources_dir();
            minable_package(&record, &dir)?;
            let bytes = read_installed_component(&dir, &record.name)?;
            let manifest = record.manifest;
            let mining = mining.clone();
            // Compiling a component is CPU-bound and takes noticeable time on first use.
            let adapter =
                tokio::task::spawn_blocking(move || WasmAdapter::load(&manifest, &bytes, &mining))
                    .await
                    .map_err(|e| Error::SourceFailed {
                        name: other.to_string(),
                        message: format!("loading was interrupted: {e}"),
                    })??;
            Ok(AnySource::Wasm(adapter))
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::domain::{Compatibility, ManifestSource, SourceManifest};

    fn record(contract: &str) -> SourcePackageRecord {
        let now = Utc::now();
        SourcePackageRecord {
            name: "demo".to_string(),
            state: SourcePackageState::Enabled,
            digest: sha256_hex(b"component"),
            manifest: SourceManifest {
                source: ManifestSource {
                    name: "demo".to_string(),
                    version: "1.0.0".to_string(),
                    description: "demo".to_string(),
                },
                compatibility: Compatibility {
                    contract: contract.to_string(),
                    memcastle: ">=0.1".to_string(),
                },
                capabilities: SourceCapabilities::default(),
                permissions: Permissions::default(),
                limits: Default::default(),
                build: None,
                test: None,
            },
            installed_at: now,
            updated_at: now,
        }
    }

    fn install_component(dir: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(dir.join("demo")).unwrap();
        std::fs::write(dir.join("demo/source.wasm"), bytes).unwrap();
    }

    #[test]
    fn an_intact_compatible_package_has_no_reason_to_be_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        install_component(dir.path(), b"component");
        assert_eq!(unavailable_reason(&record("0.1"), dir.path()), None);
        let described = describe_package(&record("0.1"), dir.path());
        assert_eq!(described.state, SourceState::Enabled);
        assert_eq!(described.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn a_package_for_another_contract_is_unavailable_and_the_reason_is_the_incompatibility() {
        let dir = tempfile::tempdir().unwrap();
        install_component(dir.path(), b"component");
        let reason = unavailable_reason(&record("0.9"), dir.path()).unwrap();
        assert!(reason.contains("contract 0.9"), "{reason}");
        assert_eq!(
            describe_package(&record("0.9"), dir.path()).state,
            SourceState::Unavailable
        );
    }

    #[test]
    fn a_package_whose_component_is_missing_or_altered_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let missing = unavailable_reason(&record("0.1"), dir.path()).unwrap();
        assert!(missing.contains("missing"), "{missing}");

        install_component(dir.path(), b"something else");
        let altered = unavailable_reason(&record("0.1"), dir.path()).unwrap();
        assert!(altered.contains("no longer matches"), "{altered}");
    }

    #[test]
    fn a_disabled_package_is_not_minable_and_the_state_is_named() {
        let dir = tempfile::tempdir().unwrap();
        install_component(dir.path(), b"component");
        let mut disabled = record("0.1");
        disabled.state = SourcePackageState::Disabled;
        let error = minable_package(&disabled, dir.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("disabled"), "{error}");
        assert!(minable_package(&record("0.1"), dir.path()).is_ok());
        let unavailable = minable_package(&record("0.9"), dir.path())
            .unwrap_err()
            .to_string();
        assert!(unavailable.contains("unavailable"), "{unavailable}");
    }

    #[test]
    fn a_built_in_source_answers_for_its_name_description_and_default_room_like_the_adapter_does() {
        let directory = AnySource::Directory(DirectoryAdapter::new(1));
        assert_eq!(directory.provider(), "directory");
        assert!(!directory.description().is_empty());
        assert_eq!(directory.default_room(), "files");
    }
}
