//! The `schedule` and `poll` triggers: ask for a run every so often.
//!
//! The two differ in what they do when a request fails. A schedule keeps its timetable (a daily trigger is due at its
//! hour whatever happened yesterday); a poll backs off, doubling its wait up to `max_backoff`, so a source that keeps
//! failing is not hammered, and returns to its interval on the first success.
//!
//! When a timetable is next due is remembered through the host, so a restart neither forgets the wait nor fires early,
//! and a slot missed while the daemon was down is fired once, not once per missed slot. Enabling a trigger never fires
//! it on the spot: the first request is one interval (or the next time of day) away.

use std::time::Duration;

use chrono::{NaiveTime, Utc};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::domain::{TriggerMechanism, next_due};

use super::{FireRequest, Host, Note};

/// When a timer fires.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timetable {
    pub every: Duration,
    pub at: Option<NaiveTime>,
    /// Set for a poll: the longest wait while requests fail.
    pub max_backoff: Option<Duration>,
}

/// How long to wait after `failures` failures in a row: the interval doubled for each, never more than `max`.
pub(super) fn backoff(every: Duration, failures: u32, max: Duration) -> Duration {
    // Capped at 2^16 intervals before the multiplication so it cannot overflow, whatever `max` is.
    every
        .saturating_mul(1u32 << failures.min(16))
        .min(max)
        .max(every)
}

pub(super) async fn run<H: Host>(
    host: H,
    name: String,
    via: TriggerMechanism,
    timetable: Timetable,
    cancel: CancellationToken,
) {
    let Timetable {
        every,
        at,
        max_backoff,
    } = timetable;
    let state = host.state(&name).await;
    let mut due = match state.as_ref().and_then(|s| s.next_due) {
        Some(due) => due,
        None => {
            let due = next_due(
                every,
                at,
                state.as_ref().and_then(|s| s.last_fired_at),
                Utc::now(),
            );
            host.note(&name, Note::NextDue(Some(due))).await;
            due
        }
    };
    let mut failures: u32 = 0;
    loop {
        // An overdue slot has a zero wait, which fires once and then moves on: a missed day is not owed twice.
        let wait = (due - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        let fired_at = Utc::now();
        let result = host
            .fire(FireRequest {
                trigger: name.clone(),
                via: Some(via),
                delivery: None,
            })
            .await;
        failures = if result.is_ok() {
            0
        } else {
            failures.saturating_add(1)
        };
        if let Err(reason) = &result {
            debug!(trigger = %name, %reason, "a timed request for a run failed");
        }
        due = match (result.is_ok(), max_backoff) {
            (false, Some(max)) => {
                fired_at
                    + chrono::Duration::from_std(backoff(every, failures, max))
                        .unwrap_or(chrono::Duration::days(1))
            }
            _ => next_due(every, at, Some(fired_at), Utc::now()),
        };
        host.note(&name, Note::NextDue(Some(due))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poll_doubles_its_wait_per_failure_and_never_waits_longer_than_its_ceiling() {
        let every = Duration::from_secs(60);
        let max = Duration::from_secs(60 * 8);
        let waits: Vec<u64> = (1..=6)
            .map(|failures| backoff(every, failures, max).as_secs())
            .collect();
        assert_eq!(waits, [120, 240, 480, 480, 480, 480]);
    }

    #[test]
    fn a_ceiling_below_the_interval_never_shortens_it() {
        let every = Duration::from_secs(60);
        assert_eq!(backoff(every, 3, Duration::from_secs(10)), every);
    }

    #[test]
    fn a_huge_failure_count_does_not_overflow() {
        let every = Duration::from_secs(3600);
        assert_eq!(
            backoff(every, u32::MAX, Duration::from_secs(86_400)),
            Duration::from_secs(86_400)
        );
    }

    use chrono::Duration as Span;

    use super::super::fake::{FakeHost, eventually};
    use crate::domain::TriggerState;

    fn timetable(every: Duration, max_backoff: Option<Duration>) -> Timetable {
        Timetable {
            every,
            at: None,
            max_backoff,
        }
    }

    #[tokio::test]
    async fn a_timer_that_never_fired_waits_a_whole_interval_and_says_when_it_is_due() {
        let host = FakeHost::default();
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            host.clone(),
            "t".to_string(),
            TriggerMechanism::Schedule,
            timetable(Duration::from_secs(3600), None),
            cancel.clone(),
        ));

        eventually("its wait to be noted", || {
            host.noted("t", |n| matches!(n, Note::NextDue(Some(_))))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(150)).await;

        assert!(host.fires().is_empty(), "enabling never fires on the spot");
        cancel.cancel();
        task.await.expect("stops when cancelled");
    }

    #[tokio::test]
    async fn a_slot_missed_while_the_daemon_was_down_fires_once_and_moves_on() {
        let host = FakeHost::default();
        let mut state = TriggerState::new("t");
        state.next_due = Some(Utc::now() - Span::days(3));
        host.set_state(state);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(run(
            host.clone(),
            "t".to_string(),
            TriggerMechanism::Schedule,
            timetable(Duration::from_secs(3600), None),
            cancel.clone(),
        ));

        eventually("the overdue slot to fire", || host.fires().len() == 1).await;
        eventually("the next slot to be noted", || {
            host.noted(
                "t",
                |n| matches!(n, Note::NextDue(Some(due)) if *due > Utc::now()),
            )
        })
        .await;
        tokio::time::sleep(Duration::from_millis(150)).await;

        assert_eq!(
            host.fires().len(),
            1,
            "three missed days are one request, not seventy-two"
        );
        assert_eq!(host.fires()[0].via, Some(TriggerMechanism::Schedule));
        cancel.cancel();
        task.await.expect("stops when cancelled");
    }

    #[tokio::test]
    async fn a_poll_that_fails_waits_longer_and_a_schedule_that_fails_keeps_its_timetable() {
        for (via, ceiling) in [
            (TriggerMechanism::Poll, Some(Duration::from_millis(400))),
            (TriggerMechanism::Schedule, None),
        ] {
            let host = FakeHost::default();
            host.refuse_with(Some("the source is down"));
            let mut state = TriggerState::new("t");
            state.next_due = Some(Utc::now());
            host.set_state(state);
            let cancel = CancellationToken::new();
            let task = tokio::spawn(run(
                host.clone(),
                "t".to_string(),
                via,
                timetable(Duration::from_millis(100), ceiling),
                cancel.clone(),
            ));

            eventually("two failed requests", || host.fires().len() >= 2).await;

            let dues: Vec<_> = host
                .notes()
                .into_iter()
                .filter_map(|(_, note)| match note {
                    Note::NextDue(Some(due)) => Some(due),
                    _ => None,
                })
                .collect();
            assert!(
                dues.len() >= 2,
                "{via}: every request is followed by the next time"
            );
            cancel.cancel();
            task.await.expect("stops when cancelled");
        }
    }
}
