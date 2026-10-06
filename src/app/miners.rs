//! Miner configuration: the `[[miners]]` of the configuration file, listed, changed and run.
//!
//! One definition per miner, kept in the file the daemon was started from and read back from it, never held in a
//! second place (`docs/adr/037-persistent-miner-configuration.md`). The CLI, REST and the read-only MCP tools all call
//! the functions here, so there is one set of rules.
//!
//! Changing miners is administrative like installing a source: it is not gated by the memory mode, and no MCP tool
//! does it, so an agent can read the miners but cannot configure what the daemon mines. Running one is a memory
//! write, because it files drawers, and is gated as one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::AppServices;
use crate::config::miners_file::{self, FileStamp};
use crate::domain::{
    CredentialRef, Job, MemoryMode, MinerDefinition, MinerTrigger, MiningSource, NameKind,
    SourceId, SourceRecord, TriggerKind, scope_broadening, validate_name,
};
use crate::error::{Error, Result};

/// Where the miners are, and the last good copy of them.
///
/// The file is the truth; this holds what was last read from it so a hand edit is noticed (the file's stamp changed),
/// an invalid one does not take the miners away (the last good copy stays, with the error beside it), and a write
/// can refuse to overwrite an edit it has not seen.
pub struct MinerRegistry {
    /// The configuration file. `None` for a configuration built in code, where changing miners is refused.
    path: Option<PathBuf>,
    state: Mutex<RegistryState>,
}

struct RegistryState {
    /// The last miners that parsed and validated.
    miners: Vec<MinerDefinition>,
    /// What the file looked like when `miners` was read; `None` before the first read.
    stamp: Option<Option<FileStamp>>,
    /// Why the file could not be read the last time it changed.
    error: Option<String>,
}

impl MinerRegistry {
    /// A registry over `path`, starting from the miners the daemon loaded at startup.
    #[must_use]
    pub fn new(path: Option<PathBuf>, initial: Vec<MinerDefinition>) -> Self {
        Self {
            path,
            state: Mutex::new(RegistryState {
                miners: initial,
                stamp: None,
                error: None,
            }),
        }
    }

    /// The path, or the error that says changing miners needs one.
    fn path(&self) -> Result<&Path> {
        self.path.as_deref().ok_or_else(|| Error::MinerConfigFile {
            path: "-".to_string(),
            reason: "this daemon was not started from a configuration file, so there is nowhere to keep miners"
                .to_string(),
        })
    }

    /// Read the file again if it changed (or `force`), keeping the last good copy when it no longer parses.
    ///
    /// Returns what changed. A hand edit that widens an enabled miner's scope is applied, because the file is the
    /// user's, but it is logged: the daemon never widens a scope itself, and says when someone else did.
    fn refresh(&self, state: &mut RegistryState, force: bool) -> Result<MinersDiff> {
        let Some(path) = self.path.as_deref() else {
            return Ok(MinersDiff::default());
        };
        let current = miners_file::stamp(path).inspect_err(|e| state.error = Some(problem(e)))?;
        if !force && state.stamp == Some(current) {
            return Ok(MinersDiff::default());
        }
        match miners_file::read(path) {
            Ok(loaded) => {
                let diff = MinersDiff::between(&state.miners, &loaded.miners);
                for name in &diff.broadened {
                    warn!(miner = %name, "the configuration file widened this miner's scope; applied as written");
                }
                if !diff.is_empty() {
                    info!(
                        added = ?diff.added, removed = ?diff.removed, changed = ?diff.changed,
                        "miner configuration reloaded"
                    );
                }
                state.miners = loaded.miners;
                state.stamp = Some(loaded.stamp);
                state.error = None;
                Ok(diff)
            }
            Err(error) => {
                // Remembered so the next request does not read the same broken file again; a further edit changes
                // the stamp and tries again.
                state.stamp = Some(current);
                state.error = Some(problem(&error));
                warn!(error = %error, "the miner configuration is invalid; keeping the last good miners");
                Err(error)
            }
        }
    }
}

