//! Human-readable listings for the CLI.
//!
//! Only used when stdout is a terminal. A pipe gets the same data as JSON
//! (see `main.rs`), so `memcastle job list | jq` keeps working and the table
//! never needs to be parseable.

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{ColumnConstraint, ContentArrangement, Table, Width};

use crate::domain::{DrawerSummary, Job, JobKind, RoomSummary, WingSummary};
use crate::term::Painter;

/// Index of the jobs table's free-text column.
const DETAIL_COLUMN: usize = 5;

/// The least width, in characters, a free-text column is squeezed to. Below
/// this a wrapped sentence is unreadable, so the table overflows a very narrow
/// terminal instead.
const FLEX_MIN_WIDTH: u16 = 12;

/// Lay `rows` out under `headers` as a table.
///
/// The `flex` column is the one free-text column. Every other column keeps its
/// natural width, so an id can still be double-clicked and a status word is
/// never split. The flex column then takes what it needs and no more: its own
/// content width when the terminal has room, otherwise whatever the others
/// leave, wrapping its text onto further lines (comfy-table does the fitting;
/// nothing here measures or crops).
///
/// `width` is the terminal's width in columns, when known. Without it
/// (a pseudo-terminal that was never sized) there is nothing to fit against, so
/// every column keeps its natural width.
pub(super) fn render_table(
    headers: &[&str],
    rows: Vec<Vec<String>>,
    flex: usize,
    painter: Painter,
    width: Option<u16>,
) -> String {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        // The width is ours to decide (below), not comfy-table's: it would
        // ask crossterm, which reports 0 columns for a terminal that was never
        // sized and squeezes every column to a single character.
        .force_no_tty()
        .set_header(headers.iter().map(|title| painter.heading(title)));
    if let Some(width) = width {
        table.set_width(width);
    }
    for row in rows {
        table.add_row(row);
    }
    for index in 0..headers.len() {
        let Some(column) = table.column_mut(index) else {
            continue;
        };
        column.set_constraint(if index == flex {
            // Only a floor: with no ceiling the column grows to its content
            // when there is room, which is the point.
            ColumnConstraint::LowerBoundary(Width::Fixed(FLEX_MIN_WIDTH))
        } else {
            ColumnConstraint::ContentWidth
        });
    }
    table.to_string()
}

/// Render `jobs` as a table: one row per job, in the order given.
///
/// `width` is the terminal's width in columns when known, see [`render_table`].
#[must_use]
pub fn render_jobs(jobs: &[Job], painter: Painter, width: Option<u16>) -> String {
    if jobs.is_empty() {
        return painter.dim("No jobs.");
    }
    let rows = jobs
        .iter()
        .map(|job| {
            vec![
                // The full id, never a prefix: `job show`/`cancel` take it
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
            ]
        })
        .collect();
    render_table(
        &["ID", "KIND", "STATUS", "PROGRESS", "CREATED", "DETAIL"],
        rows,
        DETAIL_COLUMN,
        painter,
        width,
    )
}

