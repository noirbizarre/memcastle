//! The WebAssembly source runtime: a [`SourceAdapter`] whose methods run a component (docs/adr/026).
//!
//! [`WasmAdapter`] is the same logical contract as the built-in adapters, so the pipeline cannot tell them apart.
//! What this module adds is the isolation: a component runs in a fresh sandbox per call, with the filesystem, the
//! environment, the network and programs available only as far as its manifest was agreed to, a memory ceiling, and
//! a time limit enforced by the engine rather than by the source's good manners.
//!
//! This is the only module that names `wasmtime` (`tests/source_isolation.rs`), and like every adapter it never
//! touches the store or the jobs.

mod host;
mod process;

use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, Trap};

use crate::config::MiningConfig;
use crate::domain::{
    Candidate, CanonicalDocument, Cursor, Permissions, RawDocument, Segment, SourceCapabilities,
    SourceKind, SourceManifest, SourceRef, sha256_hex,
};
use crate::error::{Error, Result};
use crate::source::manifest::check_compatible;

use super::adapter::{Discovery, SourceAdapter};
use host::memcastle::source::types as wit;
use host::{HostState, Source, SourcePre};

/// How often the engine's epoch advances, which is the resolution of a source's time limit.
const EPOCH_TICK: Duration = Duration::from_millis(50);

/// The longest a message from a failed source is kept, so a trap's backtrace cannot flood a job's error.
const MAX_MESSAGE_CHARS: usize = 600;

/// The one engine every source runs on. Epoch interruption is what bounds a call: a ticker advances the epoch and a
/// store's deadline traps the guest, even one stuck in a loop that never calls the host.
fn engine() -> Result<&'static Engine> {
    static ENGINE: OnceLock<std::result::Result<Engine, String>> = OnceLock::new();
    let engine = ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| format!("{e:#}"))?;
        let weak = engine.weak();
        std::thread::Builder::new()
            .name("memcastle-source-epoch".to_string())
            .spawn(move || {
                // Ends with the engine, which a process-wide static never does; the check is for tidiness.
                while let Some(engine) = weak.upgrade() {
                    engine.increment_epoch();
                    drop(engine);
                    std::thread::sleep(EPOCH_TICK);
                }
            })
            .map_err(|e| format!("cannot start the source timer: {e}"))?;
        Ok(engine)
    });
    engine.as_ref().map_err(|message| Error::SourceFailed {
        name: "runtime".to_string(),
        message: message.clone(),
    })
}

/// Compiled components by the digest of their bytes: compiling is the expensive step, and the same installed source
/// is loaded for every job that mines it.
static COMPONENTS: LazyLock<DashMap<String, Component>> = LazyLock::new(DashMap::new);

/// A source loaded from a WebAssembly component, behind the [`SourceAdapter`] contract.
#[derive(Clone)]
pub struct WasmAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    description: String,
    capabilities: SourceCapabilities,
    permissions: Permissions,
    engine: &'static Engine,
    pre: SourcePre<HostState>,
    memory_bytes: usize,
    timeout: Duration,
    /// Asked of the component once at load: it is constant, and the contract returns `&str`.
    default_room: String,
}

impl std::fmt::Debug for WasmAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmAdapter")
            .field("name", &self.inner.name)
            .finish_non_exhaustive()
    }
}