impl Default for MinerRegistry {
    fn default() -> Self {
        Self::new(None, Vec::new())
    }
}

/// The reason inside a configuration-file error, without the sentence around it.
fn problem(error: &Error) -> String {
    match error {
        Error::MinerConfigFile { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

/// What a reload found different, by miner name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinersDiff {
    /// Miners that were not there before.
    pub added: Vec<String>,
    /// Miners that are gone.
    pub removed: Vec<String>,
    /// Miners whose definition differs.
    pub changed: Vec<String>,
    /// Changed miners that are now enabled and were not.
    pub enabled: Vec<String>,
    /// Changed miners that are now disabled and were not.
    pub disabled: Vec<String>,
    /// Changed miners whose scope is now wider.
    pub broadened: Vec<String>,
}

impl MinersDiff {
    fn between(old: &[MinerDefinition], new: &[MinerDefinition]) -> Self {
        let mut diff = Self::default();
        for miner in new {
            let Some(before) = old.iter().find(|m| m.name == miner.name) else {
                diff.added.push(miner.name.clone());
                continue;
            };
            if before == miner {
                continue;
            }
            diff.changed.push(miner.name.clone());
            match (before.enabled, miner.enabled) {
                (false, true) => diff.enabled.push(miner.name.clone()),
                (true, false) => diff.disabled.push(miner.name.clone()),
                _ => {}
            }
            if !scope_broadening(&before.scope, &miner.scope).is_empty() {
                diff.broadened.push(miner.name.clone());
            }
        }
        diff.removed = old
            .iter()
            .filter(|m| !new.iter().any(|n| n.name == m.name))
            .map(|m| m.name.clone())
            .collect();
        diff
    }

    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Whether a miner can run right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MinerState {
    /// Enabled and its source and credential are usable.
    Ready,
    /// Switched off.
    Disabled,
    /// Enabled, but something it needs is missing; the reason says what.
    Unavailable,
}

/// A miner's credential reference, without the reference: what kind it is and whether it resolves.
///
/// The variable's name or the file's path is not given back, so a client that can read miners learns nothing about
/// where the daemon keeps its secrets, and the value is never read for this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialView {
    /// `env` or `file`.
    pub kind: String,
    /// Whether the variable is set to something, or the file exists.
    pub available: bool,
}

/// One miner as the CLI, REST and MCP show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinerView {
    /// The miner's name.
    pub name: String,
    /// The source adapter it runs.
    pub source: String,
    /// Whether it may run.
    pub enabled: bool,
    /// Whether it can run right now.
    pub state: MinerState,
    /// Why it cannot, when `state` is `unavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The part of the source it reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    /// The wing its drawers go to, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wing: Option<String>,
    /// Its credential, as a kind and an availability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialView>,
    /// Its scope filter.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub scope: Map<String, Value>,
    /// What starts it.
    pub trigger: MinerTrigger,
    /// Its source-specific settings.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub config: Map<String, Value>,
    /// The mined source this miner's cursor lives on, once something has been mined from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<SourceId>,
    /// When that source was last mined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    /// How many documents that source holds.
    #[serde(default)]
    pub documents: u64,
    /// Things that are stored but not acted on yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Every miner, and whether the file they come from is currently readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinersReport {
    /// The miners, in file order.
    pub miners: Vec<MinerView>,
    /// The file they are kept in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
    /// Why the file cannot be read, when it cannot: `miners` is then the last good copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A change to one miner: what to set, and what to clear.
///
/// A key that is absent is left alone. `scope` and `config` set the keys they name (replacing that key's value);
/// the `unset_*` lists remove keys. Creating a miner needs `source`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MinerPatch {
    /// The source adapter.
    pub source: Option<String>,
    /// Switch it on or off.
    pub enabled: Option<bool>,
    /// The part of the source to read.
    pub locator: Option<String>,
    /// The wing for its drawers.
    pub wing: Option<String>,
    /// Where its credential comes from.
    pub credential: Option<CredentialRef>,
    /// Scope keys to set.
    pub scope: Map<String, Value>,
    /// Scope keys to remove.
    pub unset_scope: Vec<String>,
    /// The trigger, replaced as a whole.
    pub trigger: Option<MinerTrigger>,
    /// Source-specific settings to set.
    pub config: Map<String, Value>,
    /// Source-specific settings to remove.
    pub unset_config: Vec<String>,
    /// Fields to clear: any of `locator`, `wing`, `credential`, `trigger`.
    pub unset: Vec<String>,
    /// Allow the change to make the scope wider.
    pub allow_broaden: bool,
}

/// The outcome of [`AppServices::set_miner`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinerChange {
    /// The miner as it is now.
    pub miner: MinerView,
    /// Whether it did not exist before.
    pub created: bool,
    /// Whether anything was written; false when the request changed nothing.
    pub changed: bool,
    /// Whether it now reads a different source or locator, which is a different cursor.
    pub identity_changed: bool,
}

