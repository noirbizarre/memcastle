//! A stand-in for the daemon, for the supervisor's own tests: it records what the triggers ask for and says what the
//! tests tell it to, so a timetable, a watcher and the webhook listener are tested without a palace.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::domain::{FireOutcome, JobId, TriggerDefinition, TriggerState};

use super::{Desired, FireRequest, Host, Note};

#[derive(Default)]
struct Inner {
    desired: Mutex<Desired>,
    fires: Mutex<Vec<FireRequest>>,
    notes: Mutex<Vec<(String, Note)>>,
    states: Mutex<HashMap<String, TriggerState>>,
    secrets: Mutex<HashMap<String, Vec<u8>>>,
    refusal: Mutex<Option<String>>,
    maintained: Mutex<usize>,
}

/// The fake daemon. Cheap to clone; every clone is the same daemon.
#[derive(Clone, Default)]
pub(crate) struct FakeHost {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl FakeHost {
    /// Say what should be running from now on.
    pub fn set_desired(&self, desired: Desired) {
        *lock(&self.inner.desired) = desired;
    }

    /// Give a webhook trigger's secret.
    pub fn set_secret(&self, trigger: &str, secret: &[u8]) {
        lock(&self.inner.secrets).insert(trigger.to_string(), secret.to_vec());
    }

    /// Remember a state, as if the daemon had stored it.
    pub fn set_state(&self, state: TriggerState) {
        lock(&self.inner.states).insert(state.name.clone(), state);
    }

    /// Make every request for a run fail with `reason`, or succeed again with `None`.
    pub fn refuse_with(&self, reason: Option<&str>) {
        *lock(&self.inner.refusal) = reason.map(str::to_string);
    }

    /// Every request for a run so far.
    pub fn fires(&self) -> Vec<FireRequest> {
        lock(&self.inner.fires).clone()
    }

    /// Every note so far, in order.
    pub fn notes(&self) -> Vec<(String, Note)> {
        lock(&self.inner.notes).clone()
    }

    /// How many times housekeeping ran.
    pub fn maintained(&self) -> usize {
        *lock(&self.inner.maintained)
    }

    /// Whether any note for `trigger` satisfies `check`.
    pub fn noted(&self, trigger: &str, check: impl Fn(&Note) -> bool) -> bool {
        self.notes()
            .iter()
            .any(|(name, note)| name == trigger && check(note))
    }
}

/// Poll `check` until it holds, for as long as a slow machine might need, and fail the test if it never does.
pub(crate) async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("gave up waiting for {what}");
}

impl Host for FakeHost {
    fn desired(&self) -> impl Future<Output = Desired> + Send {
        let desired = lock(&self.inner.desired).clone();
        async move { desired }
    }

    fn fire(
        &self,
        request: FireRequest,
    ) -> impl Future<Output = Result<FireOutcome, String>> + Send {
        lock(&self.inner.fires).push(request);
        let refusal = lock(&self.inner.refusal).clone();
        async move {
            match refusal {
                Some(reason) => Err(reason),
                None => Ok(FireOutcome::Queued { job: JobId::new() }),
            }
        }
    }

    fn state(&self, name: &str) -> impl Future<Output = Option<TriggerState>> + Send {
        let state = lock(&self.inner.states).get(name).cloned();
        async move { state }
    }

    fn note(&self, name: &str, note: Note) -> impl Future<Output = ()> + Send {
        if let Note::NextDue(due) = &note {
            let mut states = lock(&self.inner.states);
            states
                .entry(name.to_string())
                .or_insert_with(|| TriggerState::new(name))
                .next_due = *due;
        }
        lock(&self.inner.notes).push((name.to_string(), note));
        async {}
    }

    fn secret(&self, trigger: &TriggerDefinition) -> impl Future<Output = Option<Vec<u8>>> + Send {
        let secret = lock(&self.inner.secrets).get(&trigger.name).cloned();
        async move { secret }
    }

    fn maintain(&self) -> impl Future<Output = ()> + Send {
        *lock(&self.inner.maintained) += 1;
        async {}
    }
}
