//! Human renderings of the trigger routes' answers.

use serde_json::Value;

use super::table::{local_minute, render_table};
use crate::app::{TriggerChange, TriggerView, TriggersReload, TriggersReport};
use crate::domain::{FireOutcome, TriggerStatus};
use crate::term::Painter;

/// A settings map on one line: `every=1d at=03:30`.
fn flat(map: &serde_json::Map<String, Value>) -> String {
    map.iter()
        .map(|(key, value)| match value {
            Value::String(text) => format!("{key}={text}"),
            other => format!("{key}={other}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_label(trigger: &TriggerView, painter: Painter) -> String {
    match trigger.status {
        TriggerStatus::Active => painter.ok("active"),
        TriggerStatus::Disabled => painter.dim("disabled"),
        TriggerStatus::Failing => painter.error("failing"),
        TriggerStatus::Unavailable => painter.warn("unavailable"),
    }
}

/// The triggers as a table, with what a trigger that is not working is waiting for underneath.
#[must_use]
pub fn render_triggers(report: &TriggersReport, painter: Painter, width: Option<u16>) -> String {
    let mut out = String::new();
    if report.triggers.is_empty() {
        out.push_str(&painter.dim(
            "No trigger is configured, so nothing runs unattended. `memcastle trigger set <name> --miner <miner> \
             --type <schedule|poll|webhook|watch>` defines one (it starts disabled).",
        ));
    } else {
        out.push_str(&render_table(
            &[
                "NAME",
                "MINER",
                "TYPE",
                "STATUS",
                "SETTINGS",
                "LAST FIRED",
                "FIRED",
            ],
            report
                .triggers
                .iter()
                .map(|trigger| {
                    vec![
                        trigger.name.clone(),
                        trigger.miner.clone(),
                        trigger.kind.to_string(),
                        status_label(trigger, painter),
                        flat(&trigger.settings),
                        trigger
                            .last_fired_at
                            .map_or_else(|| "never".to_string(), local_minute),
                        trigger.fired.to_string(),
                    ]
                })
                .collect(),
            4,
            painter,
            width,
        ));
        for trigger in &report.triggers {
            if let Some(reason) = &trigger.reason {
                let line =
                    format!("{} is {:?}: {reason}", trigger.name, trigger.status).to_lowercase();
                out.push_str(&format!("\n{}", painter.warn(&line)));
            }
        }
    }
    out.push_str(&format!(
        "\nWebhook listener: {}",
        match (&report.webhook.listening, report.webhook.enabled) {
            (Some(addr), _) => format!("listening on {addr}"),
            (None, true) => "enabled, not listening (no webhook trigger is enabled)".to_string(),
            (None, false) => "off (`[webhook] enable = true` turns it on)".to_string(),
        }
    ));
    if let Some(error) = &report.error {
        out.push_str(&format!(
            "\n{}",
            painter.error(&format!(
                "the configuration file cannot be read, so these are the last triggers it held: {error}"
            ))
        ));
    }
    out
}

/// One trigger in full.
#[must_use]
pub fn render_trigger(trigger: &TriggerView, painter: Painter) -> String {
    let mut lines = vec![
        format!(
            "{} ({} -> miner {})",
            painter.heading(&trigger.name),
            trigger.kind,
            trigger.miner
        ),
        format!(
            "  status:     {}{}",
            status_label(trigger, painter),
            if trigger.running { " (running)" } else { "" }
        ),
    ];
    if let Some(reason) = &trigger.reason {
        lines.push(format!("  {}", painter.warn(reason)));
    }
    if !trigger.enabled {
        for step in &trigger.setup {
            lines.push(format!("  {}", painter.dim(&format!("to enable: {step}"))));
        }
    }
    if !trigger.settings.is_empty() {
        lines.push(format!("  settings:   {}", flat(&trigger.settings)));
    }
    if let Some(credential) = &trigger.credential {
        lines.push(format!(
            "  secret:     {} ({})",
            credential.kind,
            if credential.available {
                "available"
            } else {
                "missing"
            }
        ));
    }
    if let Some(endpoint) = &trigger.endpoint {
        lines.push(format!("  endpoint:   POST {endpoint}"));
    }
    if let Some(due) = trigger.next_due {
        lines.push(format!("  next due:   {}", local_minute(due)));
    }
    lines.push(format!(
        "  fired:      {} run(s) asked for, {} joined a waiting run, {} repeat(s) ignored, last {}",
        trigger.fired,
        trigger.coalesced,
        trigger.duplicates,
        trigger
            .last_fired_at
            .map_or_else(|| "never".to_string(), local_minute)
    ));
    if let Some(error) = &trigger.last_error {
        lines.push(format!(
            "  last error: {} ({} in a row{})",
            painter.error(error),
            trigger.consecutive_failures,
            trigger
                .last_error_at
                .map_or_else(String::new, |at| format!(", {}", local_minute(at)))
        ));
    }
    lines.join("\n")
}

/// What `trigger set`, `enable` and `disable` did, and the trigger as it is now.
#[must_use]
pub fn render_change(change: &TriggerChange, painter: Painter) -> String {
    let verb = if change.created {
        "Created"
    } else if change.changed {
        "Updated"
    } else {
        "Unchanged:"
    };
    format!(
        "{verb} {}\n{}",
        change.trigger.name,
        render_trigger(&change.trigger, painter)
    )
}

/// What a reload found.
#[must_use]
pub fn render_reload(reload: &TriggersReload) -> String {
    let diff = &reload.diff;
    if diff.is_empty() {
        return format!("No change: {} trigger(s) configured.", reload.triggers);
    }
    let mut lines = vec![format!(
        "Reloaded: {} trigger(s) configured.",
        reload.triggers
    )];
    for (label, names) in [
        ("added", &diff.added),
        ("removed", &diff.removed),
        ("changed", &diff.changed),
    ] {
        if !names.is_empty() {
            lines.push(format!("  {label}: {}", names.join(", ")));
        }
    }
    lines.join("\n")
}

/// What asking for a run came to.
#[must_use]
pub fn render_fire(outcome: &FireOutcome, painter: Painter) -> String {
    match outcome {
        FireOutcome::Queued { job } => format!("{} job {job}", painter.ok("Queued")),
        FireOutcome::Coalesced { job } => format!(
            "{} job {job}, which was already waiting and will read what changed",
            painter.ok("Joined")
        ),
        FireOutcome::Duplicate => "Nothing to do: that delivery was already accepted".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::{TriggersDiff, WebhookView};
    use crate::domain::JobId;

    fn view(value: serde_json::Value) -> TriggerView {
        serde_json::from_value(value).expect("a trigger view")
    }

    fn nightly() -> TriggerView {
        view(json!({
            "name": "nightly", "miner": "docs", "type": "schedule", "enabled": true,
            "status": "failing", "reason": "no route to the source", "running": true,
            "settings": { "every": "1d", "at": "03:30" },
            "credential": { "kind": "env", "available": false },
            "last_fired_at": "2030-01-01T03:30:00Z", "fired": 4, "coalesced": 1, "duplicates": 2,
            "next_due": "2030-01-02T03:30:00Z",
            "last_error": "no route to the source", "last_error_at": "2030-01-01T03:31:00Z",
            "consecutive_failures": 2,
        }))
    }

    fn quiet() -> TriggerView {
        view(json!({
            "name": "hook", "miner": "docs", "type": "webhook", "enabled": false,
            "status": "disabled", "setup": ["the webhook listener is off"], "running": false,
            "fired": 0, "coalesced": 0, "duplicates": 0, "consecutive_failures": 0,
        }))
    }

    fn webhook(listening: Option<&str>, enabled: bool) -> WebhookView {
        WebhookView {
            enabled,
            bind: "127.0.0.1".to_string(),
            port: 8787,
            allow_remote: false,
            listening: listening.map(str::to_string),
        }
    }

    fn report(
        triggers: Vec<TriggerView>,
        hook: WebhookView,
        error: Option<&str>,
    ) -> TriggersReport {
        TriggersReport {
            triggers,
            webhook: hook,
            config_file: None,
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn the_list_shows_each_trigger_with_its_status_and_why_one_is_not_working() {
        let text = render_triggers(
            &report(vec![nightly(), quiet()], webhook(None, false), None),
            Painter::PLAIN,
            None,
        );
        for expected in [
            "nightly",
            "schedule",
            "failing",
            "every=1d at=03:30",
            "hook",
            "disabled",
            "never",
            "is failing: no route to the source",
        ] {
            assert!(text.contains(expected), "`{expected}` missing:\n{text}");
        }
    }

    #[test]
    fn an_empty_list_says_nothing_runs_unattended_and_how_to_add_a_trigger() {
        let text = render_triggers(
            &report(Vec::new(), webhook(None, false), None),
            Painter::PLAIN,
            None,
        );
        assert!(text.contains("nothing runs unattended"), "{text}");
        assert!(text.contains("memcastle trigger set"), "{text}");
    }

    #[test]
    fn the_listener_is_described_as_off_allowed_or_listening() {
        let describe =
            |hook| render_triggers(&report(Vec::new(), hook, None), Painter::PLAIN, None);
        assert!(describe(webhook(None, false)).contains("off"));
        assert!(describe(webhook(None, true)).contains("no webhook trigger is enabled"));
        assert!(
            describe(webhook(Some("127.0.0.1:8787"), true)).contains("listening on 127.0.0.1:8787")
        );
    }

    #[test]
    fn an_unreadable_file_is_not_hidden() {
        let text = render_triggers(
            &report(vec![quiet()], webhook(None, false), Some("not valid TOML")),
            Painter::PLAIN,
            None,
        );
        assert!(
            text.contains("last triggers it held") && text.contains("not valid TOML"),
            "{text}"
        );
    }

    #[test]
    fn one_trigger_shows_its_failure_its_secret_availability_and_what_it_still_needs() {
        let text = render_trigger(&nightly(), Painter::PLAIN);
        for expected in [
            "nightly (schedule -> miner docs)",
            "(running)",
            "no route to the source",
            "settings:   every=1d at=03:30",
            "secret:     env (missing)",
            "next due:",
            "4 run(s) asked for, 1 joined a waiting run, 2 repeat(s) ignored",
            "last error:",
            "2 in a row",
        ] {
            assert!(text.contains(expected), "`{expected}` missing:\n{text}");
        }
        let disabled = render_trigger(&quiet(), Painter::PLAIN);
        assert!(
            disabled.contains("to enable: the webhook listener is off"),
            "{disabled}"
        );
        assert!(
            !disabled.contains("(running)") && !disabled.contains("last error"),
            "{disabled}"
        );
    }

    #[test]
    fn a_listening_webhook_shows_where_a_sender_posts_and_an_available_secret_says_so() {
        let hook = view(json!({
            "name": "hook", "miner": "docs", "type": "webhook", "enabled": true,
            "status": "active", "running": true,
            "endpoint": "http://127.0.0.1:8787/hooks/hook",
            "credential": { "kind": "file", "available": true },
            "fired": 0, "coalesced": 0, "duplicates": 0, "consecutive_failures": 0,
        }));
        let text = render_trigger(&hook, Painter::PLAIN);
        assert!(
            text.contains("POST http://127.0.0.1:8787/hooks/hook"),
            "{text}"
        );
        assert!(text.contains("secret:     file (available)"), "{text}");
    }

    #[test]
    fn a_change_says_what_happened() {
        let verb = |created, changed| {
            render_change(
                &TriggerChange {
                    trigger: quiet(),
                    created,
                    changed,
                },
                Painter::PLAIN,
            )
        };
        assert!(verb(true, true).starts_with("Created hook"));
        assert!(verb(false, true).starts_with("Updated hook"));
        assert!(verb(false, false).starts_with("Unchanged: hook"));
    }

    #[test]
    fn a_reload_says_what_it_found() {
        let none = TriggersReload {
            triggers: 2,
            diff: TriggersDiff::default(),
        };
        assert_eq!(render_reload(&none), "No change: 2 trigger(s) configured.");
        let some = TriggersReload {
            triggers: 3,
            diff: TriggersDiff {
                added: vec!["a".to_string()],
                removed: vec!["b".to_string()],
                changed: vec!["c".to_string()],
            },
        };
        let text = render_reload(&some);
        for expected in ["Reloaded: 3", "added: a", "removed: b", "changed: c"] {
            assert!(text.contains(expected), "`{expected}` missing:\n{text}");
        }
    }

    #[test]
    fn firing_says_whether_a_run_was_queued_joined_or_already_accepted() {
        let job = JobId::new();
        assert!(
            render_fire(&FireOutcome::Queued { job }, Painter::PLAIN).starts_with("Queued job")
        );
        assert!(
            render_fire(&FireOutcome::Coalesced { job }, Painter::PLAIN)
                .contains("already waiting")
        );
        assert!(render_fire(&FireOutcome::Duplicate, Painter::PLAIN).contains("already accepted"));
    }
}