impl WasmAdapter {
    /// Load the component `bytes` as the source `manifest` describes.
    ///
    /// Compiles (cached by digest), links the host functions, and asks the component for its default room, which
    /// proves it implements the contract before a job depends on it. Blocking: call from a blocking context.
    ///
    /// # Errors
    ///
    /// [`Error::SourceIncompatible`] when the manifest or the component does not fit this MemCastle, and
    /// [`Error::SourceFailed`] when the component does not run.
    pub fn load(manifest: &SourceManifest, bytes: &[u8], mining: &MiningConfig) -> Result<Self> {
        check_compatible(manifest)?;
        let name = manifest.source.name.clone();
        let incompatible = |reason: String| Error::SourceIncompatible {
            name: name.clone(),
            reason,
        };
        let engine = engine()?;
        let digest = sha256_hex(bytes);
        let component = match COMPONENTS.get(&digest) {
            Some(cached) => cached.clone(),
            None => {
                let compiled = Component::new(engine, bytes).map_err(|e| {
                    incompatible(format!("it is not a valid WebAssembly component: {e:#}"))
                })?;
                COMPONENTS.insert(digest, compiled.clone());
                compiled
            }
        };
        let mut linker = Linker::<HostState>::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|e| incompatible(format!("{e:#}")))?;
        Source::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |state| state)
            .map_err(|e| incompatible(format!("{e:#}")))?;
        let pre = linker
            .instantiate_pre(&component)
            .and_then(SourcePre::new)
            .map_err(|e| {
                incompatible(format!(
                    "it does not fit the `memcastle:source` contract this MemCastle implements: {e:#}"
                ))
            })?;

        // What the source asks for, never more than the host allows.
        let ceiling_mib = u64::from(mining.source_memory_mib);
        let memory_mib = manifest
            .limits
            .memory_mib
            .map_or(ceiling_mib, |asked| u64::from(asked).min(ceiling_mib));
        let timeout = Duration::from_secs(
            manifest
                .limits
                .timeout_secs
                .map_or(mining.source_timeout_secs, |asked| {
                    asked.min(mining.source_timeout_secs)
                }),
        );
        let mut inner = Inner {
            name,
            description: manifest.source.description.clone(),
            capabilities: manifest.capabilities,
            permissions: manifest.permissions.normalized(),
            engine,
            pre,
            memory_bytes: usize::try_from(memory_mib * 1024 * 1024).unwrap_or(usize::MAX),
            timeout,
            default_room: String::new(),
        };
        inner.default_room = inner.call(false, None, |store, guest| {
            guest.call_default_room(store).map(Ok)
        })?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Run a blocking call from a synchronous trait method without stalling the async runtime's worker.
    fn blocking<R>(&self, work: impl FnOnce(&Inner) -> R) -> R {
        match tokio::runtime::Handle::try_current() {
            // `block_in_place` is only legal on the multi-threaded runtime the daemon uses; a current-thread
            // runtime (a test's) has nothing else to starve and runs the call directly.
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| work(&self.inner))
            }
            _ => work(&self.inner),
        }
    }
}

impl Inner {
    /// One call into the component, in a sandbox built for it.
    ///
    /// `granted` is whether the manifest's permissions apply: `normalize` runs without them, so that it is pure
    /// whatever the source asked for. `f` returns the guest's own `result`, which is mapped to MemCastle's errors
    /// here, together with traps, timeouts and refused permissions.
    fn call<R>(
        &self,
        granted: bool,
        locator: Option<&str>,
        f: impl FnOnce(
            &mut Store<HostState>,
            &host::exports::memcastle::source::adapter::Guest,
        ) -> wasmtime::Result<std::result::Result<R, wit::SourceError>>,
    ) -> Result<R> {
        let none = Permissions::default();
        let permissions = if granted { &self.permissions } else { &none };
        let state = host::state(permissions, locator, self.memory_bytes, self.timeout)
            .map_err(|message| self.failed(message))?;
        let mut store = Store::new(self.engine, state);
        store.limiter(|state| &mut state.limits);
        // At least one tick, plus one because the current tick may be nearly over.
        let ticks = (self.timeout.as_millis() / EPOCH_TICK.as_millis()).max(1) + 1;
        store.set_epoch_deadline(u64::try_from(ticks).unwrap_or(u64::MAX));

        let outcome = self
            .pre
            .instantiate(&mut store)
            .and_then(|instance| f(&mut store, instance.memcastle_source_adapter()));
        let denied = store.data().denied.clone();
        match outcome {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(self.guest_error(error, denied)),
            Err(trap) => Err(self.trap_error(&trap, denied)),
        }
    }

