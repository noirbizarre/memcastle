//! Trigger configuration and the daemon's side of firing: the `[[triggers]]` of the configuration file, listed, changed
//! and fired, and the [`crate::trigger::Host`] the supervisor runs on.
//!
//! A trigger decides *when* to ask for a mining run and asks for exactly what `memcastle miner run` asks for
//! (`AppServices::request_miner_run`), so it can cause nothing a person could not. Its definition lives in the file
//! the daemon was started from and nowhere else, and so does whether it is enabled: nothing stored in the palace can
//! switch one on, which is what lets a restart, a recovery or a re-install never override the user's choice.
//!
//! Changing triggers is administrative like changing miners (`docs/adr/037`, `docs/adr/043`): not gated by the memory
//! mode and not reachable from MCP, so an agent can read what is configured but cannot decide what runs unattended.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::AppServices;
use super::miners::{CredentialView, credential_available};
use crate::config::WebhookConfig;
use crate::config::miners_file::{self, FileStamp};
use crate::domain::{
    CredentialRef, FireOutcome, JobId, MemoryMode, MinerDefinition, TriggerDefinition,
    TriggerDelivery, TriggerMechanism, TriggerPlan, TriggerState, TriggerStatus,
};
use crate::error::{Error, Result};
use crate::events::{Action, Event};
use crate::mining::AdapterInfo;
use crate::trigger::{self, Desired, FireRequest, ListenerSettings, Note, Runtime};

/// How long a delivery's id is remembered. A sender's retries stop long before a week; past it the id is free again.
const DELIVERY_RETENTION_DAYS: i64 = 7;
/// How long a delivery may sit accepted but not queued before recovery queues it: longer than any request takes, so
/// recovery never races a delivery that is being handled right now.
const RECOVERY_GRACE_SECONDS: i64 = 30;

/// Where the triggers are, and the last good copy of them.
///
/// Works like [`super::MinerRegistry`]: the file is the truth, this holds what was last read from it so a hand edit is
/// noticed, an invalid file does not take the triggers away, and a write can refuse to overwrite an edit it has not
/// seen.
pub struct TriggerRegistry {
    /// The configuration file. `None` for a configuration built in code, where changing triggers is refused.
    path: Option<PathBuf>,
    state: Mutex<RegistryState>,
    webhook: WebhookConfig,
    runtime: Arc<Runtime>,
    /// Held while a request for a run is decided and while a trigger's state is changed, so two requests cannot both
    /// find nothing waiting (and queue two runs) and two updates cannot lose each other's counters.
    fire_lock: Mutex<()>,
}

struct RegistryState {
    triggers: Vec<TriggerDefinition>,
    stamp: Option<Option<FileStamp>>,
    error: Option<String>,
}

impl TriggerRegistry {
    /// A registry over `path`, starting from the triggers the daemon loaded at startup.
    #[must_use]
    pub fn new(
        path: Option<PathBuf>,
        initial: Vec<TriggerDefinition>,
        webhook: WebhookConfig,
        runtime: Arc<Runtime>,
    ) -> Self {
        Self {
            path,
            state: Mutex::new(RegistryState {
                triggers: initial,
                stamp: None,
                error: None,
            }),
            webhook,
            runtime,
            fire_lock: Mutex::new(()),
        }
    }

    fn path(&self) -> Result<&Path> {
        self.path.as_deref().ok_or_else(|| Error::MinerConfigFile {
            path: "-".to_string(),
            reason: "this daemon was not started from a configuration file, so there is nowhere to keep triggers"
                .to_string(),
        })
    }

    /// Read the file again if it changed (or `force`), keeping the last good copy when it no longer parses.
    fn refresh(&self, state: &mut RegistryState, force: bool) -> Result<TriggersDiff> {
        let Some(path) = self.path.as_deref() else {
            return Ok(TriggersDiff::default());
        };
        let current = miners_file::stamp(path).inspect_err(|e| state.error = Some(reason_of(e)))?;
        if !force && state.stamp == Some(current) {
            return Ok(TriggersDiff::default());
        }
        match miners_file::read_triggers(path) {
            Ok(loaded) => {
                let diff = TriggersDiff::between(&state.triggers, &loaded.triggers);
                if !diff.is_empty() {
                    info!(
                        added = ?diff.added, removed = ?diff.removed, changed = ?diff.changed,
                        "trigger configuration reloaded"
                    );
                }
                state.triggers = loaded.triggers;
                state.stamp = Some(loaded.stamp);
                state.error = None;
                Ok(diff)
            }
            Err(error) => {
                // Remembered so the next request does not read the same broken file again.
                state.stamp = Some(current);
                state.error = Some(reason_of(&error));
                warn!(error = %error, "the trigger configuration is invalid; keeping the last good triggers");
                Err(error)
            }
        }
    }
}

impl Default for TriggerRegistry {
    fn default() -> Self {
        Self::new(
            None,
            Vec::new(),
            WebhookConfig::default(),
            Arc::new(Runtime::default()),
        )
    }
}

