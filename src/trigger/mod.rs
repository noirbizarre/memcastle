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

#[cfg(test)]
mod fake;
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

#[cfg(test)]
mod tests {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;

    use super::fake::{FakeHost, eventually};
    use super::*;

    fn definition(text: &str) -> (TriggerDefinition, TriggerPlan) {
        let definition: TriggerDefinition = toml::from_str(text).expect("a trigger");
        let plan = definition.plan().expect("a valid plan");
        (definition, plan)
    }

    fn poll(every: &str) -> (TriggerDefinition, TriggerPlan) {
        definition(&format!(
            "name = \"p\"\nminer = \"m\"\ntype = \"poll\"\nenabled = true\nevery = \"{every}\"\n"
        ))
    }

    fn webhook() -> (TriggerDefinition, TriggerPlan) {
        definition(
            "name = \"hook\"\nminer = \"m\"\ntype = \"webhook\"\nenabled = true\n\
             delivery_header = \"x-delivery-id\"\ncredential = { type = \"env\", name = \"HOOK\" }\n",
        )
    }

    fn listener(port: u16) -> Option<ListenerSettings> {
        Some(ListenerSettings {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            max_body_bytes: 1024,
            max_concurrent: 4,
        })
    }

    /// A supervisor running on `host`, and what stops it.
    fn supervise(host: &FakeHost) -> (Arc<Runtime>, CancellationToken, JoinHandle<()>) {
        let runtime = Arc::new(Runtime::default());
        let cancel = CancellationToken::new();
        let handle =
            tokio::spawn(Supervisor::new(host.clone(), Arc::clone(&runtime)).run(cancel.clone()));
        (runtime, cancel, handle)
    }

    async fn stop(cancel: CancellationToken, handle: JoinHandle<()>) {
        cancel.cancel();
        handle.await.expect("the supervisor stops cleanly");
    }

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret).unwrap();
        mac.update(body);
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("sha256={hex}")
    }

    async fn deliver(addr: SocketAddr, secret: &[u8], id: &str) -> reqwest::StatusCode {
        let body = b"{}";
        reqwest::Client::new()
            .post(format!("http://{addr}/hooks/hook"))
            .header("x-hub-signature-256", sign(secret, body))
            .header("x-delivery-id", id)
            .body(body.to_vec())
            .send()
            .await
            .expect("a delivery")
            .status()
    }

    #[tokio::test]
    async fn nothing_runs_until_something_is_desired_and_housekeeping_still_happens() {
        let host = FakeHost::default();
        let (runtime, cancel, handle) = supervise(&host);

        eventually("housekeeping", || host.maintained() >= 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(!runtime.is_running("p"));
        assert_eq!(runtime.webhook_addr(), None);
        assert!(host.fires().is_empty() && host.notes().is_empty());
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn a_trigger_runs_while_it_is_desired_and_what_it_waited_for_is_forgotten_when_it_stops()
    {
        let host = FakeHost::default();
        host.set_desired(Desired {
            triggers: vec![poll("1h")],
            listener: None,
        });
        let (runtime, cancel, handle) = supervise(&host);

        runtime.wake();
        eventually("the poll to start", || runtime.is_running("p")).await;
        eventually("its first wait to be noted", || {
            host.noted("p", |n| matches!(n, Note::NextDue(Some(_))))
        })
        .await;

        // Changed settings replace the task; it keeps running.
        host.set_desired(Desired {
            triggers: vec![poll("2h")],
            listener: None,
        });
        runtime.wake();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(runtime.is_running("p"));

        // Switched off: it stops, and enabling it again must not fire at once on an old due time.
        host.set_desired(Desired::default());
        runtime.wake();
        eventually("the poll to stop", || !runtime.is_running("p")).await;
        eventually("its wait to be forgotten", || {
            host.noted("p", |n| matches!(n, Note::NextDue(None)))
        })
        .await;
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn a_listener_that_cannot_bind_reports_why_on_every_webhook_and_recovers_when_the_port_frees()
     {
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let taken = blocker.local_addr().expect("addr").port();
        let host = FakeHost::default();
        host.set_desired(Desired {
            triggers: vec![webhook()],
            listener: listener(taken),
        });
        let (runtime, cancel, handle) = supervise(&host);

        runtime.wake();
        eventually("the bind failure to be reported", || {
            host.noted(
                "hook",
                |n| matches!(n, Note::Failed(why) if why.contains("cannot listen")),
            )
        })
        .await;
        assert_eq!(runtime.webhook_addr(), None);
        assert!(
            !runtime.is_running("hook"),
            "a webhook is running only while its listener is"
        );

        drop(blocker);
        runtime.wake();
        eventually("the listener to come up", || {
            runtime.webhook_addr().is_some()
        })
        .await;
        assert!(runtime.is_running("hook"));
        assert!(host.noted("hook", |n| matches!(n, Note::Healthy)));

        host.set_desired(Desired::default());
        runtime.wake();
        eventually("the listener to close", || runtime.webhook_addr().is_none()).await;
        stop(cancel, handle).await;
    }

    #[tokio::test]
    async fn an_authentic_delivery_asks_for_a_run_and_one_that_cannot_be_queued_is_told_to_retry() {
        let host = FakeHost::default();
        host.set_secret("hook", b"s3cret");
        host.set_desired(Desired {
            triggers: vec![webhook()],
            listener: listener(0),
        });
        let (runtime, cancel, handle) = supervise(&host);
        runtime.wake();
        eventually("the listener", || runtime.webhook_addr().is_some()).await;
        let addr = runtime.webhook_addr().expect("listening");

        assert_eq!(
            deliver(addr, b"s3cret", "d-1").await,
            reqwest::StatusCode::ACCEPTED
        );
        let fires = host.fires();
        assert_eq!(fires.len(), 1);
        assert_eq!(fires[0].trigger, "hook");
        assert_eq!(fires[0].via, Some(TriggerMechanism::Webhook));
        assert_eq!(fires[0].delivery.as_deref(), Some("d-1"));

        host.refuse_with(Some("the miner is disabled"));
        assert_eq!(
            deliver(addr, b"s3cret", "d-2").await,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "authentic, but nothing could be queued: the sender is told to come back"
        );
        assert_eq!(
            deliver(addr, b"wrong", "d-3").await,
            reqwest::StatusCode::UNAUTHORIZED
        );
        stop(cancel, handle).await;
    }
}