    fn failed(&self, message: impl Into<String>) -> Error {
        let mut message = message.into();
        if message.chars().count() > MAX_MESSAGE_CHARS {
            message = message.chars().take(MAX_MESSAGE_CHARS).collect::<String>() + "...";
        }
        Error::SourceFailed {
            name: self.name.clone(),
            message,
        }
    }

    fn guest_error(&self, error: wit::SourceError, denied: Option<String>) -> Error {
        match error {
            wit::SourceError::CursorInvalid(message) => Error::SourceCursorInvalid {
                provider: self.name.clone(),
                message,
            },
            wit::SourceError::InvalidInput(message) => Error::invalid_input("locator", message),
            // A refused permission is the likely cause of whatever the source made of it, so it is what is reported.
            wit::SourceError::Failed(message) => match denied {
                Some(denied) => Error::SourcePermissionDenied {
                    name: self.name.clone(),
                    message: denied,
                },
                None => self.failed(message),
            },
        }
    }

    fn trap_error(&self, error: &wasmtime::Error, denied: Option<String>) -> Error {
        if error.downcast_ref::<Trap>() == Some(&Trap::Interrupt) {
            return Error::SourceTimeout {
                name: self.name.clone(),
                secs: self.timeout.as_secs(),
            };
        }
        if let Some(message) = denied {
            return Error::SourcePermissionDenied {
                name: self.name.clone(),
                message,
            };
        }
        self.failed(format!("{error:#}"))
    }

    /// Grants a source's directory permission can make use of: the canonical form of a locator, when the manifest
    /// asks for the `locator` directory, so that the guest and the preopen agree on the path.
    fn grant_locator(&self, locator: &str) -> String {
        if self
            .permissions
            .filesystem
            .read
            .iter()
            .any(|entry| entry == "locator")
        {
            std::fs::canonicalize(locator)
                .map_or_else(|_| locator.to_string(), |p| p.display().to_string())
        } else {
            locator.to_string()
        }
    }
}

fn to_wit(source: &SourceRef) -> wit::SourceRef {
    wit::SourceRef {
        provider: source.provider.clone(),
        account: source.account.clone(),
        locator: source.locator.clone(),
    }
}

fn from_wit(source: wit::SourceRef) -> SourceRef {
    SourceRef {
        provider: source.provider,
        account: source.account,
        locator: source.locator,
    }
}

fn raw_to_wit(raw: &RawDocument) -> wit::RawDocument {
    wit::RawDocument {
        external_id: raw.external_id.clone(),
        revision: raw.revision.clone(),
        body: raw.body.clone(),
        metadata: raw.metadata.to_string(),
        occurred_at: raw.occurred_at.map(|at| at.to_rfc3339()),
    }
}

impl SourceAdapter for WasmAdapter {
    fn provider(&self) -> &str {
        &self.inner.name
    }

    fn description(&self) -> &str {
        &self.inner.description
    }

    fn capabilities(&self) -> SourceCapabilities {
        self.inner.capabilities
    }

    fn identify(&self, locator: Option<&str>) -> Result<SourceRef> {
        self.blocking(|inner| {
            // The host hands over the canonical path of a directory it is about to grant, so that `identify` is the
            // one place two spellings of a place become one source, as for a built-in adapter.
            let locator = locator.map(|locator| inner.grant_locator(locator));
            inner
                .call(true, locator.as_deref(), |store, guest| {
                    guest.call_identify(store, locator.as_deref())
                })
                .map(from_wit)
        })
    }

    fn default_wing(&self, source: &SourceRef) -> String {
        self.blocking(|inner| {
            let fallback = || "unnamed".to_string();
            inner
                .call(false, None, |store, guest| {
                    guest.call_default_wing(store, &to_wit(source)).map(Ok)
                })
                // A wing name is cosmetic: a source that fails to give one files under a neutral name, and the
                // real failure surfaces from the calls that matter.
                .unwrap_or_else(|_| fallback())
        })
    }