fn reason_of(error: &Error) -> String {
    match error {
        Error::MinerConfigFile { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

/// What a reload found different, by trigger name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggersDiff {
    /// Triggers that were not there before.
    pub added: Vec<String>,
    /// Triggers that are gone.
    pub removed: Vec<String>,
    /// Triggers whose definition differs.
    pub changed: Vec<String>,
}

impl TriggersDiff {
    fn between(old: &[TriggerDefinition], new: &[TriggerDefinition]) -> Self {
        let mut diff = Self::default();
        for trigger in new {
            match old.iter().find(|t| t.name == trigger.name) {
                None => diff.added.push(trigger.name.clone()),
                Some(before) if before != trigger => diff.changed.push(trigger.name.clone()),
                Some(_) => {}
            }
        }
        diff.removed = old
            .iter()
            .filter(|t| !new.iter().any(|n| n.name == t.name))
            .map(|t| t.name.clone())
            .collect();
        diff
    }

    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// One trigger as the CLI, REST and MCP show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerView {
    /// The trigger's name.
    pub name: String,
    /// The miner it asks to run.
    pub miner: String,
    /// How it decides to.
    #[serde(rename = "type")]
    pub kind: TriggerMechanism,
    /// Whether the user switched it on.
    pub enabled: bool,
    /// Where it stands.
    pub status: TriggerStatus,
    /// Why it is `unavailable` or `failing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What it still needs before it can be enabled, when it cannot be yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<String>,
    /// Whether its task (or the listener it is delivered to) is running right now.
    #[serde(default)]
    pub running: bool,
    /// Its shared secret's reference, as a kind and an availability; never the name, the path or the secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialView>,
    /// Its own settings.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub settings: Map<String, Value>,
    /// For a webhook that is listening: where a sender posts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// When it last asked for a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<DateTime<Utc>>,
    /// The job it last asked for or joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_job: Option<JobId>,
    /// How many runs it asked for.
    #[serde(default)]
    pub fired: u64,
    /// How many requests joined a run already waiting.
    #[serde(default)]
    pub coalesced: u64,
    /// How many deliveries were refused as repeats.
    #[serde(default)]
    pub duplicates: u64,
    /// For a timetable: when it next fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due: Option<DateTime<Utc>>,
    /// The last failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When it happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<DateTime<Utc>>,
    /// How many failures in a row.
    #[serde(default)]
    pub consecutive_failures: u32,
}

/// The webhook listener's state, as shown beside the triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookView {
    /// Whether `webhook.enable` is set. Off by default, and nothing listens while it is off.
    pub enabled: bool,
    /// The address it would bind.
    pub bind: String,
    /// The port it would bind (`0` means the OS chooses).
    pub port: u16,
    /// Whether it may bind beyond loopback.
    pub allow_remote: bool,
    /// Where it is listening now; absent while no webhook trigger is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listening: Option<String>,
}

/// Every trigger, and whether the file they come from is currently readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggersReport {
    /// The triggers, in file order.
    pub triggers: Vec<TriggerView>,
    /// The webhook listener.
    pub webhook: WebhookView,
    /// The file they are kept in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_file: Option<String>,
    /// Why the file cannot be read, when it cannot: `triggers` is then the last good copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A change to one trigger: what to set, and what to clear. A key that is absent is left alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TriggerPatch {
    /// The miner it asks to run.
    pub miner: Option<String>,
    /// How it decides to; needed to create one.
    #[serde(rename = "type")]
    pub kind: Option<TriggerMechanism>,
    /// Switch it on or off. A new trigger is off unless this says otherwise.
    pub enabled: Option<bool>,
    /// Where a webhook's shared secret comes from.
    pub credential: Option<CredentialRef>,
    /// Settings to set.
    pub settings: Map<String, Value>,
    /// Settings to remove.
    pub unset_settings: Vec<String>,
    /// Fields to clear: `credential`.
    pub unset: Vec<String>,
}

/// The outcome of [`AppServices::set_trigger`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerChange {
    /// The trigger as it is now.
    pub trigger: TriggerView,
    /// Whether it did not exist before.
    pub created: bool,
    /// Whether anything was written; false when the request changed nothing.
    pub changed: bool,
}

/// The outcome of [`AppServices::reload_triggers`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggersReload {
    /// How many triggers are configured now.
    pub triggers: usize,
    /// What differs from what the daemon had.
    #[serde(flatten)]
    pub diff: TriggersDiff,
}

fn invalid(name: &str, reason: impl Into<String>) -> Error {
    Error::TriggerInvalid {
        name: name.to_string(),
        reason: reason.into(),
    }
}

fn find_trigger<'a>(
    triggers: &'a [TriggerDefinition],
    name: &str,
) -> Result<&'a TriggerDefinition> {
    triggers
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| Error::TriggerNotFound {
            name: name.to_string(),
        })
}

/// The trigger `patch` turns `existing` into, or a new one; checked for shape.
fn apply(
    name: &str,
    existing: Option<&TriggerDefinition>,
    patch: TriggerPatch,
) -> Result<TriggerDefinition> {
    let mut trigger = match existing {
        Some(existing) => existing.clone(),
        None => {
            let (Some(miner), Some(kind)) = (patch.miner.clone(), patch.kind) else {
                return Err(invalid(
                    name,
                    format!(
                        "there is no trigger `{name}` to change, and a new one needs a `miner` and a `type`; \
                         `memcastle trigger set {name} --miner <miner> --type <schedule|poll|webhook|watch>` creates it"
                    ),
                ));
            };
            TriggerDefinition {
                name: name.to_string(),
                miner,
                kind,
                // Off until the user says otherwise: writing a trigger down never starts one.
                enabled: false,
                credential: None,
                settings: Map::new(),
            }
        }
    };
    for field in &patch.unset {
        match field.as_str() {
            "credential" => trigger.credential = None,
            other => {
                return Err(invalid(
                    name,
                    format!(
                        "cannot unset `{other}`; the only field that can be cleared is credential"
                    ),
                ));
            }
        }
    }
    if let Some(miner) = patch.miner {
        trigger.miner = miner;
    }
    if let Some(kind) = patch.kind {
        // A different mechanism has different settings; carrying the old ones over would be refused as unknown at best.
        if kind != trigger.kind && existing.is_some() {
            trigger.settings.clear();
        }
        trigger.kind = kind;
    }
    if let Some(enabled) = patch.enabled {
        trigger.enabled = enabled;
    }
    if patch.credential.is_some() {
        trigger.credential = patch.credential;
    }
    for key in &patch.unset_settings {
        trigger.settings.remove(key);
    }
    trigger.settings.extend(patch.settings);
    trigger.validate().map_err(|reason| invalid(name, reason))?;
    Ok(trigger)
}

impl AppServices {
    /// Give the services the trigger registry the daemon built from its configuration file.
    #[must_use]
    pub fn with_triggers(mut self, triggers: TriggerRegistry) -> Self {
        self.triggers = Arc::new(triggers);
        self
    }

    /// What the supervisor runs on: this daemon, as a [`trigger::Host`].
    #[must_use]
    pub fn trigger_host(&self) -> TriggerHost {
        TriggerHost { app: self.clone() }
    }

