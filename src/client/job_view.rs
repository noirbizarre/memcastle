//! Human renderings of one job, and of what a job-control request did.
//!
//! Like [`super::table`] these are only used when the output is pretty (a terminal without `--json`);
//! anything else gets the job as JSON.

use crate::app::{JobControlResult, JobControlStatus};
use crate::domain::{Job, JobId, JobKind, MiningSource};
use crate::term::Painter;

use super::palace_view::field;
use super::table::{kind_label, one_line, progress_label};

/// A timestamp in the reader's timezone, to the second: a job's start and end are close together.
fn local_second(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// What the job was asked to do beyond its kind: the parameters a person wants to confirm they typed correctly.
fn parameters(kind: &JobKind, painter: Painter) -> Vec<String> {
    let mut lines = Vec::new();
    let wing = |lines: &mut Vec<String>, wing: &Option<String>| {
        if let Some(wing) = wing {
            lines.push(field(painter, "Wing", wing));
        }
    };
    match kind {
        JobKind::Mine {
            source,
            wing: w,
            full,
            options,
        } => {
            match source {
                MiningSource::Directory { path } => {
                    lines.push(field(painter, "Source", path.display()));
                }
                MiningSource::Named { source, locator } => {
                    lines.push(field(painter, "Source", source));
                    if let Some(locator) = locator {
                        lines.push(field(painter, "Locator", locator));
                    }
                }
            }
            // One line per option, so `since=2026-09` is there to be confirmed, as the locator and wing are.
            for (key, value) in options {
                lines.push(field(painter, &format!("Option {key}"), value));
            }
            wing(&mut lines, w);
            if *full {
                // A full re-read costs more than an incremental one, so say it was asked for.
                lines.push(field(painter, "Mode", "full re-read"));
            }
        }
        JobKind::Audit { wing: w } | JobKind::Embed { wing: w } | JobKind::Extract { wing: w } => {
            wing(&mut lines, w);
        }
        JobKind::Repair { based_on_job, .. } => {
            if let Some(job) = based_on_job {
                lines.push(field(painter, "Based on", job));
            }
        }
        JobKind::Demo { steps } => lines.push(field(painter, "Steps", steps)),
        JobKind::Checkpoint { payload } => {
            // Only the size: the items themselves are in the JSON form and can be long.
            lines.push(field(painter, "Items", payload.items.len()));
        }
    }
    lines
}

/// One job as a block of `Label: value` lines.
///
/// Optional fields appear only when set, so a freshly queued job is short and a failed one shows its error.
#[must_use]
pub fn render_job(job: &Job, painter: Painter) -> String {
    let mut lines = vec![
        field(painter, "Job", painter.accent(&job.id.to_string())),
        field(painter, "Kind", kind_label(&job.kind)),
        field(painter, "Status", painter.job_status(job.status)),
    ];
    lines.extend(parameters(&job.kind, painter));
    let mut progress = progress_label(job);
    if let Some(message) = &job.progress.message {
        progress = format!("{progress} {}", one_line(message));
    }
    lines.push(field(painter, "Progress", progress));
    lines.push(field(painter, "Created", local_second(job.created_at)));
    if let Some(at) = job.started_at {
        lines.push(field(painter, "Started", local_second(at)));
    }
    if let Some(at) = job.completed_at {
        lines.push(field(painter, "Completed", local_second(at)));
    }
    if let Some(error) = &job.error {
        lines.push(field(painter, "Error", painter.error(&one_line(error))));
    }
    if let Some(result) = &job.result {
        // Indented under its label so the report reads as part of the card and not as more fields.
        let text = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
        let indented: Vec<String> = text.lines().map(|line| format!("  {line}")).collect();
        // The label alone, not `field` with an empty value: that would leave a trailing space on the line.
        lines.push(painter.dim("Result:"));
        lines.extend(indented);
    }
    lines.join("\n")
}

/// A job that was just submitted: its card, then how to follow it.
///
/// Every job is queued in the background, so the question after submitting one is always "what happened to it".
#[must_use]
pub fn render_submitted(job: &Job, painter: Painter) -> String {
    format!(
        "{} {} job {}\n{}\n\n{} {}",
        painter.ok("Queued"),
        kind_label(&job.kind),
        job.id,
        render_job(job, painter),
        painter.dim("Follow it with:"),
        painter.accent(&format!("memcastle job show {}", job.id)),
    )
}

/// One line saying what a pause, resume, cancel or retry request did.
///
/// Pause and cancel are requests, not outcomes (stopping is cooperative), and the words say so.
#[must_use]
pub fn render_control(id: JobId, result: &JobControlResult, painter: Painter) -> String {
    let (verb, rest) = match result.status {
        JobControlStatus::PauseRequested => ("Pause requested", "it stops at its next check"),
        JobControlStatus::Resumed => ("Resumed", "it is queued again"),
        JobControlStatus::CancelRequested => ("Cancel requested", "it stops at its next check"),
        JobControlStatus::ForceCancelled => ("Force-cancelled", "the local worker stopped"),
        JobControlStatus::Retried => ("Retried", "it is queued again"),
    };
    format!("{} job {id}, {}", painter.ok(verb), painter.dim(rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{JobEvent, Priority};

    fn mine_job() -> Job {
        Job::new(
            JobKind::Mine {
                source: MiningSource::Directory {
                    path: "/work/project".into(),
                },
                wing: Some("project".to_string()),
                full: false,
                options: Default::default(),
            },
            Priority::Background,
            "cli",
        )
    }

    #[test]
    fn a_queued_mining_job_shows_where_it_reads_and_where_it_files() {
        let text = render_job(&mine_job(), Painter::PLAIN);
        for expected in [
            "Kind: mine",
            "Status: queued",
            "Source: /work/project",
            "Wing: project",
            "Progress: -",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn a_job_with_nothing_to_report_has_no_error_or_result_lines() {
        let text = render_job(&mine_job(), Painter::PLAIN);
        assert!(
            !text.contains("Error:") && !text.contains("Result:"),
            "{text}"
        );
        assert!(
            !text.contains("Started:") && !text.contains("Completed:"),
            "{text}"
        );
    }

    #[test]
    fn a_failed_job_shows_its_error_on_one_line() {
        let mut job = mine_job();
        job.apply(JobEvent::Claim).unwrap();
        job.apply(JobEvent::Fail).unwrap();
        job.error = Some("boom\nsecond line".to_string());
        let text = render_job(&job, Painter::PLAIN);
        assert!(text.contains("Status: failed"), "{text}");
        assert!(text.contains("Error: boom second line"), "{text}");
        assert!(text.contains("Started:"), "{text}");
    }

    #[test]
    fn a_result_is_indented_under_its_label() {
        let mut job = mine_job();
        job.result = Some(serde_json::json!({"filed": 3}));
        let text = render_job(&job, Painter::PLAIN);
        assert!(
            text.contains("Result:\n  {\n    \"filed\": 3\n  }"),
            "{text}"
        );
    }

    #[test]
    fn a_submitted_job_says_how_to_follow_it() {
        let job = mine_job();
        let text = render_submitted(&job, Painter::PLAIN);
        assert!(
            text.starts_with(&format!("Queued mine job {}", job.id)),
            "{text}"
        );
        assert!(
            text.ends_with(&format!("Follow it with: memcastle job show {}", job.id)),
            "{text}"
        );
    }

    #[test]
    fn a_full_re_read_is_called_out() {
        let job = Job::new(
            JobKind::Mine {
                source: MiningSource::Named {
                    source: "pi".to_string(),
                    locator: None,
                },
                wing: None,
                full: true,
                options: Default::default(),
            },
            Priority::Background,
            "cli",
        );
        let text = render_job(&job, Painter::PLAIN);
        assert!(text.contains("Mode: full re-read"), "{text}");
        assert!(text.contains("Source: pi"), "{text}");
    }

    #[test]
    fn a_coloured_card_is_the_plain_card_with_escapes_stripped() {
        let job = mine_job();
        let coloured = render_submitted(&job, Painter::forced());
        assert!(coloured.contains('\u{1b}'), "colour was forced");
        assert_eq!(
            console::strip_ansi_codes(&coloured),
            render_submitted(&job, Painter::PLAIN)
        );
    }

    #[test]
    fn control_requests_are_worded_as_requests_not_outcomes() {
        let id = JobId::new();
        let line = |status| render_control(id, &JobControlResult { status }, Painter::PLAIN);
        assert!(line(JobControlStatus::PauseRequested).starts_with("Pause requested"));
        assert!(line(JobControlStatus::CancelRequested).starts_with("Cancel requested"));
        assert!(line(JobControlStatus::Resumed).starts_with("Resumed"));
        assert!(line(JobControlStatus::Retried).starts_with("Retried"));
        assert!(line(JobControlStatus::Resumed).contains(&id.to_string()));
    }
}