/// The outcome of [`AppServices::reload_miners`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinersReload {
    /// How many miners are configured now.
    pub miners: usize,
    /// What differs from what the daemon had.
    #[serde(flatten)]
    pub diff: MinersDiff,
}

impl AppServices {
    /// Give the services the miner registry the daemon built from its configuration file.
    #[must_use]
    pub fn with_miners(mut self, miners: MinerRegistry) -> Self {
        self.miners = Arc::new(miners);
        self
    }

    /// Every configured miner with its state.
    ///
    /// Gated as a **read**. A hand edit made since the last call is picked up first; if the file no longer parses
    /// the last good miners are reported with the error beside them, not an error, so an agent or a dashboard can
    /// still see what is configured and why it is stale.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] if `mode` forbids reads, or a store error.
    pub async fn list_miners(&self, mode: MemoryMode) -> Result<MinersReport> {
        Self::require_read(mode, "miner_list")?;
        let (miners, error) = {
            let mut state = self.miners.state.lock().await;
            // The error is in `state.error`; a failed refresh must not turn a read into a failure.
            let _ = self.miners.refresh(&mut state, false);
            (state.miners.clone(), state.error.clone())
        };
        let sources = self.store.list_sources().await?;
        let mut views = Vec::with_capacity(miners.len());
        for miner in &miners {
            views.push(self.miner_view(miner, &sources).await?);
        }
        Ok(MinersReport {
            miners: views,
            config_file: self.miners.path.as_ref().map(|p| p.display().to_string()),
            error,
        })
    }

    /// One miner.
    ///
    /// # Errors
    ///
    /// [`Error::MinerNotFound`], [`Error::ModeForbidden`] if `mode` forbids reads, or a store error.
    pub async fn show_miner(&self, name: &str, mode: MemoryMode) -> Result<MinerView> {
        Self::require_read(mode, "miner_get")?;
        let miner = {
            let mut state = self.miners.state.lock().await;
            let _ = self.miners.refresh(&mut state, false);
            find(&state.miners, name)?.clone()
        };
        let sources = self.store.list_sources().await?;
        self.miner_view(&miner, &sources).await
    }

    /// Create a miner, or change one: the same call for both, so `miner set` is idempotent.
    ///
    /// Everything is checked before anything is written: the definition's shape, the wing's name, that an enabled
    /// miner's source is usable and its credential resolves, and that the change does not widen the scope (unless
    /// `allow_broaden`). A rejected change leaves the file untouched.
    ///
    /// Administrative: not gated by the memory mode, and not reachable from MCP.
    ///
    /// # Errors
    ///
    /// [`Error::MinerInvalid`], [`Error::MinerScopeBroadened`], [`Error::MinerConfigFile`] when the file is
    /// unreadable or changed underneath, and store errors.
    pub async fn set_miner(&self, name: &str, patch: MinerPatch) -> Result<MinerChange> {
        self.apply_patch(name, patch, false).await
    }