    /// What the supervisor shares with the daemon: wake-ups, and what it is running.
    #[must_use]
    pub fn trigger_runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.triggers.runtime)
    }

    /// Every configured trigger with its state.
    ///
    /// Gated as a **read**. A hand edit made since the last call is picked up first; if the file no longer parses the
    /// last good triggers are reported with the error beside them, not an error.
    ///
    /// # Errors
    ///
    /// [`Error::ModeForbidden`] if `mode` forbids reads, or a store error.
    pub async fn list_triggers(&self, mode: MemoryMode) -> Result<TriggersReport> {
        Self::require_read(mode, "trigger_list")?;
        let (triggers, error) = {
            let mut state = self.triggers.state.lock().await;
            let _ = self.triggers.refresh(&mut state, false);
            (state.triggers.clone(), state.error.clone())
        };
        let (miners, adapters) = self.activation_context().await?;
        let mut views = Vec::with_capacity(triggers.len());
        for trigger in &triggers {
            views.push(self.trigger_view(trigger, &miners, &adapters).await?);
        }
        Ok(TriggersReport {
            triggers: views,
            webhook: self.webhook_view(),
            config_file: self.triggers.path.as_ref().map(|p| p.display().to_string()),
            error,
        })
    }

    /// One trigger.
    ///
    /// # Errors
    ///
    /// [`Error::TriggerNotFound`], [`Error::ModeForbidden`] if `mode` forbids reads, or a store error.
    pub async fn show_trigger(&self, name: &str, mode: MemoryMode) -> Result<TriggerView> {
        Self::require_read(mode, "trigger_get")?;
        let trigger = {
            let mut state = self.triggers.state.lock().await;
            let _ = self.triggers.refresh(&mut state, false);
            find_trigger(&state.triggers, name)?.clone()
        };
        let (miners, adapters) = self.activation_context().await?;
        self.trigger_view(&trigger, &miners, &adapters).await
    }

    /// Create a trigger, or change one: the same call for both, so `trigger set` is idempotent.
    ///
    /// Everything is checked before anything is written. A new trigger is **disabled**; one is enabled only by saying
    /// so, and only when everything it needs is in place, so a refused change leaves the file untouched and nothing
    /// is started.
    ///
    /// Administrative: not gated by the memory mode, and not reachable from MCP.
    ///
    /// # Errors
    ///
    /// [`Error::TriggerInvalid`], [`Error::TriggerNotActivatable`], [`Error::MinerConfigFile`] when the file is
    /// unreadable or changed underneath, and store errors.
    pub async fn set_trigger(&self, name: &str, patch: TriggerPatch) -> Result<TriggerChange> {
        self.apply_trigger_patch(name, patch, false).await
    }

    /// Switch a trigger on or off. Idempotent. Switching on checks every prerequisite, like [`Self::set_trigger`].
    ///
    /// # Errors
    ///
    /// [`Error::TriggerNotFound`] and the errors of [`Self::set_trigger`].
    pub async fn set_trigger_enabled(&self, name: &str, enabled: bool) -> Result<TriggerChange> {
        let patch = TriggerPatch {
            enabled: Some(enabled),
            ..TriggerPatch::default()
        };
        self.apply_trigger_patch(name, patch, true).await
    }

    /// Remove a trigger's definition and what the daemon remembers about it. The miner and what it mined stay.
    ///
    /// # Errors
    ///
    /// [`Error::TriggerNotFound`] and [`Error::MinerConfigFile`].
    pub async fn remove_trigger(&self, name: &str) -> Result<()> {
        let path = self.triggers.path()?;
        {
            let mut state = self.triggers.state.lock().await;
            self.refresh_triggers_for_write(&mut state)?;
            find_trigger(&state.triggers, name)?;
            let remaining: Vec<TriggerDefinition> = state
                .triggers
                .iter()
                .filter(|t| t.name != name)
                .cloned()
                .collect();
            let stamp = miners_file::write_triggers(path, state.stamp.flatten(), &remaining)?;
            state.triggers = remaining;
            state.stamp = Some(stamp);
        }
        let _guard = self.triggers.fire_lock.lock().await;
        self.store.delete_trigger_records(name).await?;
        info!(trigger = %name, "trigger removed");
        self.announce(Event::trigger(Action::Deleted, name, "changed"));
        self.triggers.runtime.wake();
        Ok(())
    }

    /// Read the configuration file again now, whether or not it looks changed, and say what differs.
    ///
    /// # Errors
    ///
    /// [`Error::MinerConfigFile`] when the file cannot be read or does not validate; the daemon keeps the last
    /// triggers.
    pub async fn reload_triggers(&self) -> Result<TriggersReload> {
        self.triggers.path()?;
        let diff;
        let count;
        {
            let mut state = self.triggers.state.lock().await;
            diff = self.triggers.refresh(&mut state, true)?;
            count = state.triggers.len();
        }
        self.triggers.runtime.wake();
        Ok(TriggersReload {
            triggers: count,
            diff,
        })
    }

    /// Ask for a run through a trigger, now: the manual way to fire it, which takes the same path as every other.
    ///
    /// Refused while the trigger is disabled, so "disabled" means the same thing for every way of firing it (to mine
    /// without a trigger, `memcastle miner run`). Gated as a **write**, like any mining.
    ///
    /// # Errors
    ///
    /// [`Error::TriggerNotFound`], [`Error::TriggerDisabled`], [`Error::ModeForbidden`], and the errors of running the
    /// miner (`Error::MinerDisabled`, `Error::MinerNotRunnable`, ...).
    pub async fn fire_trigger(&self, name: &str, mode: MemoryMode) -> Result<FireOutcome> {
        Self::require_write(mode, "trigger_fire")?;
        self.fire_inner(name, None, None).await
    }

    /// Queue the deliveries a crash left accepted but not queued. Called once at startup, after the scheduler recovered.
    ///
    /// Idempotent and safe to repeat: the mining pipeline skips what has not changed, so a delivery that did reach a
    /// job before the crash costs a pass, not a duplicate. A trigger that is disabled now is *not* run (a restart never
    /// overrides the user's choice), and its pending deliveries are dropped.
    pub async fn recover_triggers(&self) {
        let pending = match self.store.list_unqueued_trigger_deliveries().await {
            Ok(pending) => pending,
            Err(error) => {
                warn!(%error, "could not look for trigger deliveries that were never queued");
                return;
            }
        };
        let cutoff = Utc::now() - chrono::Duration::seconds(RECOVERY_GRACE_SECONDS);
        for delivery in pending.into_iter().filter(|d| d.received_at <= cutoff) {
            let _guard = self.triggers.fire_lock.lock().await;
            let enabled = {
                let mut state = self.triggers.state.lock().await;
                let _ = self.triggers.refresh(&mut state, false);
                state
                    .triggers
                    .iter()
                    .any(|t| t.name == delivery.trigger && t.enabled)
            };
            if !enabled {
                // Forgotten rather than left owing a run to a trigger the user switched off.
                let _ = self
                    .store
                    .delete_trigger_delivery(&delivery.trigger, &delivery.key)
                    .await;
                continue;
            }
            match self
                .request_trigger_run(&delivery.trigger, Some(TriggerMechanism::Webhook))
                .await
            {
                Ok((job, _)) => {
                    let _ = self
                        .store
                        .set_trigger_delivery_job(&delivery.trigger, &delivery.key, job)
                        .await;
                    info!(trigger = %delivery.trigger, %job, "queued a delivery that was accepted before a restart");
                }
                Err(error) => {
                    warn!(trigger = %delivery.trigger, %error, "could not queue a delivery accepted before a restart");
                }
            }
        }
    }

    // ---- internals ----------------------------------------------------------------------------------------------

    fn refresh_triggers_for_write(&self, state: &mut RegistryState) -> Result<()> {
        // A hand edit since the last request is applied first, so the write is built on what is in the file.
        self.triggers.refresh(state, false)?;
        if let Some(reason) = &state.error {
            return Err(Error::MinerConfigFile {
                path: self.triggers.path()?.display().to_string(),
                reason: reason.clone(),
            });
        }
        Ok(())
    }

    fn webhook_view(&self) -> WebhookView {
        let config = &self.triggers.webhook;
        WebhookView {
            enabled: config.enable,
            bind: config.bind.to_string(),
            port: config.port,
            allow_remote: config.allow_remote,
            listening: self.triggers.runtime.webhook_addr().map(|a| a.to_string()),
        }
    }

    /// The miners and the sources a trigger is checked against, read once so a list is judged on one snapshot.
    async fn activation_context(&self) -> Result<(Vec<MinerDefinition>, Vec<AdapterInfo>)> {
        let miners = self.miners.snapshot().await;
        let adapters = crate::mining::registry::list_adapters(&self.store, &self.mining).await?;
        Ok((miners, adapters))
    }

    /// What the listener may do: where it binds, or why it must not.
    fn listener_settings(&self) -> std::result::Result<ListenerSettings, String> {
        let config = &self.triggers.webhook;
        if !config.enable {
            return Err("webhooks are off: set `[webhook] enable = true` (or MEMCASTLE_WEBHOOK_ENABLE=true) in the \
                        daemon's configuration and restart it. Nothing listens until you do"
                .to_string());
        }
        if !config.bind.is_loopback() && !self.auth_enabled() {
            return Err(format!(
                "`webhook.bind` is {}, which is not loopback, and this daemon has authentication disabled; enable \
                 `[auth]` first, or bind the listener to 127.0.0.1 and put a reverse proxy in front of it",
                config.bind
            ));
        }
        Ok(ListenerSettings {
            addr: SocketAddr::new(config.bind, config.port),
            max_body_bytes: config.max_body_bytes,
            max_concurrent: config.max_concurrent,
        })
    }

    /// Everything an enabled trigger needs, as one sentence per thing missing, and what to do about it; the plan when
    /// nothing is.
    async fn trigger_activation(
        &self,
        trigger: &TriggerDefinition,
        miners: &[MinerDefinition],
        adapters: &[AdapterInfo],
    ) -> std::result::Result<TriggerPlan, String> {
        let plan = trigger.plan()?;
        let Some(miner) = miners.iter().find(|m| m.name == trigger.miner) else {
            return Err(format!(
                "the miner `{0}` does not exist; `memcastle miner set {0} --source <source>` creates it",
                trigger.miner
            ));
        };
        if !miner.enabled {
            return Err(format!(
                "the miner `{0}` is disabled, so a run could not start; `memcastle miner enable {0}` enables it",
                miner.name
            ));
        }
        if let Err(reason) = self.check_activation(miner).await {
            return Err(format!("the miner `{}` cannot run: {reason}", miner.name));
        }
        let supported = adapters
            .iter()
            .find(|a| a.name == miner.source)
            .is_some_and(|a| a.triggers.iter().any(|t| t.kind == trigger.kind));
        if !supported {
            return Err(format!(
                "the source `{}` does not declare support for `{}` triggers; `memcastle sources` shows what each \
                 source supports",
                miner.source, trigger.kind
            ));
        }
        match &plan {
            TriggerPlan::Webhook(_) => {
                self.listener_settings()?;
                if let Some(credential) = &trigger.credential
                    && !credential_available(credential)
                {
                    return Err(match credential {
                        CredentialRef::Env { name } => format!(
                            "the webhook's secret is read from the environment variable `{name}`, which is not set or \
                             is empty in the daemon's environment; export it and restart the daemon"
                        ),
                        _ => "the webhook's secret file does not exist; create it with the shared secret in it"
                            .to_string(),
                    });
                }
            }
            TriggerPlan::Watch { path, .. } if !path.exists() => {
                return Err(format!(
                    "the watched path {} does not exist; create it, or change the trigger's `path`",
                    path.display()
                ));
            }
            _ => {}
        }
        Ok(plan)
    }

    async fn trigger_view(
        &self,
        trigger: &TriggerDefinition,
        miners: &[MinerDefinition],
        adapters: &[AdapterInfo],
    ) -> Result<TriggerView> {
        let state = self
            .store
            .get_trigger_state(&trigger.name)
            .await?
            .unwrap_or_else(|| TriggerState::new(&trigger.name));
        let activation = self.trigger_activation(trigger, miners, adapters).await;
        let setup: Vec<String> = activation.as_ref().err().cloned().into_iter().collect();
        let (status, reason) = if !trigger.enabled {
            (TriggerStatus::Disabled, None)
        } else if let Err(reason) = &activation {
            (TriggerStatus::Unavailable, Some(reason.clone()))
        } else if state.consecutive_failures > 0 {
            (TriggerStatus::Failing, state.last_error.clone())
        } else {
            (TriggerStatus::Active, None)
        };
        let running = trigger.enabled && self.triggers.runtime.is_running(&trigger.name);
        let endpoint = (running && trigger.kind == TriggerMechanism::Webhook)
            .then(|| self.triggers.runtime.webhook_addr())
            .flatten()
            .map(|addr| format!("http://{addr}/hooks/{}", trigger.name));
        let credential = trigger.credential.as_ref().map(|c| CredentialView {
            kind: match c {
                CredentialRef::Env { .. } => "env",
                CredentialRef::File { .. } => "file",
                CredentialRef::Oauth => "oauth",
            }
            .to_string(),
            available: credential_available(c),
        });
        Ok(TriggerView {
            name: trigger.name.clone(),
            miner: trigger.miner.clone(),
            kind: trigger.kind,
            enabled: trigger.enabled,
            status,
            reason,
            setup,
            running,
            credential,
            settings: trigger.settings.clone(),
            endpoint,
            last_fired_at: state.last_fired_at,
            last_job: state.last_job,
            fired: state.fired,
            coalesced: state.coalesced,
            duplicates: state.duplicates,
            next_due: state.next_due,
            last_error: state.last_error,
            last_error_at: state.last_error_at,
            consecutive_failures: state.consecutive_failures,
        })
    }

    async fn apply_trigger_patch(
        &self,
        name: &str,
        patch: TriggerPatch,
        must_exist: bool,
    ) -> Result<TriggerChange> {
        let path = self.triggers.path()?;
        let mut state = self.triggers.state.lock().await;
        self.refresh_triggers_for_write(&mut state)?;

        let existing = state.triggers.iter().find(|t| t.name == name).cloned();
        if must_exist && existing.is_none() {
            return Err(Error::TriggerNotFound {
                name: name.to_string(),
            });
        }
        let candidate = apply(name, existing.as_ref(), patch)?;
        let changed = existing.as_ref() != Some(&candidate);
        // Only a trigger that is going to run is held to what running needs: a disabled one may be written ahead of
        // the miner, the secret or the listener it will need, and says what is missing when asked.
        if candidate.enabled && changed {
            let (miners, adapters) = self.activation_context().await?;
            self.trigger_activation(&candidate, &miners, &adapters)
                .await
                .map_err(|reason| Error::TriggerNotActivatable {
                    name: name.to_string(),
                    reason,
                })?;
        }

        let created = existing.is_none();
        if changed {
            let mut all = state.triggers.clone();
            match all.iter_mut().find(|t| t.name == name) {
                Some(slot) => *slot = candidate.clone(),
                None => all.push(candidate.clone()),
            }
            let stamp = miners_file::write_triggers(path, state.stamp.flatten(), &all)?;
            state.triggers = all;
            state.stamp = Some(stamp);
            info!(trigger = %name, created, enabled = candidate.enabled, "trigger configuration written");
        }
        drop(state);

        if changed {
            if existing.as_ref().is_some_and(|before| !before.enabled) && candidate.enabled {
                // Switched on after a while off: its first request is a whole interval away, not whatever was
                // pending when it was switched off.
                let _guard = self.triggers.fire_lock.lock().await;
                if let Some(mut remembered) = self.store.get_trigger_state(name).await? {
                    remembered.next_due = None;
                    self.store.save_trigger_state(&remembered).await?;
                }
            }
            self.announce(Event::trigger(
                if created {
                    Action::Created
                } else {
                    Action::Updated
                },
                name,
                "changed",
            ));
            self.triggers.runtime.wake();
        }
        let (miners, adapters) = self.activation_context().await?;
        Ok(TriggerChange {
            trigger: self.trigger_view(&candidate, &miners, &adapters).await?,
            created,
            changed,
        })
    }

    /// Ask the miner behind trigger `name` for a run, joining one already waiting. The one place a trigger's request
    /// turns into a job, whatever fired it.
    async fn request_trigger_run(
        &self,
        name: &str,
        via: Option<TriggerMechanism>,
    ) -> Result<(JobId, bool)> {
        let miner = {
            let mut state = self.triggers.state.lock().await;
            let _ = self.triggers.refresh(&mut state, false);
            find_trigger(&state.triggers, name)?.miner.clone()
        };
        let requested_by = match via {
            Some(_) => format!("trigger:{name}"),
            None => format!("trigger:{name}:manual"),
        };
        // A trigger is the user's own standing instruction, not a client session, so no session's memory mode applies.
        let (job, joined) = self
            .request_miner_run(&miner, false, &requested_by, MemoryMode::Full, true)
            .await?;
        Ok((job.id, joined))
    }

    /// Ask for a run on behalf of a trigger and record what came of it.
    async fn fire_inner(
        &self,
        name: &str,
        via: Option<TriggerMechanism>,
        delivery: Option<String>,
    ) -> Result<FireOutcome> {
        let _guard = self.triggers.fire_lock.lock().await;
        let trigger = {
            let mut state = self.triggers.state.lock().await;
            let _ = self.triggers.refresh(&mut state, false);
            find_trigger(&state.triggers, name)?.clone()
        };
        if !trigger.enabled {
            return Err(Error::TriggerDisabled {
                name: name.to_string(),
            });
        }
        let mut state = self
            .store
            .get_trigger_state(name)
            .await?
            .unwrap_or_else(|| TriggerState::new(name));

        if let Some(key) = &delivery {
            let accepted = self
                .store
                .create_trigger_delivery_once(&TriggerDelivery {
                    trigger: name.to_string(),
                    key: key.clone(),
                    received_at: Utc::now(),
                    job: None,
                })
                .await?;
            if !accepted {
                state.duplicates += 1;
                self.store.save_trigger_state(&state).await?;
                return Ok(FireOutcome::Duplicate);
            }
        }

        match self.request_trigger_run(name, via).await {
            Ok((job, joined)) => {
                if let Some(key) = &delivery {
                    self.store.set_trigger_delivery_job(name, key, job).await?;
                }
                state.last_fired_at = Some(Utc::now());
                state.last_job = Some(job);
                if joined {
                    state.coalesced += 1;
                } else {
                    state.fired += 1;
                }
                state.succeeded();
                self.store.save_trigger_state(&state).await?;
                self.announce(Event::trigger(
                    Action::Updated,
                    name,
                    if joined { "coalesced" } else { "queued" },
                ));
                Ok(if joined {
                    FireOutcome::Coalesced { job }
                } else {
                    FireOutcome::Queued { job }
                })
            }
            Err(error) => {
                // The delivery is forgotten so the sender's retry is a fresh attempt, not a "duplicate" of one that
                // never ran.
                if let Some(key) = &delivery {
                    let _ = self.store.delete_trigger_delivery(name, key).await;
                }
                // A person who asks and is refused has the refusal in front of them; only the unattended failures are
                // something to find later.
                if via.is_some() {
                    state.failed(error.to_string(), Utc::now());
                    self.store.save_trigger_state(&state).await?;
                    self.announce(Event::trigger(Action::Updated, name, "failed"));
                }
                Err(error)
            }
        }
    }

    /// What the secret of a webhook trigger is right now, read from where the trigger points.
    async fn webhook_secret(&self, trigger: &TriggerDefinition) -> Option<Vec<u8>> {
        let secret = match trigger.credential.as_ref()? {
            CredentialRef::Env { name } => std::env::var(name).ok()?.into_bytes(),
            CredentialRef::File { path } => tokio::fs::read(path).await.ok()?,
            // A webhook's secret is a shared one, never an OAuth sign-in (the definition refuses it).
            CredentialRef::Oauth => return None,
        };
        // A trailing newline is what `echo secret > file` leaves, and no sender signs with it.
        let trimmed = secret.trim_ascii();
        (!trimmed.is_empty()).then(|| trimmed.to_vec())
    }
}

