//! Terminal presentation for the CLI: colour, "is a person at the other end",
//! and confirmation prompts.
//!
//! The rule everything here follows: **a person gets decoration, a pipe gets
//! plain data.** Colour is on only for a terminal that wants it (`console`
//! honours `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` and `TERM=dumb`), and a
//! prompt is shown only when both stdin and stderr are terminals. Scripts,
//! CI and the test-suite therefore never see an escape code or block on a
//! question.

use std::io::IsTerminal;

use console::Style;

use crate::domain::JobStatus;
use crate::error::{Error, Result};

/// Whether stdout is a terminal (as opposed to a pipe or a file).
///
/// Decides between a human rendering and machine-readable JSON for commands
/// whose output is a listing.
#[must_use]
pub fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

/// The terminal's width in columns, when stdout is one and it reports a real
/// size. A pseudo-terminal that was never sized reports 0 columns; taken
/// literally, a table would be squeezed to one character per column, so 0 is
/// "unknown" and the table keeps its natural width.
#[must_use]
pub fn terminal_width() -> Option<u16> {
    console::Term::stdout()
        .size_checked()
        .map(|(_rows, columns)| columns)
        .filter(|&columns| columns > 0)
}

/// Whether a person can answer a prompt: stdin to read the answer from and
/// stderr to show the question on. Prompts go to stderr so stdout stays
/// capturable (`memcastle auth generate | op item create ...`).
#[must_use]
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Colours stdout output, or not.
///
/// A value rather than a global, so a renderer can be tested in both modes
/// without touching the process environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Painter {
    color: bool,
}

impl Painter {
    /// Never colours. What every non-terminal output uses.
    pub const PLAIN: Self = Self { color: false };

    /// Colours when stdout is a terminal that supports it and the user has
    /// not opted out (`NO_COLOR`).
    #[must_use]
    pub fn for_stdout() -> Self {
        Self {
            color: console::colors_enabled(),
        }
    }

    /// Colours when stderr is a terminal that supports it and the user has
    /// not opted out. Separate from [`Self::for_stdout`]: `cmd | less` leaves
    /// stderr on the terminal, and the two streams are decided independently.
    #[must_use]
    pub fn for_stderr() -> Self {
        Self {
            color: console::colors_enabled_stderr(),
        }
    }

    /// Always colours. Only for tests of the coloured rendering.
    #[must_use]
    pub const fn forced() -> Self {
        Self { color: true }
    }

    /// Whether this painter emits escape codes.
    #[must_use]
    pub const fn is_colored(self) -> bool {
        self.color
    }

    /// Apply `style` to `text`, or return it untouched.
    ///
    /// `force_styling(self.color)` rather than relying on `console`'s global
    /// detection: the decision was made once, in the constructor, and must not
    /// be re-made (differently) per call.
    fn paint(self, style: Style, text: &str) -> String {
        if self.color {
            style.force_styling(true).apply_to(text).to_string()
        } else {
            text.to_string()
        }
    }

    /// Something that worked or is healthy.
    #[must_use]
    pub fn ok(self, text: &str) -> String {
        self.paint(Style::new().green(), text)
    }

    /// Something that deserves attention but is not a failure.
    #[must_use]
    pub fn warn(self, text: &str) -> String {
        self.paint(Style::new().yellow(), text)
    }

    /// Something that failed or is unavailable.
    #[must_use]
    pub fn error(self, text: &str) -> String {
        self.paint(Style::new().red().bold(), text)
    }

    /// A heading or the headline of a report.
    #[must_use]
    pub fn heading(self, text: &str) -> String {
        self.paint(Style::new().bold(), text)
    }

    /// Secondary text: labels, hints, things that are fine to skim past.
    #[must_use]
    pub fn dim(self, text: &str) -> String {
        self.paint(Style::new().dim(), text)
    }

    /// A value the user may want to copy: a URL, a command, an id.
    #[must_use]
    pub fn accent(self, text: &str) -> String {
        self.paint(Style::new().cyan(), text)
    }

    /// A job status in the colour its meaning has everywhere it is shown:
    /// in-flight work cyan, waiting work yellow, a pause magenta, success
    /// green, failure red, and a withdrawn job dim.
    #[must_use]
    pub fn job_status(self, status: JobStatus) -> String {
        let style = match status {
            JobStatus::Queued => Style::new().yellow(),
            JobStatus::Running => Style::new().cyan().bold(),
            JobStatus::Paused => Style::new().magenta(),
            JobStatus::Completed => Style::new().green(),
            JobStatus::Failed => Style::new().red().bold(),
            JobStatus::Cancelled => Style::new().dim(),
        };
        self.paint(style, status.as_str())
    }
}