    fn default_room(&self) -> &str {
        &self.inner.default_room
    }

    async fn discover(
        &self,
        source: &SourceRef,
        cursor: &Cursor,
        limit: usize,
    ) -> Result<Discovery> {
        let inner = self.inner.clone();
        let source = source.clone();
        let cursor = cursor.to_string();
        let limit = u32::try_from(limit).unwrap_or(u32::MAX);
        run_blocking(move || {
            let found = inner.call(true, Some(&source.locator), |store, guest| {
                guest.call_discover(store, &to_wit(&source), &cursor, limit)
            })?;
            let candidates = found
                .candidates
                .into_iter()
                .map(|candidate| {
                    let cursor_after =
                        serde_json::from_str(&candidate.cursor_after).map_err(|e| {
                            inner.failed(format!(
                                "candidate `{}` has a cursor-after that is not JSON: {e}",
                                candidate.external_id
                            ))
                        })?;
                    Ok(Candidate {
                        external_id: candidate.external_id,
                        cursor_after,
                        handle: candidate.handle,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Discovery {
                candidates,
                exhausted: found.exhausted,
            })
        })
        .await
    }

    async fn read(&self, source: &SourceRef, candidate: &Candidate) -> Result<Option<RawDocument>> {
        let inner = self.inner.clone();
        let source = source.clone();
        let candidate = wit::Candidate {
            external_id: candidate.external_id.clone(),
            cursor_after: candidate.cursor_after.to_string(),
            handle: candidate.handle.clone(),
        };
        run_blocking(move || {
            let read = inner.call(true, Some(&source.locator), |store, guest| {
                guest.call_read(store, &to_wit(&source), &candidate)
            })?;
            read.map(|raw| {
                let metadata = serde_json::from_str(&raw.metadata).map_err(|e| {
                    inner.failed(format!(
                        "document `{}` has metadata that is not JSON: {e}",
                        raw.external_id
                    ))
                })?;
                let occurred_at = raw
                    .occurred_at
                    .as_deref()
                    .map(|at| {
                        chrono::DateTime::parse_from_rfc3339(at)
                            .map(|at| at.with_timezone(&chrono::Utc))
                            .map_err(|e| {
                                inner.failed(format!(
                                    "document `{}` has an occurred-at that is not RFC 3339: {e}",
                                    raw.external_id
                                ))
                            })
                    })
                    .transpose()?;
                Ok(RawDocument {
                    external_id: raw.external_id,
                    revision: raw.revision,
                    body: raw.body,
                    metadata,
                    occurred_at,
                })
            })
            .transpose()
        })
        .await
    }

    fn normalize(&self, raw: &RawDocument) -> Result<CanonicalDocument> {
        self.blocking(|inner| {
            // No grants: normalization is pure by contract, and the host holds a source to it.
            let canonical = inner.call(false, None, |store, guest| {
                guest.call_normalize(store, &raw_to_wit(raw))
            })?;
            Ok(CanonicalDocument {
                title: canonical.title,
                room: canonical.room,
                name: canonical.name,
                kind: match canonical.kind {
                    wit::SourceKind::File => SourceKind::File,
                    wit::SourceKind::Manual => SourceKind::Manual,
                    wit::SourceKind::Transcript => SourceKind::Transcript,
                    wit::SourceKind::Other => SourceKind::Other,
                },
                uri: canonical.uri,
                tags: canonical.tags,
                segments: canonical
                    .segments
                    .into_iter()
                    .map(|segment| Segment { text: segment.text })
                    .collect(),
            })
        })
    }
}

/// Run a blocking call off the async workers: a source may take its whole time limit, and must not stall the jobs
/// and requests sharing the runtime meanwhile.
async fn run_blocking<R: Send + 'static>(
    work: impl FnOnce() -> Result<R> + Send + 'static,
) -> Result<R> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::SourceFailed {
            name: "runtime".to_string(),
            message: format!("the source call was interrupted: {e}"),
        })?
}
