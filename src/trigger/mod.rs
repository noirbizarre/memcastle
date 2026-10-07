//! Source triggers: deciding *when* to ask for a mining run (docs/adr/043).
//!
//! A trigger is a supervised task per enabled `[[triggers]]` entry: a timetable, a poll, a file-system watcher, or a
//! delivery to the webhook listener. Every one of them ends in the same call, [`Host::fire`], which is the call
//! `memcastle miner run` makes, so a trigger can cause nothing a person could not ask for by hand, and the pipeline
//! alone decides what is new.
//!
//! This module owns the *lifecycle* (start, stop, reconnect, back off, report) and knows nothing else: it reaches no
//! store, no jobs and no source. Everything it needs from the daemon (what is enabled now, how to ask for a run, where
//! to remember progress, how to read a secret) comes through the [`Host`] trait, which `app` implements. Only `app`
//! and `server` name this module (`tests/trigger_isolation.rs`).
//!
//! Nothing here starts on its own. The supervisor starts what [`Host::desired`] returns, and the host returns only
//! triggers the user enabled and whose prerequisites hold, so a defined trigger, an installed source or an enabled
//! miner starts no task, opens no port and watches no file.

mod timer;
mod watch;
mod webhook;

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::domain::{FireOutcome, TriggerDefinition, TriggerMechanism, TriggerPlan, TriggerState};

pub use webhook::verify as verify_delivery;

/// How often the supervisor looks again at what should be running, with no one asking: it is how a hand edit of the
/// configuration file, a miner that became runnable and a listener that could not bind are noticed.
const TICK: Duration = Duration::from_secs(5);
/// How often old deliveries are pruned.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(3600);
/// How long a stopping task is given to finish before it is aborted.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// One request for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireRequest {
    /// The trigger asking.
    pub trigger: String,
    /// How it decided to; `None` is a person asking (`memcastle trigger fire`).
    pub via: Option<TriggerMechanism>,
    /// The sender's id for a webhook delivery, so a repeat of it is answered but not run again.
    pub delivery: Option<String>,
}

/// What the supervisor tells the host about a trigger, so the host can keep it where the user can see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// The next time a timetable fires, or `None` when it no longer applies (the trigger was stopped on purpose).
    NextDue(Option<DateTime<Utc>>),
    /// Something went wrong: what, never a payload or a secret.
    Failed(String),
    /// It is working again.
    Healthy,
}

/// Where the webhook listener should listen, and how much it takes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerSettings {
    /// The address, already vetted by the host (loopback, or remote with the opt-in and authentication).
    pub addr: SocketAddr,
    /// The largest delivery body, in bytes.
    pub max_body_bytes: usize,
    /// How many deliveries are worked on at once.
    pub max_concurrent: usize,
}

/// What should be running right now.
#[derive(Debug, Clone, Default)]
pub struct Desired {
    /// Every trigger that is enabled and whose prerequisites hold, with its settings parsed.
    pub triggers: Vec<(TriggerDefinition, TriggerPlan)>,
    /// Where the webhook listener may listen; `None` when webhooks are not enabled or the address is not safe.
    pub listener: Option<ListenerSettings>,
}

/// What the supervisor needs from the daemon.
pub trait Host: Clone + Send + Sync + 'static {
    /// What should be running now, read from the configuration as it is at this moment.
    fn desired(&self) -> impl Future<Output = Desired> + Send;
    /// Ask for a run. Records the outcome (or the failure) on the trigger.
    fn fire(
        &self,
        request: FireRequest,
    ) -> impl Future<Output = std::result::Result<FireOutcome, String>> + Send;
    /// What the daemon remembers about a trigger.
    fn state(&self, name: &str) -> impl Future<Output = Option<TriggerState>> + Send;
    /// Remember something about a trigger.
    fn note(&self, name: &str, note: Note) -> impl Future<Output = ()> + Send;
    /// The shared secret of a webhook trigger, read now so a rotated secret takes effect without a restart.
    fn secret(&self, trigger: &TriggerDefinition) -> impl Future<Output = Option<Vec<u8>>> + Send;
    /// Housekeeping that does not belong to one trigger (forgetting old deliveries).
    fn maintain(&self) -> impl Future<Output = ()> + Send;
}