    /// Switch a miner on or off. Idempotent. Switching on checks that it can run, like [`Self::set_miner`].
    ///
    /// # Errors
    ///
    /// [`Error::MinerNotFound`] and the errors of [`Self::set_miner`].
    pub async fn set_miner_enabled(&self, name: &str, enabled: bool) -> Result<MinerChange> {
        let patch = MinerPatch {
            enabled: Some(enabled),
            ..MinerPatch::default()
        };
        self.apply_patch(name, patch, true).await
    }

    /// Remove a miner's definition. What it mined, and the source's cursor, stay.
    ///
    /// # Errors
    ///
    /// [`Error::MinerNotFound`] and [`Error::MinerConfigFile`].
    pub async fn remove_miner(&self, name: &str) -> Result<()> {
        let path = self.miners.path()?;
        let mut state = self.miners.state.lock().await;
        self.refresh_for_write(&mut state)?;
        find(&state.miners, name)?;
        let remaining: Vec<MinerDefinition> = state
            .miners
            .iter()
            .filter(|m| m.name != name)
            .cloned()
            .collect();
        let stamp = miners_file::write(path, state.stamp.flatten(), &remaining)?;
        state.miners = remaining;
        state.stamp = Some(stamp);
        info!(miner = %name, "miner removed");
        Ok(())
    }

    /// Read the configuration file again now, whether or not it looks changed, and say what differs.
    ///
    /// # Errors
    ///
    /// [`Error::MinerConfigFile`] when the file cannot be read or does not validate; the daemon keeps the last good
    /// miners.
    pub async fn reload_miners(&self) -> Result<MinersReload> {
        self.miners.path()?;
        let mut state = self.miners.state.lock().await;
        let diff = self.miners.refresh(&mut state, true)?;
        Ok(MinersReload {
            miners: state.miners.len(),
            diff,
        })
    }

    /// Submit the mining job for a miner.
    ///
    /// Refused when the miner is disabled or cannot run, and when it has a scope or settings: no source applies them
    /// yet, so running it could only mine more than the filter says, and the daemon never does that silently. The
    /// job mines the miner's source and locator, so it continues from the cursor the same source already has.
    ///
    /// Gated as a **write**, like any mining.
    ///
    /// # Errors
    ///
    /// [`Error::MinerNotFound`], [`Error::MinerDisabled`], [`Error::MinerNotRunnable`], and the errors of
    /// [`AppServices::submit_mine`].
    pub async fn run_miner(
        &self,
        name: &str,
        full: bool,
        requested_by: &str,
        mode: MemoryMode,
    ) -> Result<Job> {
        Self::require_write(mode, "miner_run")?;
        let miner = {
            let mut state = self.miners.state.lock().await;
            let _ = self.miners.refresh(&mut state, false);
            find(&state.miners, name)?.clone()
        };
        if !miner.enabled {
            return Err(Error::MinerDisabled { name: miner.name });
        }
        let not_runnable = |reason: String| Error::MinerNotRunnable {
            name: miner.name.clone(),
            reason,
        };
        if let Err(reason) = self.check_activation(&miner).await {
            return Err(not_runnable(reason));
        }
        if !miner.scope.is_empty() || !miner.config.is_empty() {
            return Err(not_runnable(
                "it has a scope or settings, and no source applies them yet, so a run would ignore them; \
                 remove them, or mine by hand with `memcastle mine`"
                    .to_string(),
            ));
        }
        let Some(locator) = miner.locator.clone() else {
            return Err(not_runnable(
                "it has no locator; set one with `memcastle miner set <name> --locator <where>`"
                    .to_string(),
            ));
        };
        self.submit_mine(
            MiningSource::Named {
                source: miner.source.clone(),
                locator: Some(locator),
            },
            miner.wing.clone(),
            full,
            requested_by,
            mode,
        )
        .await
    }