/// The daemon as the trigger supervisor sees it.
#[derive(Clone)]
pub struct TriggerHost {
    app: AppServices,
}

impl trigger::Host for TriggerHost {
    async fn desired(&self) -> Desired {
        let app = &self.app;
        let enabled: Vec<TriggerDefinition> = {
            let mut state = app.triggers.state.lock().await;
            let _ = app.triggers.refresh(&mut state, false);
            state
                .triggers
                .iter()
                .filter(|t| t.enabled)
                .cloned()
                .collect()
        };
        if enabled.is_empty() {
            return Desired::default();
        }
        let Ok((miners, adapters)) = app.activation_context().await else {
            // The store could not be read: keep what is running rather than stop everything on a hiccup. An empty
            // answer would stop it, so retry on the next tick through the caller's loop.
            return Desired::default();
        };
        let mut triggers = Vec::new();
        for trigger in enabled {
            match app.trigger_activation(&trigger, &miners, &adapters).await {
                Ok(plan) => triggers.push((trigger, plan)),
                Err(reason) => {
                    // Not running, and the view says why; once is enough in the log.
                    tracing::debug!(trigger = %trigger.name, %reason, "an enabled trigger is not runnable");
                }
            }
        }
        Desired {
            triggers,
            listener: app.listener_settings().ok(),
        }
    }

