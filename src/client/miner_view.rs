//! Human renderings of the miner routes' answers.

use serde_json::Value;

use super::table::{local_minute, render_table};
use crate::app::{MinerChange, MinerState, MinerView, MinersReload, MinersReport};
use crate::term::Painter;

/// A scope or settings value on one line: `groups=MemCastle,Ops since=2024`.
fn flat(map: &serde_json::Map<String, Value>) -> String {
    map.iter()
        .map(|(key, value)| {
            let text = match value {
                Value::String(s) => s.clone(),
                Value::Array(items) => items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map_or_else(|| item.to_string(), str::to_string)
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                other => other.to_string(),
            };
            format!("{key}={text}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn state_label(miner: &MinerView, painter: Painter) -> String {
    match miner.state {
        MinerState::Ready => painter.ok("ready"),
        MinerState::Disabled => painter.dim("disabled"),
        MinerState::Unavailable => painter.warn("unavailable"),
    }
}

/// The miners as a table, with what a miner that is not ready is waiting for underneath.
#[must_use]
pub fn render_miners(report: &MinersReport, painter: Painter, width: Option<u16>) -> String {
    let mut out = String::new();
    if report.miners.is_empty() {
        out.push_str(&painter.dim(
            "No miner is configured. `memcastle miner set <name> --source <source> --locator <where>` adds one.",
        ));
    } else {
        out.push_str(&render_table(
            &["NAME", "SOURCE", "STATE", "SCOPE", "LAST RUN", "DOCUMENTS"],
            report
                .miners
                .iter()
                .map(|miner| {
                    vec![
                        miner.name.clone(),
                        miner.source.clone(),
                        state_label(miner, painter),
                        flat(&miner.options),
                        miner
                            .last_run_at
                            .map_or_else(|| "never".to_string(), local_minute),
                        miner.documents.to_string(),
                    ]
                })
                .collect(),
            4,
            painter,
            width,
        ));
        for miner in &report.miners {
            if let Some(reason) = &miner.reason {
                out.push_str(&format!(
                    "\n{}",
                    painter.warn(&format!("{} is unavailable: {reason}", miner.name))
                ));
            }
        }
    }
    if let Some(error) = &report.error {
        out.push_str(&format!(
            "\n{}",
            painter.error(&format!(
                "the configuration file cannot be read, so these are the last miners it held: {error}"
            ))
        ));
    }
    out
}

/// One miner in full.
#[must_use]
pub fn render_miner(miner: &MinerView, painter: Painter) -> String {
    let mut lines = vec![
        format!("{} ({})", painter.heading(&miner.name), miner.source),
        format!("  state:      {}", state_label(miner, painter)),
    ];
    if let Some(reason) = &miner.reason {
        lines.push(format!(
            "  {}",
            painter.warn(&format!("unavailable: {reason}"))
        ));
    }
    if let Some(locator) = &miner.locator {
        lines.push(format!("  locator:    {locator}"));
    }
    if let Some(wing) = &miner.wing {
        lines.push(format!("  wing:       {wing}"));
    }
    if let Some(credential) = &miner.credential {
        lines.push(format!(
            "  credential: {} ({})",
            credential.kind,
            if credential.available {
                "available"
            } else {
                "missing"
            }
        ));
    }
    if !miner.options.is_empty() {
        lines.push(format!("  options:    {}", flat(&miner.options)));
    }
    lines.push(format!(
        "  mined:      {} documents, last run {}",
        miner.documents,
        miner
            .last_run_at
            .map_or_else(|| "never".to_string(), local_minute)
    ));
    lines.join("\n")
}

/// What `miner set`, `enable` and `disable` did, and the miner as it is now.
#[must_use]
pub fn render_change(change: &MinerChange, painter: Painter) -> String {
    let verb = if change.created {
        "Created"
    } else if change.changed {
        "Updated"
    } else {
        "Unchanged:"
    };
    let mut out = format!(
        "{verb} {}\n{}",
        change.miner.name,
        render_miner(&change.miner, painter)
    );
    if change.identity_changed {
        out.push_str(&format!(
            "\n{}",
            painter.warn(
                "it now reads a different source or locator, so it starts from that source's own cursor"
            )
        ));
    }
    out
}

/// What a reload found.
#[must_use]
pub fn render_reload(reload: &MinersReload, painter: Painter) -> String {
    let diff = &reload.diff;
    if diff.is_empty() {
        return format!("No change: {} miner(s) configured.", reload.miners);
    }
    let mut lines = vec![format!("Reloaded: {} miner(s) configured.", reload.miners)];
    for (label, names) in [
        ("added", &diff.added),
        ("removed", &diff.removed),
        ("changed", &diff.changed),
        ("enabled", &diff.enabled),
        ("disabled", &diff.disabled),
    ] {
        if !names.is_empty() {
            lines.push(format!("  {label}: {}", names.join(", ")));
        }
    }
    if !diff.broadened.is_empty() {
        lines.push(format!(
            "  {}",
            painter.warn(&format!(
                "options may be broadened by the file: {}",
                diff.broadened.join(", ")
            ))
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::{MinerChange, MinersDiff};

    fn view(value: serde_json::Value) -> MinerView {
        serde_json::from_value(value).expect("a miner view")
    }

    fn signal() -> MinerView {
        view(json!({
            "name": "signal-personal", "source": "signal", "enabled": true, "state": "unavailable",
            "reason": "the credential's environment variable `SIGNAL_TOKEN` is not set",
            "options": { "groups": ["MemCastle", "Ops"] },
            "credential": { "kind": "env", "available": false }, "documents": 3,
        }))
    }

    #[test]
    fn the_list_shows_each_miner_and_why_one_is_unavailable() {
        let report = MinersReport {
            miners: vec![signal()],
            config_file: None,
            error: None,
        };
        let text = render_miners(&report, Painter::PLAIN, None);
        for expected in [
            "signal-personal",
            "unavailable",
            "groups=MemCastle,Ops",
            "SIGNAL_TOKEN",
        ] {
            assert!(text.contains(expected), "`{expected}` missing:\n{text}");
        }
    }

    #[test]
    fn an_empty_list_says_how_to_add_a_miner_and_a_broken_file_is_not_hidden() {
        let report = MinersReport {
            miners: Vec::new(),
            config_file: None,
            error: Some("not valid TOML".to_string()),
        };
        let text = render_miners(&report, Painter::PLAIN, None);
        assert!(text.contains("memcastle miner set"), "{text}");
        assert!(
            text.contains("last miners it held") && text.contains("not valid TOML"),
            "{text}"
        );
    }

    #[test]
    fn one_miner_never_shows_the_credential_reference_only_its_availability() {
        let text = render_miner(&signal(), Painter::PLAIN);
        assert!(text.contains("credential: env (missing)"), "{text}");
        assert!(!text.contains("name = "), "{text}");
    }

    #[test]
    fn a_change_says_what_happened_and_warns_when_the_cursor_changed() {
        let change = MinerChange {
            miner: signal(),
            created: false,
            changed: true,
            identity_changed: true,
        };
        let text = render_change(&change, Painter::PLAIN);
        assert!(text.starts_with("Updated signal-personal"), "{text}");
        assert!(text.contains("different source or locator"), "{text}");
    }

    #[test]
    fn a_reload_lists_what_changed_and_flags_possibly_widened_options() {
        let reload = MinersReload {
            miners: 2,
            diff: MinersDiff {
                changed: vec!["chat".to_string()],
                broadened: vec!["chat".to_string()],
                ..MinersDiff::default()
            },
        };
        let text = render_reload(&reload, Painter::PLAIN);
        assert!(
            text.contains("changed: chat")
                && text.contains("options may be broadened by the file: chat"),
            "{text}"
        );
        let none = MinersReload {
            miners: 1,
            diff: MinersDiff::default(),
        };
        assert_eq!(
            render_reload(&none, Painter::PLAIN),
            "No change: 1 miner(s) configured."
        );
    }
}