    /// Re-read the file for a write, and refuse when it cannot be trusted.
    fn refresh_for_write(&self, state: &mut RegistryState) -> Result<()> {
        // A hand edit since the last request is applied first, so the write is built on what is in the file.
        self.miners.refresh(state, false)?;
        if let Some(reason) = &state.error {
            // Writing would overwrite whatever the user was in the middle of fixing.
            return Err(Error::MinerConfigFile {
                path: self.miners.path()?.display().to_string(),
                reason: reason.clone(),
            });
        }
        Ok(())
    }

    async fn apply_patch(
        &self,
        name: &str,
        patch: MinerPatch,
        must_exist: bool,
    ) -> Result<MinerChange> {
        let path = self.miners.path()?;
        let mut state = self.miners.state.lock().await;
        self.refresh_for_write(&mut state)?;

        let existing = state.miners.iter().find(|m| m.name == name).cloned();
        if must_exist && existing.is_none() {
            return Err(Error::MinerNotFound {
                name: name.to_string(),
            });
        }
        let allow_broaden = patch.allow_broaden;
        let candidate = apply(name, existing.as_ref(), patch)?;
        if let Some(wing) = &candidate.wing {
            validate_name(NameKind::Wing, wing).map_err(|e| invalid(name, e.to_string()))?;
        }
        if let Some(before) = &existing {
            let reasons = scope_broadening(&before.scope, &candidate.scope);
            if !reasons.is_empty() && !allow_broaden {
                return Err(Error::MinerScopeBroadened {
                    name: name.to_string(),
                    reasons: reasons.join("; "),
                });
            }
        }
        // Only a miner that is going to run is held to what running needs: a disabled one may name a source that is
        // not installed yet, so a configuration can be written ahead of the install.
        if candidate.enabled && existing.as_ref() != Some(&candidate) {
            self.check_activation(&candidate)
                .await
                .map_err(|reason| invalid(name, reason))?;
        }

        let created = existing.is_none();
        let identity_changed = existing
            .as_ref()
            .is_some_and(|b| b.source != candidate.source || b.locator != candidate.locator);
        let changed = existing.as_ref() != Some(&candidate);
        if changed {
            let mut all = state.miners.clone();
            match all.iter_mut().find(|m| m.name == name) {
                Some(slot) => *slot = candidate.clone(),
                None => all.push(candidate.clone()),
            }
            let stamp = miners_file::write(path, state.stamp.flatten(), &all)?;
            state.miners = all;
            state.stamp = Some(stamp);
            info!(miner = %name, created, "miner configuration written");
        }
        drop(state);

        let sources = self.store.list_sources().await?;
        Ok(MinerChange {
            miner: self.miner_view(&candidate, &sources).await?,
            created,
            changed,
            identity_changed,
        })
    }

    /// What an enabled miner needs in order to run: a usable source, a locator the source can use, a credential that
    /// resolves. The reason is a sentence that says what to do.
    async fn check_activation(&self, miner: &MinerDefinition) -> std::result::Result<(), String> {
        crate::mining::registry::ensure_minable(&self.store, &self.mining, &miner.source)
            .await
            .map_err(|e| e.to_string())?;
        if miner.source == "directory" {
            let Some(locator) = miner.locator.as_deref() else {
                return Err(
                    "the directory source needs a `locator`: the absolute path to mine".to_string(),
                );
            };
            let path = Path::new(locator);
            // The daemon resolves a relative path against its own working directory, which is not the caller's.
            if !(path.is_absolute() || path.has_root()) {
                return Err(format!(
                    "the locator `{locator}` is relative; give an absolute path, since the daemon resolves it \
                     against its own working directory"
                ));
            }
            if !miner.scope.is_empty() || !miner.config.is_empty() {
                return Err(
                    "the directory source has no scope or settings; remove `scope` and `config`"
                        .to_string(),
                );
            }
        }
        self.check_credential(miner).await?;
        Ok(())
    }

