//! Which sources exist, and how a provider name becomes an adapter.
//!
//! There are two kinds of source behind the one [`SourceAdapter`] contract (docs/adr/026): the adapters compiled
//! into MemCastle (`directory`, `pi-sessions`), and the packages a user installed, each a WebAssembly component. A
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
use super::adapters::pi_sessions::{self, PiSessionsAdapter};
use super::wasm::WasmAdapter;
use super::{ProviderInfo, SourceOrigin};

/// The names an installed source may not take: they are the built-in adapters', and a package called `directory`
/// would otherwise silently replace what `memcastle mine <path>` means.
pub const BUILTIN_NAMES: [&str; 2] = [directory::PROVIDER, pi_sessions::PROVIDER];

/// A source, whichever kind it is.
///
/// A closed enum rather than a `dyn` object: the adapter trait's `impl Future` returns keep dispatch static, and
/// there are exactly two kinds.
pub enum AnySource {
    /// The `directory` adapter.
    Directory(DirectoryAdapter),
    /// The `pi-sessions` adapter.
    PiSessions(PiSessionsAdapter),
    /// An installed WebAssembly source.
    Wasm(WasmAdapter),
}

impl SourceAdapter for AnySource {
    fn provider(&self) -> &str {
        match self {
            Self::Directory(a) => a.provider(),
            Self::PiSessions(a) => a.provider(),
            Self::Wasm(a) => a.provider(),
        }
    }

    fn description(&self) -> &str {
        match self {
            Self::Directory(a) => a.description(),
            Self::PiSessions(a) => a.description(),
            Self::Wasm(a) => a.description(),
        }
    }

    fn capabilities(&self) -> SourceCapabilities {
        match self {
            Self::Directory(a) => a.capabilities(),
            Self::PiSessions(a) => a.capabilities(),
            Self::Wasm(a) => a.capabilities(),
        }
    }

    fn identify(&self, locator: Option<&str>) -> Result<SourceRef> {
        match self {
            Self::Directory(a) => a.identify(locator),
            Self::PiSessions(a) => a.identify(locator),
            Self::Wasm(a) => a.identify(locator),
        }
    }

    fn default_wing(&self, source: &SourceRef) -> String {
        match self {
            Self::Directory(a) => a.default_wing(source),
            Self::PiSessions(a) => a.default_wing(source),
            Self::Wasm(a) => a.default_wing(source),
        }
    }

    fn default_room(&self) -> &str {
        match self {
            Self::Directory(a) => a.default_room(),
            Self::PiSessions(a) => a.default_room(),
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
            Self::PiSessions(a) => a.discover(source, cursor, limit).await,
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
            Self::PiSessions(a) => a.read(source, candidate).await,
            Self::Wasm(a) => a.read(source, candidate).await,
        }
    }

    fn normalize(&self, raw: &RawDocument) -> Result<crate::domain::CanonicalDocument> {
        match self {
            Self::Directory(a) => a.normalize(raw),
            Self::PiSessions(a) => a.normalize(raw),
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
    let pi = PiSessionsAdapter::new(Some(Path::new("/")));
    vec![describe(&directory), describe(&pi)]
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
        format!("unknown source `{provider}`; known sources: {known}"),
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
        pi_sessions::PROVIDER => Ok(AnySource::PiSessions(PiSessionsAdapter::new(
            mining.pi_sessions_dir.as_deref(),
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