/// Ask the user to confirm `question` before `action` happens.
///
/// Returns `Ok(())` when the action may proceed:
///
/// - `assume_yes` is set (`--yes`), or
/// - nobody is there to ask. A script that runs `memcastle repair --apply`
///   has already decided, and blocking it on stdin would hang CI until a
///   timeout kills it.
///
/// # Errors
///
/// [`Error::Aborted`] when the user declines, so a chained command stops and
/// the exit code says nothing was done. [`Error::PromptFailed`] when the
/// terminal could not be read (Ctrl-C included).
pub fn confirm(question: &str, action: &str, assume_yes: bool) -> Result<()> {
    confirm_with(assume_yes, is_interactive(), action, || ask(question))
}

/// [`confirm`] with the terminal probed and the prompt injected, so the
/// decision table can be tested without a terminal.
fn confirm_with(
    assume_yes: bool,
    interactive: bool,
    action: &str,
    ask: impl FnOnce() -> Result<bool>,
) -> Result<()> {
    if assume_yes || !interactive {
        return Ok(());
    }
    if ask()? {
        Ok(())
    } else {
        Err(Error::aborted(action))
    }
}

/// Show the yes/no prompt on stderr. Defaults to "no": pressing Enter by
/// reflex must never be the thing that applies a destructive change.
fn ask(question: &str) -> Result<bool> {
    dialoguer::Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt(question)
        .default(false)
        .interact_on(&console::Term::stderr())
        .map_err(|error| Error::prompt_failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESCAPE: char = '\u{1b}';

    #[test]
    fn a_plain_painter_returns_the_text_untouched_for_every_role() {
        let painter = Painter::PLAIN;
        for painted in [
            painter.ok("x"),
            painter.warn("x"),
            painter.error("x"),
            painter.heading("x"),
            painter.dim("x"),
            painter.accent("x"),
        ] {
            assert_eq!(painted, "x");
        }
    }

    #[test]
    fn a_coloured_painter_wraps_the_text_in_escape_codes_for_every_role() {
        let painter = Painter::forced();
        for painted in [
            painter.ok("x"),
            painter.warn("x"),
            painter.error("x"),
            painter.heading("x"),
            painter.dim("x"),
            painter.accent("x"),
        ] {
            assert!(painted.starts_with(ESCAPE), "{painted:?}");
            assert!(painted.contains('x'), "{painted:?}");
        }
    }

    #[test]
    fn every_job_status_is_shown_under_its_wire_name_and_each_gets_its_own_colour() {
        let all = [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Paused,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ];
        let mut seen = std::collections::HashSet::new();
        for status in all {
            assert_eq!(Painter::PLAIN.job_status(status), status.as_str());
            let coloured = Painter::forced().job_status(status);
            assert!(coloured.contains(status.as_str()), "{coloured:?}");
            // Two statuses sharing a colour could not be told apart at a
            // glance, which is the whole point of colouring them.
            assert!(seen.insert(coloured), "{status} shares a style");
        }
    }

    #[test]
    fn yes_skips_the_prompt_even_on_a_terminal() {
        let outcome = confirm_with(true, true, "doing it", || {
            panic!("--yes must not prompt");
        });
        assert!(outcome.is_ok());
    }

    #[test]
    fn without_a_terminal_the_action_proceeds_and_nothing_is_asked() {
        let outcome = confirm_with(false, false, "doing it", || {
            panic!("a pipe cannot answer a prompt");
        });
        assert!(outcome.is_ok());
    }

    #[test]
    fn a_confirmed_prompt_lets_the_action_proceed() {
        assert!(confirm_with(false, true, "doing it", || Ok(true)).is_ok());
    }

    #[test]
    fn a_declined_prompt_aborts_with_the_action_named() {
        let error = confirm_with(false, true, "applying repairs", || Ok(false)).unwrap_err();
        assert!(matches!(&error, Error::Aborted { action } if action == "applying repairs"));
    }

    #[test]
    fn a_prompt_that_cannot_be_read_is_an_error_not_a_yes() {
        let error = confirm_with(false, true, "doing it", || {
            Err(Error::prompt_failed("interrupted"))
        })
        .unwrap_err();
        assert!(matches!(error, Error::PromptFailed { .. }));
    }
}
