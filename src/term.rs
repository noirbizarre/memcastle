//! Terminal presentation for the CLI: colour, "is a person at the other end",
//! and confirmation prompts.
//!
//! The rule everything here follows: **a person gets decoration, a pipe gets
//! plain data.** Colour follows the output stream, `FORCE_COLOR` and `NO_COLOR`, and a
//! prompt is shown only when both stdin and stderr are terminals. Scripts,
//! CI and the test-suite therefore never block on a question or receive coloured JSON.

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

/// Decide colour per stream; a deliberate opt-out always wins over a forced opt-in.
/// `TERM=dumb` only disables automatic detection, so an explicit force can override it.
#[must_use]
pub fn color_for(terminal: bool) -> bool {
    color_for_env(
        terminal,
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var("FORCE_COLOR").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    )
}

fn color_for_env(
    terminal: bool,
    no_color: bool,
    force_color: Option<&str>,
    term: Option<&str>,
) -> bool {
    if no_color {
        return false;
    }
    match force_color {
        Some("0") => false,
        Some(value) if !value.is_empty() => true,
        _ => terminal && term != Some("dumb"),
    }
}

/// Whether stdout should use colour for human-oriented output.
#[must_use]
pub fn stdout_color() -> bool {
    color_for(stdout_is_terminal())
}

/// Whether stderr should use colour for human-oriented output.
#[must_use]
pub fn stderr_color() -> bool {
    color_for(std::io::stderr().is_terminal())
}

/// Colour only JSON shown directly to a person: forced colour must not break a pipe's JSON.
#[must_use]
pub fn terminal_json_color() -> bool {
    stdout_is_terminal() && stdout_color()
}