/// What the supervisor shares with the rest of the daemon: a way to wake it, and what it is doing.
#[derive(Default)]
pub struct Runtime {
    wake: Notify,
    inner: Mutex<RuntimeState>,
}

#[derive(Default)]
struct RuntimeState {
    webhook_addr: Option<SocketAddr>,
    running: BTreeSet<String>,
}

impl Runtime {
    /// Ask the supervisor to look at the configuration now, rather than at its next tick.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Where the webhook listener is listening, or `None` while it is not.
    #[must_use]
    pub fn webhook_addr(&self) -> Option<SocketAddr> {
        self.lock().webhook_addr
    }

    /// Whether the trigger `name` is running: its task is up, or the listener it is delivered to is.
    #[must_use]
    pub fn is_running(&self, name: &str) -> bool {
        self.lock().running.contains(name)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        // The state is two plain values, so a panic elsewhere cannot leave it half written.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn publish(&self, addr: Option<SocketAddr>, running: BTreeSet<String>) {
        let mut state = self.lock();
        state.webhook_addr = addr;
        state.running = running;
    }
}

/// A running task and the definition it was started from, so a changed definition restarts it.
struct Task {
    definition: TriggerDefinition,
    plan: TriggerPlan,
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

/// The webhook listener that is up.
struct Listener {
    settings: ListenerSettings,
    addr: SocketAddr,
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

/// Runs every trigger that should be running, and nothing else.
pub struct Supervisor<H: Host> {
    host: H,
    runtime: Arc<Runtime>,
}

impl<H: Host> Supervisor<H> {
    /// A supervisor over `host`, reporting through `runtime`.
    #[must_use]
    pub fn new(host: H, runtime: Arc<Runtime>) -> Self {
        Self { host, runtime }
    }

    /// Supervise until `cancel`, then stop every trigger and the listener and return.
    pub async fn run(self, cancel: CancellationToken) {
        let mut tasks: HashMap<String, Task> = HashMap::new();
        let mut listener: Option<Listener> = None;
        // The last bind failure that was reported, so a port that stays taken is one log line, not one every tick.
        let mut bind_error: Option<String> = None;
        let routes: webhook::Routes = Arc::new(RwLock::new(HashMap::new()));
        let mut last_maintenance: Option<tokio::time::Instant> = None;

        loop {
            if last_maintenance.is_none_or(|at| at.elapsed() >= MAINTENANCE_EVERY) {
                self.host.maintain().await;
                last_maintenance = Some(tokio::time::Instant::now());
            }
            let desired = self.host.desired().await;
            self.reconcile(
                &desired,
                &mut tasks,
                &mut listener,
                &mut bind_error,
                &routes,
            )
            .await;

            tokio::select! {
                () = cancel.cancelled() => break,
                () = self.runtime.wake.notified() => {}
                () = tokio::time::sleep(TICK) => {}
            }
        }

        // Shutdown, not a decision by the user: what each timetable was waiting for is kept for the next start.
        for (_, task) in tasks.drain() {
            stop_task(task).await;
        }
        if let Some(listener) = listener.take() {
            stop_listener(listener).await;
        }
        self.runtime.publish(None, BTreeSet::new());
    }