/// Render the mining sources: the adapters the daemon ships, then the sources
/// it has mined with where each run stopped.
#[must_use]
pub fn render_sources(
    report: &crate::app::SourcesReport,
    painter: Painter,
    width: Option<u16>,
) -> String {
    let yes_no = |value: bool| if value { "yes" } else { "no" }.to_string();
    let adapters = render_table(
        &[
            "SOURCE",
            "ORIGIN",
            "STATE",
            "INCREMENTAL",
            "KEEPS RAW",
            "CREDENTIALS",
            "PERMISSIONS",
            "READS",
        ],
        report
            .adapters
            .iter()
            .map(|source| {
                vec![
                    // A package's version is part of what it is; a built-in source is versioned with MemCastle.
                    source.version.as_ref().map_or_else(
                        || source.name.clone(),
                        |version| format!("{} {version}", source.name),
                    ),
                    source.origin.to_string(),
                    source.state.to_string(),
                    yes_no(source.capabilities.incremental),
                    yes_no(source.capabilities.retains_raw),
                    yes_no(source.capabilities.needs_credentials),
                    // Built-in sources are native code under MemCastle's own authority: nothing was granted.
                    match source.origin {
                        crate::mining::SourceOrigin::Builtin => "built in".to_string(),
                        _ => source.permissions.describe(),
                    },
                    source.description.clone(),
                ]
            })
            .collect(),
        7,
        painter,
        width,
    );
    // Why a source cannot run is what someone looking at "unavailable" wants next.
    let unavailable: String = report
        .adapters
        .iter()
        .filter_map(|source| {
            source.unavailable_reason.as_ref().map(|reason| {
                format!(
                    "{}\n",
                    painter.warn(&format!("{} is unavailable: {reason}", source.name))
                )
            })
        })
        .collect();
    let adapters = format!(
        "{adapters}{}",
        if unavailable.is_empty() {
            String::new()
        } else {
            format!("\n{}", unavailable.trim_end())
        }
    );
    if report.sources.is_empty() {
        return format!(
            "{adapters}\n{}",
            painter.dim("No source has been mined yet.")
        );
    }
    let mined = render_table(
        &["SOURCE", "LOCATOR", "DOCUMENTS", "LAST RUN", "LAST JOB"],
        report
            .sources
            .iter()
            .map(|source| {
                vec![
                    source.source.clone(),
                    source.locator.clone(),
                    source.documents.to_string(),
                    source
                        .last_run_at
                        .map_or_else(|| "never".to_string(), local_minute),
                    source
                        .last_job
                        .map_or_else(|| "-".to_string(), |job| job.to_string()),
                ]
            })
            .collect(),
        1,
        painter,
        width,
    );
    format!("{adapters}\n{mined}")
}

/// Render one source: what it is, what it can do, what it was granted and why it cannot run, if it cannot.
#[must_use]
pub fn render_source(source: &crate::mining::AdapterInfo, painter: Painter) -> String {
    let yes_no = |value: bool| if value { "yes" } else { "no" };
    let mut lines = vec![
        format!(
            "{} {}",
            painter.heading(&source.name),
            source.version.as_deref().unwrap_or("(built in)")
        ),
        format!("  {}", source.description),
        format!("  state:        {}", source.state),
        format!(
            "  capabilities: incremental {}, keeps raw {}, needs credentials {}",
            yes_no(source.capabilities.incremental),
            yes_no(source.capabilities.retains_raw),
            yes_no(source.capabilities.needs_credentials)
        ),
    ];
    if source.origin != crate::mining::SourceOrigin::Builtin {
        lines.push(format!("  origin:       {}", source.origin));
        if let Some(registry) = &source.registry {
            lines.push(format!("  registry:     {registry}"));
        }
        if let Some(key) = &source.signed_by {
            lines.push(format!("  signed by:    key {key}"));
        }
        lines.push(format!("  permissions:  {}", source.permissions.describe()));
    }
    if let Some(reason) = &source.unavailable_reason {
        lines.push(format!(
            "  {}",
            painter.warn(&format!("unavailable: {reason}"))
        ));
    }
    lines.join("\n")
}

/// Render what `memcastle source search` found: one row per source, with the version `install` would take and what is
/// installed already.
#[must_use]
pub fn render_registry_search(
    search: &crate::app::RegistrySearch,
    painter: Painter,
    width: Option<u16>,
) -> String {
    let warnings: String = search
        .warnings
        .iter()
        .map(|warning| format!("\n{}", painter.warn(&format!("warning: {warning}"))))
        .collect();
    if search.entries.is_empty() {
        return format!("{}{warnings}", painter.dim("No source matches."));
    }
    let table = render_table(
        &["SOURCE", "VERSION", "FROM", "INSTALLED", "READS"],
        search
            .entries
            .iter()
            .map(|entry| {
                vec![
                    entry.name.clone(),
                    entry
                        .version
                        .clone()
                        // A source with no installable version is still listed, with why, so it is not mistaken for
                        // a source nobody offers.
                        .unwrap_or_else(|| "none".to_string()),
                    entry.origin.to_string(),
                    match (&entry.installed, entry.update_available) {
                        (Some(installed), true) => {
                            format!("{} (update available)", installed.version)
                        }
                        (Some(installed), false) => installed.version.clone(),
                        (None, _) => "-".to_string(),
                    },
                    entry.description.clone(),
                ]
            })
            .collect(),
        4,
        painter,
        width,
    );
    let unavailable: String = search
        .entries
        .iter()
        .filter_map(|entry| {
            entry.unavailable_reason.as_ref().map(|reason| {
                format!(
                    "\n{}",
                    painter.warn(&format!("{} cannot be installed: {reason}", entry.name))
                )
            })
        })
        .collect();
    format!("{table}{unavailable}{warnings}")
}

