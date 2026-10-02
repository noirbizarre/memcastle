//! Human-readable listings for the CLI.
//!
//! Only used when stdout is a terminal. A pipe gets the same data as JSON
//! (see `main.rs`), so `memcastle jobs list | jq` keeps working and the table
//! never needs to be parseable.

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{ColumnConstraint, ContentArrangement, Table, Width};

use crate::domain::{Job, JobKind};
use crate::term::Painter;

/// The longest the "detail" column may get before it is cut with an ellipsis.
/// A failed job's error can run to a paragraph, and one such row would
/// otherwise wrap into a dozen lines and push the rest of the list away; the
/// full text is one `memcastle jobs show` away.
const DETAIL_MAX_CHARS: usize = 60;

/// Index of the free-text column, the only one allowed to shrink.
const DETAIL_COLUMN: usize = 5;

/// The least width, in characters, the detail column is squeezed to.
const DETAIL_MIN_WIDTH: u16 = 12;

/// Render `jobs` as a table: one row per job, oldest first as given.
///
/// `width` is the terminal's width in columns when known; the table shrinks
/// its wrappable columns to fit it instead of overflowing and breaking every
/// row across two lines. `None` leaves the columns at their natural width.
#[must_use]
pub fn render_jobs(jobs: &[Job], painter: Painter, width: Option<u16>) -> String {
    if jobs.is_empty() {
        return painter.dim("No jobs.");
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        // The width is ours to decide (below), not comfy-table's: it would
        // ask crossterm, which reports 0 columns for a terminal that was never
        // sized and squeezes every column to a single character.
        .force_no_tty()
        .set_header(
            ["ID", "KIND", "STATUS", "PROGRESS", "CREATED", "DETAIL"]
                .map(|title| painter.heading(title)),
        );
    if let Some(width) = width {
        table.set_width(width);
    }

    for job in jobs {
        table.add_row([
            // The full id, never a prefix: `jobs show`/`cancel` take it
            // verbatim, and a column you cannot copy from is no use.
            job.id.to_string(),
            kind_label(&job.kind),
            painter.job_status(job.status),
            progress_label(job),
            job.created_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            detail_label(job, painter),
        ]);
    }
    // Every column but the last keeps its natural width. A UUID broken across
    // lines cannot be double-clicked or pasted into `jobs show`, and a status
    // word split across two lines is worse than a row that overflows a narrow
    // terminal. Only the free-text detail gives way to fit the screen.
    for index in 0..DETAIL_COLUMN {
        if let Some(column) = table.column_mut(index) {
            column.set_constraint(ColumnConstraint::ContentWidth);
        }
    }
    if let Some(detail) = table.column_mut(DETAIL_COLUMN) {
        detail.set_constraint(ColumnConstraint::LowerBoundary(Width::Fixed(
            DETAIL_MIN_WIDTH,
        )));
    }
    table.to_string()
}

/// A job kind as one short word, plus the one parameter that changes what it
/// does to your data: a repair that applies is not the same job as a dry run.
fn kind_label(kind: &JobKind) -> String {
    match kind {
        JobKind::Demo { .. } => "demo".to_string(),
        JobKind::Mine { .. } => "mine".to_string(),
        JobKind::Checkpoint { .. } => "checkpoint".to_string(),
        JobKind::Audit { .. } => "audit".to_string(),
        JobKind::Repair { dry_run: true, .. } => "repair (dry run)".to_string(),
        JobKind::Repair { dry_run: false, .. } => "repair (apply)".to_string(),
    }
}

/// `3/10`, or just `3` when the total is not known, or `-` before any work.
fn progress_label(job: &Job) -> String {
    match (job.progress.current, job.progress.total) {
        (current, Some(total)) => format!("{current}/{total}"),
        (0, None) => "-".to_string(),
        (current, None) => current.to_string(),
    }
}

/// What a person scanning the list wants beside the status: why it failed,
/// otherwise what it is doing.
fn detail_label(job: &Job, painter: Painter) -> String {
    if let Some(error) = &job.error {
        return painter.error(&truncate(error));
    }
    match &job.progress.message {
        Some(message) => truncate(message),
        None => "-".to_string(),
    }
}

/// Collapse to one line and cut at [`DETAIL_MAX_CHARS`] characters (not
/// bytes: a cut inside a multi-byte character would panic).
fn truncate(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= DETAIL_MAX_CHARS {
        return single_line;
    }
    let kept: String = single_line.chars().take(DETAIL_MAX_CHARS - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{JobEvent, Priority};

    fn job(kind: JobKind) -> Job {
        Job::new(kind, Priority::Normal, "cli")
    }

    /// A failed job, reached the only way a job can get there: claimed, then
    /// failed through the state machine, never by assigning the status.
    fn failed_job(kind: JobKind, error: &str) -> Job {
        let mut job = job(kind);
        job.apply(JobEvent::Claim)
            .expect("a queued job can be claimed");
        job.apply(JobEvent::Fail).expect("a running job can fail");
        job.error = Some(error.to_string());
        job
    }

    #[test]
    fn an_empty_list_says_so_instead_of_printing_a_bare_header() {
        assert_eq!(render_jobs(&[], Painter::PLAIN, None), "No jobs.");
    }

    #[test]
    fn each_job_is_a_row_with_its_full_id_kind_and_status() {
        let demo = job(JobKind::Demo { steps: 3 });
        let failed = failed_job(JobKind::Audit { scope: None }, "boom");

        let text = render_jobs(&[demo.clone(), failed.clone()], Painter::PLAIN, None);

        for header in ["ID", "KIND", "STATUS", "PROGRESS", "CREATED", "DETAIL"] {
            assert!(text.contains(header), "missing {header}:\n{text}");
        }
        assert!(text.contains(&demo.id.to_string()), "{text}");
        assert!(text.contains(&failed.id.to_string()), "{text}");
        assert!(text.contains("demo") && text.contains("audit"), "{text}");
        assert!(text.contains("queued") && text.contains("failed"), "{text}");
    }

    #[test]
    fn a_plain_painter_leaves_no_escape_codes_in_the_table() {
        let text = render_jobs(&[job(JobKind::Demo { steps: 1 })], Painter::PLAIN, None);
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    #[test]
    fn a_coloured_painter_colours_the_status_but_keeps_the_word_readable() {
        let text = render_jobs(&[job(JobKind::Demo { steps: 1 })], Painter::forced(), None);
        assert!(text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("queued"), "{text:?}");
    }

    #[test]
    fn a_repair_that_applies_is_told_apart_from_a_dry_run() {
        let dry = job(JobKind::Repair {
            dry_run: true,
            based_on_job: None,
        });
        let apply = job(JobKind::Repair {
            dry_run: false,
            based_on_job: None,
        });
        assert_eq!(kind_label(&dry.kind), "repair (dry run)");
        assert_eq!(kind_label(&apply.kind), "repair (apply)");
    }

    #[test]
    fn progress_shows_a_fraction_when_the_total_is_known() {
        let mut job = job(JobKind::Demo { steps: 10 });
        assert_eq!(progress_label(&job), "-");
        job.progress.current = 3;
        assert_eq!(progress_label(&job), "3");
        job.progress.total = Some(10);
        assert_eq!(progress_label(&job), "3/10");
    }

    #[test]
    fn a_long_multiline_error_is_cut_to_one_short_line() {
        let failed = failed_job(
            JobKind::Demo { steps: 1 },
            &format!("first line\n{}", "é".repeat(200)),
        );

        let detail = detail_label(&failed, Painter::PLAIN);

        assert!(!detail.contains('\n'), "{detail:?}");
        assert_eq!(detail.chars().count(), DETAIL_MAX_CHARS, "{detail:?}");
        assert!(detail.ends_with('…'), "{detail:?}");
    }

    #[test]
    fn the_error_wins_over_the_progress_message_as_the_detail() {
        let mut failed = job(JobKind::Demo { steps: 1 });
        failed.progress.message = Some("step 1".to_string());
        failed.error = Some("boom".to_string());
        assert_eq!(detail_label(&failed, Painter::PLAIN), "boom");
    }
}