    /// A source that signs in with OAuth needs that done whatever the miner says, and a miner that says `oauth` for a
    /// source that does not is mistaken: both are said here, with the command that fixes it.
    pub(super) async fn check_credential(
        &self,
        miner: &MinerDefinition,
    ) -> std::result::Result<(), String> {
        let requirement = self.oauth_requirement(&miner.source).await;
        match &miner.credential {
            Some(CredentialRef::Env { name })
                if !credential_available(&CredentialRef::Env { name: name.clone() }) =>
            {
                return Err(format!(
                    "the credential's environment variable `{name}` is not set or is empty in the daemon's \
                     environment; export it and restart the daemon, or disable the miner"
                ));
            }
            Some(CredentialRef::File { path }) if !Path::new(path).is_file() => {
                return Err(format!(
                    "the credential's file `{path}` does not exist; create it, or disable the miner"
                ));
            }
            Some(CredentialRef::Oauth) if requirement.is_none() => {
                return Err(format!(
                    "the source `{}` does not sign in with OAuth, so `oauth` is not a credential it can use; \
                     use `--credential-env` or `--credential-file`, or unset the credential",
                    miner.source
                ));
            }
            _ => {}
        }
        if let Some(requirement) = requirement
            && !self.signed_in(&miner.source, requirement).await
        {
            return Err(format!(
                "the source `{0}` is not signed in; run `memcastle source auth {0}`",
                miner.source
            ));
        }
        Ok(())
    }

    /// What the installed source `name` declares under `[permissions.oauth]`, if it is installed and declares it.
    async fn oauth_requirement(&self, name: &str) -> Option<crate::domain::OAuthRequirement> {
        self.store
            .get_source_package(name)
            .await
            .ok()
            .flatten()
            .and_then(|record| record.manifest.permissions.oauth)
    }

    /// Whether `source` has a credential it could use, asked off the async runtime because the store may be a system
    /// service.
    async fn signed_in(&self, source: &str, requirement: crate::domain::OAuthRequirement) -> bool {
        use crate::domain::AccessTokens as _;
        let credentials = self.credentials.clone();
        let source = source.to_string();
        tokio::task::spawn_blocking(move || credentials.is_signed_in(&source, &requirement))
            .await
            .unwrap_or(false)
    }

    /// A miner for display: its state, and what has been mined from it.
    async fn miner_view(
        &self,
        miner: &MinerDefinition,
        sources: &[SourceRecord],
    ) -> Result<MinerView> {
        let (state, reason) = if !miner.enabled {
            (MinerState::Disabled, None)
        } else {
            match self.check_activation(miner).await {
                Ok(()) => (MinerState::Ready, None),
                Err(reason) => (MinerState::Unavailable, Some(reason)),
            }
        };
        // The record the adapter keeps the cursor on: same source and the miner's locator (a directory's is
        // canonicalised when it is mined, so a symlinked locator still finds it).
        let record = miner.locator.as_deref().and_then(|locator| {
            let canonical = (miner.source == "directory")
                .then(|| std::fs::canonicalize(locator).ok())
                .flatten()
                .map(|p| p.to_string_lossy().into_owned());
            sources.iter().find(|s| {
                s.source == miner.source
                    && (s.locator == locator || canonical.as_deref() == Some(s.locator.as_str()))
            })
        });
        let documents = match record {
            Some(record) => self.store.count_source_documents(record.id).await?,
            None => 0,
        };
        let mut warnings = Vec::new();
        if miner.trigger.kind != TriggerKind::Manual {
            warnings.push(format!(
                "the {} trigger is stored but not acted on yet; `memcastle miner run {}` mines it now",
                serde_json::to_value(miner.trigger.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                miner.name
            ));
        }
        if !miner.scope.is_empty() || !miner.config.is_empty() {
            warnings.push(
                "the scope and settings are stored but no source applies them yet, so `miner run` refuses this miner"
                    .to_string(),
            );
        }
        let credential = match &miner.credential {
            Some(c) => Some(CredentialView {
                kind: match c {
                    CredentialRef::Env { .. } => "env",
                    CredentialRef::File { .. } => "file",
                    CredentialRef::Oauth => "oauth",
                }
                .to_string(),
                // For `oauth`, "resolves" means signed in: the same question the activation check asks.
                available: match (c, self.oauth_requirement(&miner.source).await) {
                    (CredentialRef::Oauth, Some(requirement)) => {
                        self.signed_in(&miner.source, requirement).await
                    }
                    (CredentialRef::Oauth, None) => false,
                    (other, _) => credential_available(other),
                },
            }),
            None => None,
        };
        Ok(MinerView {
            name: miner.name.clone(),
            source: miner.source.clone(),
            enabled: miner.enabled,
            state,
            reason,
            locator: miner.locator.clone(),
            wing: miner.wing.clone(),
            credential,
            scope: miner.scope.clone(),
            trigger: miner.trigger.clone(),
            config: miner.config.clone(),
            source_id: record.map(|r| r.id),
            last_run_at: record.and_then(|r| r.last_run_at),
            documents,
            warnings,
        })
    }
}

fn invalid(name: &str, reason: impl Into<String>) -> Error {
    Error::MinerInvalid {
        name: name.to_string(),
        reason: reason.into(),
    }
}

fn find<'a>(miners: &'a [MinerDefinition], name: &str) -> Result<&'a MinerDefinition> {
    miners
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| Error::MinerNotFound {
            name: name.to_string(),
        })
}

