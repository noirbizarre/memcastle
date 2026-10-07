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