    async fn fire(&self, request: FireRequest) -> std::result::Result<FireOutcome, String> {
        self.app
            .fire_inner(&request.trigger, request.via, request.delivery)
            .await
            .map_err(|e| e.to_string())
    }

    async fn state(&self, name: &str) -> Option<TriggerState> {
        self.app.store.get_trigger_state(name).await.ok().flatten()
    }

    async fn note(&self, name: &str, note: Note) {
        let app = &self.app;
        let _guard = app.triggers.fire_lock.lock().await;
        let mut state = match app.store.get_trigger_state(name).await {
            Ok(state) => state.unwrap_or_else(|| TriggerState::new(name)),
            Err(error) => {
                warn!(trigger = %name, %error, "could not read a trigger's state to update it");
                return;
            }
        };
        let before = state.clone();
        match note {
            Note::NextDue(due) => state.next_due = due,
            Note::Failed(reason) => {
                state.failed(reason, Utc::now());
                app.announce(Event::trigger(Action::Updated, name, "failed"));
            }
            Note::Healthy => state.succeeded(),
        }
        if state != before
            && let Err(error) = app.store.save_trigger_state(&state).await
        {
            warn!(trigger = %name, %error, "could not save a trigger's state");
        }
    }

    async fn secret(&self, trigger: &TriggerDefinition) -> Option<Vec<u8>> {
        self.app.webhook_secret(trigger).await
    }