/// Highlight serialized JSON without touching its bytes or confusing an escaped quote for a delimiter.
#[must_use]
pub fn highlight_json(text: &str, painter: Painter) -> String {
    if !painter.is_colored() {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    while offset < bytes.len() {
        let start = offset;
        let style = match bytes[offset] {
            b'"' => {
                offset += 1;
                while offset < bytes.len() {
                    if bytes[offset] == b'\\' {
                        offset += 2;
                    } else if bytes[offset] == b'"' {
                        offset += 1;
                        break;
                    } else {
                        offset += 1;
                    }
                }
                let is_key = bytes[offset..]
                    .iter()
                    .copied()
                    .find(|byte| !byte.is_ascii_whitespace())
                    == Some(b':');
                if is_key {
                    Style::new().cyan()
                } else {
                    Style::new().green()
                }
            }
            b'-' | b'0'..=b'9' => {
                offset += 1;
                while offset < bytes.len()
                    && matches!(
                        bytes[offset],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    offset += 1;
                }
                Style::new().yellow()
            }
            b't' | b'f' | b'n' => {
                offset += 1;
                while offset < bytes.len() && bytes[offset].is_ascii_alphabetic() {
                    offset += 1;
                }
                Style::new().magenta()
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                offset += 1;
                Style::new().dim()
            }
            _ => {
                // An unstyled Unicode character is still a whole UTF-8 scalar, not one byte of a slice.
                offset += text[offset..].chars().next().unwrap().len_utf8();
                output.push_str(&text[start..offset]);
                continue;
            }
        };
        output.push_str(&painter.paint(style, &text[start..offset]));
    }
    output
}

/// Whether `--json` was given. Set once at startup; see [`set_json`].
static JSON_FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Record the global `--json` flag.
///
/// A process-wide value rather than a parameter: the flag is global, so every command
/// would otherwise have to thread it through to the one place that prints. Set once;
/// a second call is ignored, so a late caller cannot flip the contract mid-run.
pub fn set_json(json: bool) {
    let _ = JSON_FLAG.set(json);
}

/// The one rule every command's output follows: a person at a terminal gets the
/// pretty rendering, anything else (a pipe, a file, `--json`) gets JSON.
///
/// A pure function of its inputs so the four cases are testable without a terminal.
#[must_use]
pub const fn is_pretty(json_flag: bool, stdout_is_terminal: bool) -> bool {
    !json_flag && stdout_is_terminal
}

/// Whether this command should print its pretty rendering rather than JSON.
#[must_use]
pub fn pretty() -> bool {
    is_pretty(
        JSON_FLAG.get().copied().unwrap_or(false),
        stdout_is_terminal(),
    )
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
    /// Never colours. What machine-readable output uses.
    pub const PLAIN: Self = Self { color: false };

    /// Colours according to stdout and the user's `FORCE_COLOR`/`NO_COLOR` settings.
    #[must_use]
    pub fn for_stdout() -> Self {
        Self {
            color: stdout_color(),
        }
    }

    /// Colours according to stderr and the user's settings.
    /// Separate from [`Self::for_stdout`]: `cmd | less` leaves
    /// stderr on the terminal, and the two streams are decided independently.
    #[must_use]
    pub fn for_stderr() -> Self {
        Self {
            color: stderr_color(),
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

/// Open `url` in the user's browser, best effort, and say whether something was started.
///
/// Only an `https` address (or `http` to this machine) is opened: the address comes from a provider's answer through
/// the daemon, and handing a program an arbitrary one (a `file:` path, a custom scheme) would let that answer start
/// whatever is registered for it. Always paired with printing the address, because a headless host has no browser.
#[must_use]
pub fn open_in_browser(url: &str) -> bool {
    if !crate::domain::is_secure_endpoint(url) {
        return false;
    }
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(windows)]
    let (program, args): (&str, Vec<&str>) = ("rundll32", vec!["url.dll,FileProtocolHandler", url]);
    #[cfg(not(any(target_os = "macos", windows)))]
    let (program, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    let Ok(mut child) = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Reaped on a thread of its own, so a launcher that lingers neither delays the command nor leaves a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
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

/// Open `$VISUAL`, else `$EDITOR`, on a scratch file holding `initial`, and return what was saved.
///
/// The variable may carry arguments (`code --wait`), split on whitespace, which is what shells and `git` accept for it.
/// The scratch file is removed when this returns, so an abandoned note leaves nothing behind.
///
/// # Errors
///
/// [`Error::InvalidInput`] when no editor is configured or the editor exits unsuccessfully (a failed or cancelled edit
/// must not be saved as if it were finished), and [`Error::Io`] when the editor cannot be started or the file read.
pub fn edit(initial: &str) -> Result<String> {
    let configured = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty());
    edit_with(configured.as_deref(), initial)
}

/// [`edit`] with the editor command given, which is the seam the tests use instead of the real environment.
fn edit_with(editor: Option<&str>, initial: &str) -> Result<String> {
    use std::io::Write;

    let Some(editor) = editor else {
        return Err(Error::invalid_input(
            "note",
            "no editor is configured: set `$VISUAL` or `$EDITOR`, or give the note as an argument or on standard input",
        ));
    };
    let mut words = editor.split_whitespace();
    let Some(program) = words.next() else {
        return Err(Error::invalid_input(
            "note",
            "the configured editor is blank",
        ));
    };
    // A `.md` suffix is what lets an editor pick a sensible mode for what is usually prose.
    let mut file = tempfile::Builder::new()
        .prefix("memcastle-note-")
        .suffix(".md")
        .tempfile()
        .map_err(|source| Error::io("<scratch file>", source))?;
    file.write_all(initial.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|source| Error::io(file.path().display().to_string(), source))?;
    let status = std::process::Command::new(program)
        .args(words)
        .arg(file.path())
        .status()
        .map_err(|source| Error::io(program, source))?;
    if !status.success() {
        return Err(Error::invalid_input(
            "note",
            format!("the editor `{program}` exited with {status}, so nothing was saved"),
        ));
    }
    std::fs::read_to_string(file.path())
        .map_err(|source| Error::io(file.path().display().to_string(), source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_terminal_without_the_json_flag_gets_the_pretty_rendering() {
        // (json flag, stdout is a terminal) -> pretty
        assert!(is_pretty(false, true), "a person at a terminal");
        assert!(!is_pretty(true, true), "--json forces JSON on a terminal");
        assert!(!is_pretty(false, false), "a pipe always gets JSON");
        assert!(!is_pretty(true, false), "--json on a pipe is still JSON");
    }

    #[test]
    fn no_color_wins_and_force_color_only_changes_colour_not_output_form() {
        assert!(color_for_env(false, false, Some("1"), Some("dumb")));
        assert!(!color_for_env(true, true, Some("1"), None));
        assert!(!color_for_env(true, false, Some("0"), None));
        assert!(!color_for_env(false, false, None, None));
        assert!(!color_for_env(true, false, None, Some("dumb")));
        assert!(color_for_env(true, false, None, Some("xterm")));
        assert!(!is_pretty(true, true));
        assert!(!is_pretty(false, false));
    }

    #[test]
    fn highlighted_json_keeps_the_original_data_and_escapes() {
        let text = serde_json::to_string_pretty(&serde_json::json!({
            "key\"with\\backslash": ["unicode é 😀 and \"escaped\"", -12.5e+3, true, false, null, {"a": 0}]
        })).unwrap();
        assert_eq!(highlight_json(&text, Painter::PLAIN), text);
        let highlighted = highlight_json(&text, Painter::forced());
        assert!(highlighted.contains('\u{1b}'));
        assert!(highlighted.contains("é 😀"));
        assert!(text.contains("true"));
        assert_eq!(console::strip_ansi_codes(&highlighted), text);
        assert_eq!(
            console::strip_ansi_codes(&highlight_json("-12.5e+3", Painter::forced())),
            "-12.5e+3"
        );
    }

    #[test]
    fn editing_without_an_editor_says_how_to_configure_one() {
        let error = edit_with(None, "").unwrap_err();
        assert!(error.to_string().contains("$VISUAL"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn the_editor_is_given_the_seed_and_what_it_saves_is_returned() {
        // A script run through `sh` stands in for an editor, with an argument before the file it is handed. It
        // appends to what it finds, which proves both that the seed was written and that the result is read back.
        // `sed -i` is no stand-in: its argument syntax differs between GNU and BSD, so it fails on macOS.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("editor.sh");
        std::fs::write(&script, "printf '%s' \"$1 \" >> \"$2\"\n").unwrap();
        let editor = format!("sh {} added", script.display());

        let saved = edit_with(Some(&editor), "a draft note\n").unwrap();

        assert_eq!(saved, "a draft note\nadded ");
    }

    #[cfg(unix)]
    #[test]
    fn an_editor_that_fails_saves_nothing() {
        let error = edit_with(Some("false"), "x").unwrap_err();
        assert!(error.to_string().contains("nothing was saved"), "{error}");
    }

    #[test]
    fn an_editor_that_cannot_start_is_reported_by_name() {
        let error = edit_with(Some("memcastle-no-such-editor"), "x").unwrap_err();
        assert!(
            error.to_string().contains("memcastle-no-such-editor"),
            "{error}"
        );
    }

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