/// Whether a credential reference points at something that exists. The value itself is never read here.
fn credential_available(credential: &CredentialRef) -> bool {
    match credential {
        CredentialRef::Env { name } => std::env::var_os(name).is_some_and(|v| !v.is_empty()),
        CredentialRef::File { path } => Path::new(path).is_file(),
        // Whether a sign-in exists is the daemon's to answer, not a file or a variable to look at.
        CredentialRef::Oauth => false,
    }
}

/// The definition `patch` turns `existing` into, or a new one; checked for shape.
fn apply(
    name: &str,
    existing: Option<&MinerDefinition>,
    patch: MinerPatch,
) -> Result<MinerDefinition> {
    let mut miner = match existing {
        Some(existing) => existing.clone(),
        None => {
            let Some(source) = patch.source.clone() else {
                return Err(invalid(
                    name,
                    format!(
                        "there is no miner `{name}` to change, and a new one needs a `source`; \
                         `memcastle miner set {name} --source <source>` creates it"
                    ),
                ));
            };
            MinerDefinition {
                name: name.to_string(),
                source,
                enabled: true,
                locator: None,
                wing: None,
                credential: None,
                scope: Map::new(),
                trigger: MinerTrigger::default(),
                config: Map::new(),
            }
        }
    };
    for field in &patch.unset {
        match field.as_str() {
            "locator" => miner.locator = None,
            "wing" => miner.wing = None,
            "credential" => miner.credential = None,
            "trigger" => miner.trigger = MinerTrigger::default(),
            other => {
                return Err(invalid(
                    name,
                    format!(
                        "cannot unset `{other}`; the fields that can be cleared are locator, wing, credential and trigger"
                    ),
                ));
            }
        }
    }
    if let Some(source) = patch.source {
        miner.source = source;
    }
    if let Some(enabled) = patch.enabled {
        miner.enabled = enabled;
    }
    if patch.locator.is_some() {
        miner.locator = patch.locator;
    }
    if patch.wing.is_some() {
        miner.wing = patch.wing;
    }
    if patch.credential.is_some() {
        miner.credential = patch.credential;
    }
    if let Some(trigger) = patch.trigger {
        miner.trigger = trigger;
    }
    for key in &patch.unset_scope {
        miner.scope.remove(key);
    }
    miner.scope.extend(patch.scope);
    for key in &patch.unset_config {
        miner.config.remove(key);
    }
    miner.config.extend(patch.config);
    miner.validate().map_err(|reason| invalid(name, reason))?;
    Ok(miner)
}