    async fn maintain(&self) {
        let before = Utc::now() - chrono::Duration::days(DELIVERY_RETENTION_DAYS);
        if let Err(error) = self.app.store.prune_trigger_deliveries(before).await {
            warn!(%error, "could not prune old trigger deliveries");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{MinerPatch, MinerRegistry};
    use crate::domain::MemoryMode;
    use crate::jobs::Scheduler;
    use crate::store::SurrealStore;
    use crate::trigger::Host as _;

    fn definition(text: &str) -> TriggerDefinition {
        toml::from_str(text).expect("a trigger")
    }

    #[test]
    fn a_new_trigger_is_created_disabled_even_when_the_patch_is_silent_about_it() {
        let patch = TriggerPatch {
            miner: Some("docs".to_string()),
            kind: Some(TriggerMechanism::Poll),
            settings: serde_json::json!({"every": "5m"})
                .as_object()
                .cloned()
                .unwrap(),
            ..TriggerPatch::default()
        };
        let created = apply("t", None, patch).expect("valid");
        assert!(!created.enabled);
    }

    #[test]
    fn a_new_trigger_needs_a_miner_and_a_type_and_says_how_to_give_them() {
        let error = apply("t", None, TriggerPatch::default()).expect_err("incomplete");
        assert!(error.to_string().contains("--miner"), "{error}");
    }

    #[test]
    fn changing_the_type_drops_settings_the_new_type_does_not_have() {
        let existing = definition("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"5m\"\n");
        let patch = TriggerPatch {
            kind: Some(TriggerMechanism::Watch),
            settings: serde_json::json!({"path": "/n"})
                .as_object()
                .cloned()
                .unwrap(),
            ..TriggerPatch::default()
        };
        let changed = apply("t", Some(&existing), patch).expect("valid");
        assert_eq!(changed.kind, TriggerMechanism::Watch);
        assert!(
            !changed.settings.contains_key("every"),
            "{:?}",
            changed.settings
        );
    }

    #[test]
    fn a_diff_names_what_was_added_changed_and_removed() {
        let a = definition("name = \"a\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"5m\"\n");
        let mut b = a.clone();
        b.name = "b".to_string();
        let mut a2 = a.clone();
        a2.enabled = true;
        let diff = TriggersDiff::between(
            &[a.clone(), b],
            &[
                a2,
                definition("name = \"c\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"5m\"\n"),
            ],
        );
        assert_eq!(diff.added, ["c"]);
        assert_eq!(diff.changed, ["a"]);
        assert_eq!(diff.removed, ["b"]);
    }

    /// A service over an in-memory palace whose scheduler is never started, so every job stays queued where a test can
    /// see it, with one `directory` miner (`docs`) ready to run.
    async fn app_with_a_miner() -> (AppServices, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let notes = dir.path().join("notes");
        std::fs::create_dir(&notes).expect("notes dir");
        std::fs::write(notes.join("a.txt"), "a note").expect("a note");
        let config = dir.path().join("config.toml");
        let store = SurrealStore::connect_memory_for_tests().await;
        let scheduler = Arc::new(Scheduler::new(store.clone(), 1));
        let app = AppServices::new(store, scheduler)
            .with_miners(MinerRegistry::new(Some(config.clone()), Vec::new()))
            .with_triggers(TriggerRegistry::new(
                Some(config),
                Vec::new(),
                WebhookConfig::default(),
                Arc::new(Runtime::default()),
            ));
        app.set_miner(
            "docs",
            MinerPatch {
                source: Some("directory".to_string()),
                locator: Some(notes.display().to_string()),
                ..MinerPatch::default()
            },
        )
        .await
        .expect("a miner");
        (app, dir)
    }

    fn poll_patch(enabled: bool) -> TriggerPatch {
        TriggerPatch {
            miner: Some("docs".to_string()),
            kind: Some(TriggerMechanism::Poll),
            enabled: Some(enabled),
            settings: serde_json::json!({"every": "1h"})
                .as_object()
                .cloned()
                .unwrap(),
            ..TriggerPatch::default()
        }
    }

    async fn queued_mines(app: &AppServices) -> usize {
        app.store
            .list_jobs(Some(crate::domain::JobStatus::Queued))
            .await
            .expect("jobs")
            .len()
    }

    #[tokio::test]
    async fn a_trigger_is_saved_disabled_and_a_disabled_trigger_cannot_be_fired() {
        let (app, _dir) = app_with_a_miner().await;
        let change = app
            .set_trigger("t", poll_patch(false))
            .await
            .expect("saved");
        assert!(change.created && !change.trigger.enabled);
        assert_eq!(change.trigger.status, TriggerStatus::Disabled);

        let error = app
            .fire_trigger("t", MemoryMode::Full)
            .await
            .expect_err("disabled");
        assert!(matches!(error, Error::TriggerDisabled { .. }), "{error}");
        assert_eq!(
            queued_mines(&app).await,
            0,
            "a disabled trigger asks for nothing"
        );
    }

    #[tokio::test]
    async fn firing_queues_one_run_and_the_next_requests_join_it_while_it_waits() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");

        let first = app
            .fire_trigger("t", MemoryMode::Full)
            .await
            .expect("fired");
        let FireOutcome::Queued { job } = first else {
            panic!("the first request queues a run: {first:?}");
        };
        for _ in 0..5 {
            let again = app
                .fire_trigger("t", MemoryMode::Full)
                .await
                .expect("fired");
            assert_eq!(
                again,
                FireOutcome::Coalesced { job },
                "joins the waiting run"
            );
        }
        assert_eq!(queued_mines(&app).await, 1, "a burst is one job, not six");

        let view = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!((view.fired, view.coalesced), (1, 5));
        assert_eq!(view.last_job, Some(job));
    }

    #[tokio::test]
    async fn a_trigger_asks_for_the_same_run_a_person_would_and_says_who_asked() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        let host = app.trigger_host();

        host.fire(FireRequest {
            trigger: "t".to_string(),
            via: Some(TriggerMechanism::Poll),
            delivery: None,
        })
        .await
        .expect("fired");
        let jobs = app.store.list_jobs(None).await.expect("jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].requested_by, "trigger:t");
        let person = app
            .run_miner("docs", false, "cli", MemoryMode::Full)
            .await
            .expect("a person's run");
        match (&jobs[0].kind, &person.kind) {
            (
                crate::domain::JobKind::Mine {
                    source: a,
                    options: oa,
                    ..
                },
                crate::domain::JobKind::Mine {
                    source: b,
                    options: ob,
                    ..
                },
            ) => {
                assert_eq!(
                    serde_json::to_value(a).unwrap(),
                    serde_json::to_value(b).unwrap(),
                    "the same source, from the same path"
                );
                assert_eq!(oa, ob);
            }
            other => panic!("both are mining jobs: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_repeated_delivery_is_answered_but_not_run_again() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        let host = app.trigger_host();
        let request = |id: &str| FireRequest {
            trigger: "t".to_string(),
            via: Some(TriggerMechanism::Webhook),
            delivery: Some(id.to_string()),
        };

        assert!(matches!(
            host.fire(request("d-1")).await,
            Ok(FireOutcome::Queued { .. })
        ));
        assert_eq!(host.fire(request("d-1")).await, Ok(FireOutcome::Duplicate));
        assert!(
            matches!(
                host.fire(request("d-2")).await,
                Ok(FireOutcome::Coalesced { .. })
            ),
            "another delivery is another request, which joins the waiting run"
        );
        assert_eq!(queued_mines(&app).await, 1);
        let view = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!(view.duplicates, 1);
    }

    #[tokio::test]
    async fn a_delivery_that_could_not_be_queued_is_not_remembered_so_the_retry_runs() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        let host = app.trigger_host();
        let request = FireRequest {
            trigger: "t".to_string(),
            via: Some(TriggerMechanism::Webhook),
            delivery: Some("d-1".to_string()),
        };

        // The miner is switched off under the trigger, so asking for its run fails.
        app.set_miner_enabled("docs", false)
            .await
            .expect("disabled");
        assert!(host.fire(request.clone()).await.is_err());
        let failing = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!(failing.consecutive_failures, 1);
        assert!(failing.last_error.is_some());

        app.set_miner_enabled("docs", true).await.expect("enabled");
        assert!(
            matches!(host.fire(request).await, Ok(FireOutcome::Queued { .. })),
            "the sender's retry is a new attempt, not a repeat of one that never ran"
        );
        let healthy = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!(healthy.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn failures_make_a_trigger_failing_and_health_clears_it_but_keeps_the_record() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        let host = app.trigger_host();

        host.note("t", Note::Failed("the watcher broke".to_string()))
            .await;
        host.note("t", Note::Failed("the watcher broke".to_string()))
            .await;
        let failing = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!(failing.status, TriggerStatus::Failing);
        assert_eq!(failing.consecutive_failures, 2);
        assert_eq!(failing.reason.as_deref(), Some("the watcher broke"));

        host.note("t", Note::Healthy).await;
        let healthy = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!(healthy.status, TriggerStatus::Active);
        assert_eq!(healthy.last_error.as_deref(), Some("the watcher broke"));
    }

    #[tokio::test]
    async fn a_delivery_accepted_before_a_crash_is_queued_on_recovery_only_while_its_trigger_is_enabled()
     {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("kept", poll_patch(true))
            .await
            .expect("enabled");
        app.set_trigger("off", poll_patch(false))
            .await
            .expect("disabled");
        let old = Utc::now() - chrono::Duration::minutes(5);
        for trigger in ["kept", "off"] {
            let delivery = TriggerDelivery {
                trigger: trigger.to_string(),
                key: "d".to_string(),
                received_at: old,
                job: None,
            };
            assert!(
                app.store
                    .create_trigger_delivery_once(&delivery)
                    .await
                    .unwrap()
            );
        }

        app.recover_triggers().await;

        assert_eq!(
            queued_mines(&app).await,
            1,
            "only the enabled trigger's delivery is owed a run"
        );
        let kept = app
            .store
            .get_trigger_delivery("kept", "d")
            .await
            .unwrap()
            .unwrap();
        assert!(kept.job.is_some());
        assert!(
            app.store
                .get_trigger_delivery("off", "d")
                .await
                .unwrap()
                .is_none(),
            "a restart never overrides the user's choice to switch a trigger off"
        );
    }

    #[tokio::test]
    async fn a_delivery_still_being_handled_is_not_recovered_out_from_under_its_request() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        let fresh = TriggerDelivery {
            trigger: "t".to_string(),
            key: "d".to_string(),
            received_at: Utc::now(),
            job: None,
        };
        assert!(
            app.store
                .create_trigger_delivery_once(&fresh)
                .await
                .unwrap()
        );
        app.recover_triggers().await;
        assert_eq!(queued_mines(&app).await, 0);
    }

    #[tokio::test]
    async fn enabling_is_refused_with_what_is_missing_and_nothing_is_written() {
        let (app, dir) = app_with_a_miner().await;
        let webhook = TriggerPatch {
            miner: Some("docs".to_string()),
            kind: Some(TriggerMechanism::Webhook),
            credential: Some(CredentialRef::File {
                path: dir.path().join("secret").display().to_string(),
            }),
            ..TriggerPatch::default()
        };
        // Saved disabled: it may be written ahead of what it needs.
        app.set_trigger("hook", webhook)
            .await
            .expect("saved disabled");
        let view = app
            .show_trigger("hook", MemoryMode::Full)
            .await
            .expect("view");
        assert!(
            view.setup.iter().any(|s| s.contains("[webhook]")),
            "{:?}",
            view.setup
        );

        let error = app
            .set_trigger_enabled("hook", true)
            .await
            .expect_err("refused");
        match error {
            Error::TriggerNotActivatable { reason, .. } => {
                assert!(reason.contains("webhooks are off"), "{reason}")
            }
            other => panic!("{other}"),
        }
        let still = app
            .show_trigger("hook", MemoryMode::Full)
            .await
            .expect("view");
        assert!(!still.enabled, "a refused enable changes nothing");

        let missing = TriggerPatch {
            miner: Some("nobody".to_string()),
            kind: Some(TriggerMechanism::Poll),
            enabled: Some(true),
            settings: serde_json::json!({"every": "1h"})
                .as_object()
                .cloned()
                .unwrap(),
            ..TriggerPatch::default()
        };
        let error = app
            .set_trigger("orphan", missing)
            .await
            .expect_err("no such miner");
        assert!(
            error.to_string().contains("miner `nobody` does not exist"),
            "{error}"
        );
        let watch = TriggerPatch {
            miner: Some("docs".to_string()),
            kind: Some(TriggerMechanism::Watch),
            enabled: Some(true),
            settings: serde_json::json!({"path": dir.path().join("absent")})
                .as_object()
                .cloned()
                .unwrap(),
            ..TriggerPatch::default()
        };
        let error = app.set_trigger("w", watch).await.expect_err("no such path");
        assert!(error.to_string().contains("does not exist"), "{error}");
        assert_eq!(
            app.list_triggers(MemoryMode::Full)
                .await
                .unwrap()
                .triggers
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn switching_a_trigger_back_on_forgets_when_it_was_due_while_it_was_off() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        let host = app.trigger_host();
        host.note(
            "t",
            Note::NextDue(Some(Utc::now() - chrono::Duration::days(3))),
        )
        .await;
        app.set_trigger_enabled("t", false).await.expect("off");

        app.set_trigger_enabled("t", true).await.expect("on");

        let view = app.show_trigger("t", MemoryMode::Full).await.expect("view");
        assert_eq!(view.next_due, None, "enabling never fires on the spot");
    }

    #[tokio::test]
    async fn removing_a_trigger_forgets_what_was_remembered_about_it() {
        let (app, _dir) = app_with_a_miner().await;
        app.set_trigger("t", poll_patch(true))
            .await
            .expect("enabled");
        app.fire_trigger("t", MemoryMode::Full)
            .await
            .expect("fired");
        app.remove_trigger("t").await.expect("removed");
        assert!(app.store.get_trigger_state("t").await.unwrap().is_none());
        assert!(matches!(
            app.show_trigger("t", MemoryMode::Full).await,
            Err(Error::TriggerNotFound { .. })
        ));
    }
}