    async fn reconcile(
        &self,
        desired: &Desired,
        tasks: &mut HashMap<String, Task>,
        listener: &mut Option<Listener>,
        bind_error: &mut Option<String>,
        routes: &webhook::Routes,
    ) {
        // Timers and watchers: one task each.
        let wanted: HashMap<&str, &(TriggerDefinition, TriggerPlan)> = desired
            .triggers
            .iter()
            .filter(|(_, plan)| !matches!(plan, TriggerPlan::Webhook(_)))
            .map(|entry| (entry.0.name.as_str(), entry))
            .collect();

        let stale: Vec<String> = tasks
            .iter()
            .filter(|(name, task)| {
                task.handle.is_finished()
                    || wanted.get(name.as_str()).is_none_or(|(definition, plan)| {
                        *definition != task.definition || *plan != task.plan
                    })
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            let Some(task) = tasks.remove(&name) else {
                continue;
            };
            let crashed = task.handle.is_finished();
            stop_task(task).await;
            if !wanted.contains_key(name.as_str()) {
                // Switched off, removed or no longer runnable: the user's decision, so what it was waiting for no longer
                // applies and enabling it again waits a whole interval instead of firing at once.
                self.host.note(&name, Note::NextDue(None)).await;
            } else if crashed {
                warn!(trigger = %name, "a trigger task ended unexpectedly; starting it again");
            }
        }
        for (definition, plan) in wanted.values().map(|entry| (&entry.0, &entry.1)) {
            if tasks.contains_key(&definition.name) {
                continue;
            }
            info!(trigger = %definition.name, kind = %definition.kind, "trigger started");
            let cancel = CancellationToken::new();
            let handle = match plan {
                TriggerPlan::Schedule { every, at } => tokio::spawn(timer::run(
                    self.host.clone(),
                    definition.name.clone(),
                    TriggerMechanism::Schedule,
                    timer::Timetable {
                        every: *every,
                        at: *at,
                        max_backoff: None,
                    },
                    cancel.clone(),
                )),
                TriggerPlan::Poll { every, max_backoff } => tokio::spawn(timer::run(
                    self.host.clone(),
                    definition.name.clone(),
                    TriggerMechanism::Poll,
                    timer::Timetable {
                        every: *every,
                        at: None,
                        max_backoff: Some(*max_backoff),
                    },
                    cancel.clone(),
                )),
                TriggerPlan::Watch {
                    path,
                    debounce,
                    recursive,
                } => tokio::spawn(watch::run(
                    self.host.clone(),
                    definition.name.clone(),
                    path.clone(),
                    *debounce,
                    *recursive,
                    cancel.clone(),
                )),
                TriggerPlan::Webhook(_) => continue,
            };
            tasks.insert(
                definition.name.clone(),
                Task {
                    definition: definition.clone(),
                    plan: plan.clone(),
                    cancel,
                    handle,
                },
            );
        }

        // Webhooks: one listener for all of them, bound only while at least one is enabled.
        let hooks: HashMap<String, Arc<webhook::Hook>> = desired
            .triggers
            .iter()
            .filter_map(|(definition, plan)| match plan {
                TriggerPlan::Webhook(plan) => Some((
                    definition.name.clone(),
                    Arc::new(webhook::Hook {
                        definition: definition.clone(),
                        plan: plan.clone(),
                    }),
                )),
                _ => None,
            })
            .collect();
        let hook_names: Vec<String> = hooks.keys().cloned().collect();
        // Replaced before the listener is (re)checked, so a delivery never meets a route table that is being rebuilt.
        *routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hooks;

        match (hook_names.is_empty(), desired.listener) {
            (false, Some(settings)) => {
                let up = listener
                    .as_ref()
                    .is_some_and(|l| l.settings == settings && !l.handle.is_finished());
                if !up {
                    if let Some(old) = listener.take() {
                        stop_listener(old).await;
                    }
                    match webhook::bind(&self.host, settings, routes).await {
                        Ok(started) => {
                            info!(addr = %started.addr, "webhook listener started");
                            *bind_error = None;
                            for name in &hook_names {
                                self.host.note(name, Note::Healthy).await;
                            }
                            *listener = Some(started);
                        }
                        Err(reason) => {
                            if bind_error.as_deref() != Some(reason.as_str()) {
                                warn!(%reason, "the webhook listener could not start");
                                *bind_error = Some(reason.clone());
                            }
                            for name in &hook_names {
                                self.host.note(name, Note::Failed(reason.clone())).await;
                            }
                        }
                    }
                }
            }
            _ => {
                *bind_error = None;
                if let Some(old) = listener.take() {
                    info!("webhook listener stopped: no webhook trigger is enabled");
                    stop_listener(old).await;
                }
            }
        }

        let mut running: BTreeSet<String> = tasks.keys().cloned().collect();
        if listener.is_some() {
            running.extend(hook_names);
        }
        self.runtime
            .publish(listener.as_ref().map(|l| l.addr), running);
    }
}

async fn stop_task(task: Task) {
    task.cancel.cancel();
    let mut handle = task.handle;
    if tokio::time::timeout(STOP_TIMEOUT, &mut handle)
        .await
        .is_err()
    {
        handle.abort();
    }
}

async fn stop_listener(listener: Listener) {
    listener.cancel.cancel();
    let mut handle = listener.handle;
    if tokio::time::timeout(STOP_TIMEOUT, &mut handle)
        .await
        .is_err()
    {
        handle.abort();
    }
}