/// Render the sources that have an update.
#[must_use]
pub fn render_update_check(
    check: &crate::app::UpdateCheck,
    painter: Painter,
    width: Option<u16>,
) -> String {
    let warnings: String = check
        .warnings
        .iter()
        .map(|warning| format!("\n{}", painter.warn(&format!("warning: {warning}"))))
        .collect();
    if check.updates.is_empty() {
        return format!("{}{warnings}", painter.dim("Every source is up to date."));
    }
    let table = render_table(
        &["SOURCE", "INSTALLED", "AVAILABLE", "FROM"],
        check
            .updates
            .iter()
            .map(|update| {
                vec![
                    update.name.clone(),
                    update.installed.clone(),
                    update.available.clone(),
                    update.origin.to_string(),
                ]
            })
            .collect(),
        0,
        painter,
        width,
    );
    format!(
        "{table}\n{}{warnings}",
        painter.dim("Run `memcastle source update` to install them.")
    )
}

/// Render what an update did, one line per source.
#[must_use]
pub fn render_updates(outcomes: &[crate::app::UpdateOutcome], painter: Painter) -> String {
    use crate::app::UpdateStatus;
    if outcomes.is_empty() {
        return painter.dim("Every source is up to date.");
    }
    outcomes
        .iter()
        .map(|outcome| match &outcome.status {
            UpdateStatus::Updated => format!(
                "{} {} {} -> {}",
                painter.ok("Updated"),
                outcome.name,
                outcome.from,
                outcome.to.as_deref().unwrap_or("?")
            ),
            UpdateStatus::Current => format!(
                "{} {} {} is the newest version",
                painter.dim("Current"),
                outcome.name,
                outcome.from
            ),
            UpdateStatus::NeedsConsent {
                permissions,
                digest,
            } => format!(
                "{} {} {} asks for permissions you have not agreed to: {permissions}\n  \
                 run `memcastle source update {} --yes` after reviewing them, or pass `--consent {digest}` to `install`",
                painter.warn("Waiting"),
                outcome.name,
                outcome.to.as_deref().unwrap_or("?"),
                outcome.name
            ),
            UpdateStatus::Failed { message } => format!(
                "{} {}: {message}",
                painter.error("Failed"),
                outcome.name
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render `wings` as a table: one row per wing, with its counts.
///
/// Wings are addressed by name, so there is no id column to copy from.
#[must_use]
pub fn render_wings(wings: &[WingSummary], painter: Painter, width: Option<u16>) -> String {
    if wings.is_empty() {
        return painter.dim("No wings.");
    }
    let rows = wings
        .iter()
        .map(|summary| {
            vec![
                summary.wing.name.clone(),
                summary.rooms.to_string(),
                summary.drawers.to_string(),
                local_minute(summary.wing.created_at),
                summary
                    .wing
                    .description
                    .as_deref()
                    .map_or_else(|| "-".to_string(), one_line),
            ]
        })
        .collect();
    render_table(
        &["WING", "ROOMS", "DRAWERS", "CREATED", "DESCRIPTION"],
        rows,
        4,
        painter,
        width,
    )
}

/// Render `rooms` as a table. The wing is a column because a listing may span
/// wings, and `wing/room` is how a room is addressed.
#[must_use]
pub fn render_rooms(rooms: &[RoomSummary], painter: Painter, width: Option<u16>) -> String {
    if rooms.is_empty() {
        return painter.dim("No rooms.");
    }
    let rows = rooms
        .iter()
        .map(|summary| {
            vec![
                summary.wing_name.clone(),
                summary.room.name.clone(),
                summary.drawers.to_string(),
                local_minute(summary.room.created_at),
                summary
                    .room
                    .description
                    .as_deref()
                    .map_or_else(|| "-".to_string(), one_line),
            ]
        })
        .collect();
    render_table(
        &["WING", "ROOM", "DRAWERS", "CREATED", "DESCRIPTION"],
        rows,
        4,
        painter,
        width,
    )
}

/// Render `drawers` as a table. The id is always shown in full: an unnamed
/// drawer can only be addressed by it.
#[must_use]
pub fn render_drawers(drawers: &[DrawerSummary], painter: Painter, width: Option<u16>) -> String {
    if drawers.is_empty() {
        return painter.dim("No drawers.");
    }
    let rows = drawers
        .iter()
        .map(|drawer| {
            vec![
                drawer.id.to_string(),
                drawer.name.clone().unwrap_or_else(|| "-".to_string()),
                drawer.chars.to_string(),
                local_minute(drawer.created_at),
                one_line(&drawer.preview),
            ]
        })
        .collect();
    render_table(
        &["ID", "NAME", "CHARS", "CREATED", "PREVIEW"],
        rows,
        4,
        painter,
        width,
    )
}

/// A timestamp in the reader's timezone, to the minute.
pub(super) fn local_minute(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// A job kind as one short word, plus the one parameter that changes what it
/// does to your data: a repair that applies is not the same job as a dry run.
pub(super) fn kind_label(kind: &JobKind) -> String {
    match kind {
        JobKind::Demo { .. } => "demo".to_string(),
        JobKind::Mine { .. } => "mine".to_string(),
        JobKind::Checkpoint { .. } => "checkpoint".to_string(),
        JobKind::Audit { .. } => "audit".to_string(),
        JobKind::Embed { .. } => "embed".to_string(),
        JobKind::Extract { .. } => "extract".to_string(),
        JobKind::Repair { dry_run: true, .. } => "repair (dry run)".to_string(),
        JobKind::Repair { dry_run: false, .. } => "repair (apply)".to_string(),
    }
}

/// `3/10`, or just `3` when the total is not known, or `-` before any work.
pub(super) fn progress_label(job: &Job) -> String {
    match (job.progress.current, job.progress.total) {
        (current, Some(total)) => format!("{current}/{total}"),
        (0, None) => "-".to_string(),
        (current, None) => current.to_string(),
    }
}

/// What a person scanning the list wants beside the status: why it failed,
/// otherwise what it is doing. Always in full: the table wraps it to the
/// terminal, so nothing is lost to a hard-coded cut.
fn detail_label(job: &Job, painter: Painter) -> String {
    if let Some(error) = &job.error {
        return painter.error(&one_line(error));
    }
    match &job.progress.message {
        Some(message) => one_line(message),
        None => "-".to_string(),
    }
}

/// Collapse every run of whitespace, newlines included, to one space. A
/// multi-line error would otherwise put hard line breaks in a cell, which the
/// table cannot re-flow when the terminal is narrow.
pub(super) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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
        let failed = failed_job(JobKind::Audit { wing: None }, "boom");

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

    /// The visible width of the widest line, escape codes not counted.
    fn widest_line(text: &str) -> usize {
        text.lines()
            .map(console::measure_text_width)
            .max()
            .unwrap_or(0)
    }

    /// A table with one failed job whose error is `error`, rendered plain at `width`.
    fn failed_table(error: &str, width: Option<u16>) -> (Job, String) {
        let failed = failed_job(JobKind::Demo { steps: 1 }, error);
        let text = render_jobs(std::slice::from_ref(&failed), Painter::PLAIN, width);
        (failed, text)
    }

    #[test]
    fn a_detail_longer_than_the_old_sixty_character_cap_is_shown_whole_on_a_wide_terminal() {
        let error = "the daemon lost its lease on the palace while it was writing a batch of drawers and gave up";
        assert!(error.chars().count() > 60);

        let (_, text) = failed_table(error, Some(300));

        assert!(text.contains(error), "{text}");
        assert!(!text.contains('…'), "{text}");
        assert_eq!(text.lines().count(), 5, "one row, no wrapping:\n{text}");
    }

    #[test]
    fn the_detail_takes_only_the_width_it_needs_when_there_is_room_to_spare() {
        let (_, text) = failed_table("boom", Some(300));

        // Header, rule, row and the two borders: far narrower than the terminal.
        assert!(widest_line(&text) < 120, "{text}");
    }

    #[test]
    fn a_narrower_terminal_wraps_the_detail_and_keeps_every_word_of_it() {
        let error = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar";

        let (_, text) = failed_table(error, Some(100));

        assert!(widest_line(&text) <= 100, "{text}");
        assert!(text.lines().count() > 5, "the detail must wrap:\n{text}");
        for word in error.split(' ') {
            assert!(text.contains(word), "lost `{word}`:\n{text}");
        }
    }

    #[test]
    fn the_id_kind_and_status_are_never_wrapped_however_narrow_the_terminal() {
        let error = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike";
        for width in [200, 120, 90, 60, 20] {
            let (failed, text) = failed_table(error, Some(width));
            assert!(text.contains(&failed.id.to_string()), "{width}:\n{text}");
            assert!(text.contains("failed"), "{width}:\n{text}");
            assert!(text.contains("demo"), "{width}:\n{text}");
        }
    }

    #[test]
    fn a_terminal_too_narrow_for_the_other_columns_overflows_instead_of_breaking_them() {
        let (_, text) = failed_table("boom and more words to wrap around", Some(40));

        assert!(widest_line(&text) > 40, "{text}");
    }

    #[test]
    fn an_unknown_width_leaves_every_column_at_its_natural_width() {
        let error = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike";

        let (_, text) = failed_table(error, None);

        assert!(text.contains(error), "{text}");
        assert_eq!(text.lines().count(), 5, "{text}");
    }

    #[test]
    fn a_multi_line_error_is_collapsed_to_one_line_before_layout() {
        let (_, text) = failed_table("first line\n\n   second   line\n", Some(300));

        assert!(text.contains("first line second line"), "{text}");
    }

    #[test]
    fn a_coloured_table_has_the_same_words_as_the_plain_one_even_when_the_detail_wraps() {
        let failed = failed_job(
            JobKind::Demo { steps: 1 },
            "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november",
        );
        let jobs = std::slice::from_ref(&failed);

        let plain = render_jobs(jobs, Painter::PLAIN, Some(100));
        let coloured = render_jobs(jobs, Painter::forced(), Some(100));

        assert!(coloured.contains('\u{1b}'), "{coloured:?}");
        assert_eq!(console::strip_ansi_codes(&coloured), plain);
    }

    #[test]
    fn the_layout_is_not_specific_to_jobs() {
        // A different column count and a free-text column in the middle.
        let rows = vec![vec![
            "w-1".to_string(),
            "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima".to_string(),
            "ok".to_string(),
        ]];

        let wide = render_table(
            &["NAME", "NOTE", "STATE"],
            rows.clone(),
            1,
            Painter::PLAIN,
            Some(200),
        );
        let narrow = render_table(
            &["NAME", "NOTE", "STATE"],
            rows,
            1,
            Painter::PLAIN,
            Some(40),
        );

        assert_eq!(wide.lines().count(), 5, "{wide}");
        assert!(narrow.lines().count() > 5, "{narrow}");
        assert!(widest_line(&narrow) <= 40, "{narrow}");
        for text in [&wide, &narrow] {
            assert!(text.contains("w-1") && text.contains("ok"), "{text}");
        }
    }

    #[test]
    fn the_error_wins_over_the_progress_message_as_the_detail() {
        let mut failed = job(JobKind::Demo { steps: 1 });
        failed.progress.message = Some("step 1".to_string());
        failed.error = Some("boom".to_string());
        assert_eq!(detail_label(&failed, Painter::PLAIN), "boom");
    }

    fn wing_summary(name: &str, rooms: u64, drawers: u64) -> WingSummary {
        WingSummary {
            wing: crate::domain::Wing {
                id: crate::domain::WingId::new(),
                palace: crate::domain::PalaceId::new(),
                name: name.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
            },
            rooms,
            drawers,
        }
    }

    #[test]
    fn empty_palace_listings_say_so_instead_of_printing_a_bare_header() {
        assert_eq!(render_wings(&[], Painter::PLAIN, None), "No wings.");
        assert_eq!(render_rooms(&[], Painter::PLAIN, None), "No rooms.");
        assert_eq!(render_drawers(&[], Painter::PLAIN, None), "No drawers.");
    }

    #[test]
    fn a_wing_row_shows_its_name_and_counts() {
        let text = render_wings(&[wing_summary("work", 12, 37)], Painter::PLAIN, None);
        for expected in ["WING", "ROOMS", "DRAWERS", "work", "12", "37"] {
            assert!(text.contains(expected), "missing {expected}:\n{text}");
        }
        assert!(!text.contains('\u{1b}'), "a plain painter emits no escapes");
    }

    #[test]
    fn a_drawer_row_carries_its_full_id_and_a_dash_when_it_has_no_name() {
        let drawer = DrawerSummary {
            id: crate::domain::DrawerId::new(),
            room: crate::domain::RoomId::new(),
            name: None,
            chars: 5,
            preview: "line one\nline two".to_string(),
            source: crate::domain::Source {
                kind: crate::domain::SourceKind::Manual,
                uri: None,
                agent: None,
                origin: None,
            },
            created_at: chrono::Utc::now(),
        };
        let text = render_drawers(std::slice::from_ref(&drawer), Painter::PLAIN, Some(200));
        assert!(text.contains(&drawer.id.to_string()), "{text}");
        assert!(text.contains("line one line two"), "{text}");
        assert!(text.contains(" - "), "{text}");
    }

    fn source(name: &str, origin: crate::mining::SourceOrigin) -> crate::mining::AdapterInfo {
        crate::mining::AdapterInfo {
            name: name.to_string(),
            description: format!("reads {name}"),
            capabilities: crate::domain::SourceCapabilities {
                incremental: true,
                retains_raw: false,
                needs_credentials: false,
            },
            origin,
            version: None,
            state: crate::domain::SourceState::Enabled,
            unavailable_reason: None,
            permissions: crate::domain::Permissions::default(),
            registry: None,
            signed_by: None,
        }
    }

    fn package(name: &str) -> crate::mining::AdapterInfo {
        let mut info = source(name, crate::mining::SourceOrigin::Package);
        info.version = Some("1.2.3".to_string());
        info.permissions.network = true;
        info
    }

    #[test]
    fn a_built_in_source_is_listed_as_built_in_and_a_package_with_its_version_state_and_permissions()
     {
        let report = crate::app::SourcesReport {
            adapters: vec![
                source("directory", crate::mining::SourceOrigin::Builtin),
                package("slack"),
            ],
            sources: vec![],
        };

        let text = render_sources(&report, Painter::PLAIN, Some(200));

        for header in ["SOURCE", "STATE", "PERMISSIONS", "READS"] {
            assert!(text.contains(header), "missing {header}:\n{text}");
        }
        assert!(text.contains("built in"), "{text}");
        assert!(
            text.contains("slack 1.2.3"),
            "the version is part of what a package is:\n{text}"
        );
        assert!(text.contains("use the network"), "{text}");
        assert!(text.contains("No source has been mined yet."), "{text}");
    }

    #[test]
    fn why_a_source_is_unavailable_is_printed_under_the_table() {
        let mut broken = package("slack");
        broken.state = crate::domain::SourceState::Unavailable;
        broken.unavailable_reason = Some("its component file is missing".to_string());
        let report = crate::app::SourcesReport {
            adapters: vec![broken],
            sources: vec![],
        };

        let text = render_sources(&report, Painter::PLAIN, Some(200));

        assert!(text.contains("unavailable"), "{text}");
        assert!(
            text.contains("slack is unavailable: its component file is missing"),
            "{text}"
        );
    }

    #[test]
    fn mined_sources_are_listed_after_the_adapters_with_their_last_run() {
        let report = crate::app::SourcesReport {
            adapters: vec![source("directory", crate::mining::SourceOrigin::Builtin)],
            sources: vec![
                crate::app::SourceSummary {
                    id: crate::domain::SourceId::derive(uuid::Uuid::nil(), "a"),
                    source: "directory".into(),
                    account: None,
                    locator: "/notes".into(),
                    cursor: serde_json::Value::Null,
                    last_job: None,
                    last_run_at: None,
                    documents: 7,
                },
                crate::app::SourceSummary {
                    id: crate::domain::SourceId::derive(uuid::Uuid::nil(), "b"),
                    source: "directory".into(),
                    account: None,
                    locator: "/other".into(),
                    cursor: serde_json::Value::Null,
                    last_job: Some(crate::domain::JobId::new()),
                    last_run_at: Some(chrono::Utc::now()),
                    documents: 1,
                },
            ],
        };

        let text = render_sources(&report, Painter::PLAIN, Some(200));

        assert!(text.contains("/notes") && text.contains("/other"), "{text}");
        assert!(
            text.contains("never"),
            "a source that never ran says so:\n{text}"
        );
        assert!(text.contains("LAST JOB"), "{text}");
    }

    #[test]
    fn one_source_shows_its_capabilities_and_only_a_packages_permissions() {
        let built_in = render_source(
            &source("directory", crate::mining::SourceOrigin::Builtin),
            Painter::PLAIN,
        );
        assert!(built_in.contains("(built in)"), "{built_in}");
        assert!(
            !built_in.contains("permissions:"),
            "native code was granted nothing: {built_in}"
        );

        let mut broken = package("slack");
        broken.unavailable_reason = Some("it no longer matches".to_string());
        let text = render_source(&broken, Painter::PLAIN);
        assert!(text.contains("1.2.3"), "{text}");
        assert!(text.contains("permissions:  use the network"), "{text}");
        assert!(
            text.contains("incremental yes, keeps raw no, needs credentials no"),
            "{text}"
        );
        assert!(text.contains("unavailable: it no longer matches"), "{text}");
    }

    fn entry(name: &str) -> crate::app::RegistryEntry {
        crate::app::RegistryEntry {
            name: name.to_string(),
            description: format!("reads {name}"),
            origin: crate::mining::SourceOrigin::Registry,
            registry: "https://example.org/index.json".to_string(),
            version: Some("1.2.0".to_string()),
            unavailable_reason: None,
            license: None,
            installed: None,
            update_available: false,
        }
    }

    #[test]
    fn a_search_lists_the_version_to_install_and_what_is_installed_and_why_something_cannot_be() {
        let mut updatable = entry("slack");
        updatable.installed = Some(crate::app::InstalledVersion {
            version: "1.0.0".to_string(),
            state: crate::domain::SourceState::Enabled,
        });
        updatable.update_available = true;
        let mut stuck = entry("old");
        stuck.version = None;
        stuck.unavailable_reason = Some("it requires MemCastle >=9".to_string());
        let search = crate::app::RegistrySearch {
            entries: vec![entry("claude"), updatable, stuck],
            warnings: vec!["the registry `x` cannot be used".to_string()],
        };

        let text = render_registry_search(&search, Painter::PLAIN, Some(200));

        assert!(text.contains("claude") && text.contains("1.2.0"), "{text}");
        assert!(text.contains("1.0.0 (update available)"), "{text}");
        assert!(
            text.contains("old cannot be installed: it requires MemCastle >=9"),
            "{text}"
        );
        assert!(
            text.contains("warning: the registry `x` cannot be used"),
            "{text}"
        );
        let none = render_registry_search(
            &crate::app::RegistrySearch::default(),
            Painter::PLAIN,
            Some(200),
        );
        assert!(none.contains("No source matches."), "{none}");
    }

    #[test]
    fn an_update_report_says_what_changed_and_what_is_waiting_for_consent() {
        use crate::app::{UpdateOutcome, UpdateStatus};
        let outcome = |name: &str, status| UpdateOutcome {
            name: name.to_string(),
            from: "1.0.0".to_string(),
            to: Some("1.1.0".to_string()),
            status,
        };
        let text = render_updates(
            &[
                outcome("a", UpdateStatus::Updated),
                outcome("b", UpdateStatus::Current),
                outcome(
                    "c",
                    UpdateStatus::NeedsConsent {
                        permissions: "use the network".to_string(),
                        digest: "abc".to_string(),
                    },
                ),
                outcome(
                    "d",
                    UpdateStatus::Failed {
                        message: "boom".to_string(),
                    },
                ),
            ],
            Painter::PLAIN,
        );
        assert!(text.contains("Updated a 1.0.0 -> 1.1.0"), "{text}");
        assert!(text.contains("b 1.0.0 is the newest"), "{text}");
        assert!(
            text.contains("c 1.1.0 asks for permissions you have not agreed to: use the network"),
            "{text}"
        );
        assert!(text.contains("Failed d: boom"), "{text}");
        assert!(render_updates(&[], Painter::PLAIN).contains("up to date"));
    }

    #[test]
    fn the_update_check_lists_each_source_with_both_versions() {
        let check = crate::app::UpdateCheck {
            updates: vec![crate::app::UpdateCandidate {
                name: "slack".to_string(),
                installed: "1.0.0".to_string(),
                available: "1.1.0".to_string(),
                origin: crate::mining::SourceOrigin::Bundled,
            }],
            warnings: vec![],
        };
        let text = render_update_check(&check, Painter::PLAIN, Some(200));
        assert!(
            text.contains("slack") && text.contains("1.0.0") && text.contains("1.1.0"),
            "{text}"
        );
        assert!(text.contains("bundled"), "{text}");
        assert!(
            render_update_check(&crate::app::UpdateCheck::default(), Painter::PLAIN, None)
                .contains("up to date")
        );
    }
}
