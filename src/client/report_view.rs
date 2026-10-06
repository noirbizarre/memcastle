//! Human renderings of the short reports the CLI prints: what a migration did or would do, and a shutdown.
//!
//! Like [`super::table`] these are only used when the output is pretty (a terminal without `--json`).

use crate::migrate::{MigrationReport, MigrationStatus};
use crate::term::Painter;

/// What `memcastle migrate` did: the versions it moved between and the steps it applied.
#[must_use]
pub fn render_migration_report(report: &MigrationReport, painter: Painter) -> String {
    if report.applied.is_empty() {
        return format!(
            "{} (data version {})",
            painter.ok("Already up to date"),
            report.to_version
        );
    }
    let mut lines = vec![format!(
        "{} from data version {} to {}",
        painter.ok("Migrated"),
        report.from_version,
        report.to_version
    )];
    lines.extend(report.applied.iter().map(|step| format!("  {step}")));
    lines.join("\n")
}

/// What `memcastle migrate --status` and `--check` found: whether anything is pending, and which steps.
#[must_use]
pub fn render_migration_status(status: &MigrationStatus, painter: Painter) -> String {
    if status.pending.is_empty() {
        return format!(
            "{} (data version {})",
            painter.ok("Up to date"),
            status.current_version
        );
    }
    let mut lines = vec![format!(
        "{} {} pending: data is at version {} and this build expects {}",
        painter.warn("Migrations"),
        status.pending.len(),
        status.current_version,
        status.latest_version
    )];
    lines.extend(status.pending.iter().map(|step| format!("  {step}")));
    lines.push(format!(
        "Apply them with {}.",
        painter.accent("`memcastle migrate`")
    ));
    lines.join("\n")
}

/// The one line `memcastle daemon stop` prints: the daemon drains its jobs before it exits, so it is only asked to.
#[must_use]
pub fn render_shutdown(painter: Painter) -> String {
    format!("{} the daemon is shutting down", painter.ok("Stopping:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_applied_nothing_says_the_palace_is_up_to_date() {
        let report = MigrationReport {
            from_version: 3,
            to_version: 3,
            applied: Vec::new(),
        };
        assert_eq!(
            render_migration_report(&report, Painter::PLAIN),
            "Already up to date (data version 3)"
        );
    }

    #[test]
    fn a_run_that_applied_steps_lists_them_in_order() {
        let report = MigrationReport {
            from_version: 1,
            to_version: 3,
            applied: vec!["a".to_string(), "b".to_string()],
        };
        assert_eq!(
            render_migration_report(&report, Painter::PLAIN),
            "Migrated from data version 1 to 3\n  a\n  b"
        );
    }

    #[test]
    fn pending_migrations_are_listed_with_the_command_that_applies_them() {
        let status = MigrationStatus {
            current_version: 1,
            latest_version: 3,
            pending: vec!["a".to_string(), "b".to_string()],
        };
        let text = render_migration_status(&status, Painter::PLAIN);
        assert!(text.starts_with("Migrations 2 pending"), "{text}");
        assert!(text.contains("  a\n  b"), "{text}");
        assert!(text.ends_with("`memcastle migrate`."), "{text}");
    }

    #[test]
    fn nothing_pending_is_up_to_date() {
        let status = MigrationStatus {
            current_version: 3,
            latest_version: 3,
            pending: Vec::new(),
        };
        assert_eq!(
            render_migration_status(&status, Painter::PLAIN),
            "Up to date (data version 3)"
        );
    }
}
