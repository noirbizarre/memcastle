//! Human renderings of what retrieval returns: search and recall hits, the wake-up context and diary entries.
//!
//! Like [`super::table`] these are only used when the output is pretty (a terminal without `--json`).
//! Content is cut to a few lines here because a hit is a pointer to a drawer, not the drawer:
//! `drawer show` and the JSON form carry it whole.

use crate::app::WakeUpContext;
use crate::domain::Drawer;
use crate::search::SearchHit;
use crate::term::Painter;

use super::palace_view::field;

/// How many lines of a drawer's content a listing shows before it says there is more.
const SNIPPET_LINES: usize = 4;

/// The width a snippet line is cut to when the terminal's is unknown.
const DEFAULT_WIDTH: usize = 100;

/// The indent of a snippet under its heading.
const INDENT: &str = "    ";

/// The first lines of `content`, each cut to fit `width`, indented under a heading.
///
/// Blank lines are skipped so a snippet is dense, and a cut (of a line or of the whole) is marked with an ellipsis so
/// nobody mistakes the excerpt for the whole drawer.
fn snippet(content: &str, painter: Painter, width: Option<u16>) -> String {
    let room = width
        .map_or(DEFAULT_WIDTH, usize::from)
        .saturating_sub(INDENT.len())
        // Below this a line is unreadable, so a very narrow terminal overflows instead.
        .max(20);
    let mut lines = content
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty());
    let mut shown: Vec<String> = lines
        .by_ref()
        .take(SNIPPET_LINES)
        .map(|line| {
            if line.chars().count() > room {
                let cut: String = line.chars().take(room - 1).collect();
                format!("{INDENT}{cut}…")
            } else {
                format!("{INDENT}{line}")
            }
        })
        .collect();
    if lines.next().is_some() {
        shown.push(format!("{INDENT}{}", painter.dim("…")));
    }
    shown.join("\n")
}

/// Where a drawer says it came from, when it says: the mined document, else the name it is addressed by.
fn location(drawer: &Drawer) -> Option<String> {
    drawer.source.uri.clone().or_else(|| drawer.name.clone())
}

/// One drawer's heading line: its id, then the place it came from and its tags.
fn heading(drawer: &Drawer, painter: Painter) -> String {
    let mut parts = vec![painter.accent(&drawer.id.to_string())];
    if let Some(place) = location(drawer) {
        parts.push(place);
    }
    if !drawer.tags.is_empty() {
        parts.push(painter.dim(&format!("[{}]", drawer.tags.join(", "))));
    }
    parts.join("  ")
}

/// Search or recall hits, best first: a numbered heading with the score, then an excerpt.
#[must_use]
pub fn render_hits(hits: &[SearchHit], painter: Painter, width: Option<u16>) -> String {
    if hits.is_empty() {
        return painter.dim("No matches.");
    }
    hits.iter()
        .enumerate()
        .map(|(index, hit)| {
            let mut head = format!(
                "{} {}  {}",
                painter.dim(&format!("{}.", index + 1)),
                painter.heading(&format!("{:.3}", hit.score)),
                heading(&hit.drawer, painter),
            );
            // Why a hit that matched no word of the query is here at all.
            if !hit.via.is_empty() {
                head.push_str(&format!(
                    "\n{INDENT}{} {}",
                    painter.dim("via"),
                    hit.via.join(", ")
                ));
            }
            format!("{head}\n{}", snippet(&hit.drawer.content, painter, width))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The session-start context: the agent's last diary entry, then the recent highlights.
#[must_use]
pub fn render_wake_up(context: &WakeUpContext, painter: Painter, width: Option<u16>) -> String {
    let mut blocks = Vec::new();
    match &context.diary {
        Some(entry) => blocks.push(format!(
            "{}\n{}\n{}",
            painter.heading("Last diary entry"),
            field(painter, "Written", entry.created_at.to_rfc3339()),
            snippet(&entry.content, painter, width),
        )),
        None => blocks.push(format!(
            "{}\n{}",
            painter.heading("Last diary entry"),
            painter.dim("None."),
        )),
    }
    if context.recent_highlights.is_empty() {
        blocks.push(format!(
            "{}\n{}",
            painter.heading("Recent highlights"),
            painter.dim("None.")
        ));
    } else {
        let items: Vec<String> = context
            .recent_highlights
            .iter()
            .map(|drawer| {
                format!(
                    "{}\n{}",
                    heading(drawer, painter),
                    snippet(&drawer.content, painter, width)
                )
            })
            .collect();
        blocks.push(format!(
            "{}\n{}",
            painter.heading("Recent highlights"),
            items.join("\n")
        ));
    }
    blocks.push(field(
        painter,
        "Generated",
        context.generated_at.to_rfc3339(),
    ));
    blocks.join("\n\n")
}

/// Diary entries, newest first as the daemon returns them, each in full: a diary is read to be read.
#[must_use]
pub fn render_diary(entries: &[Drawer], painter: Painter) -> String {
    if entries.is_empty() {
        return painter.dim("No diary entries.");
    }
    entries
        .iter()
        .map(|entry| {
            format!(
                "{} {}\n{}",
                painter.heading(&entry.created_at.to_rfc3339()),
                painter.dim(&entry.id.to_string()),
                entry.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Drawer, DrawerId, Provenance, RoomId, Source, SourceKind};

    fn drawer(content: &str) -> Drawer {
        Drawer::new(
            DrawerId::new(),
            RoomId::new(),
            content.to_string(),
            Source {
                kind: SourceKind::Note,
                uri: None,
                agent: None,
                origin: None,
            },
            Vec::new(),
            Provenance {
                requested_by: "cli".to_string(),
                job_id: None,
            },
        )
    }

    fn hit(content: &str, score: f32) -> SearchHit {
        SearchHit {
            drawer: drawer(content),
            score,
            signals: Default::default(),
            via: Vec::new(),
        }
    }

    #[test]
    fn no_hits_says_so_instead_of_printing_nothing() {
        assert_eq!(render_hits(&[], Painter::PLAIN, None), "No matches.");
    }

    #[test]
    fn hits_are_numbered_in_the_order_given_with_their_score() {
        let text = render_hits(
            &[hit("first", 0.9), hit("second", 0.25)],
            Painter::PLAIN,
            None,
        );
        assert!(text.starts_with("1. 0.900  "), "{text}");
        assert!(text.contains("\n\n2. 0.250  "), "{text}");
        assert!(
            text.contains("    first") && text.contains("    second"),
            "{text}"
        );
    }

    #[test]
    fn a_long_drawer_is_cut_to_a_few_lines_and_marked_as_cut() {
        let content = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = render_hits(&[hit(&content, 1.0)], Painter::PLAIN, None);
        assert!(
            text.contains("line 4") && !text.contains("line 5"),
            "{text}"
        );
        assert!(text.ends_with("    …"), "{text}");
    }

    #[test]
    fn a_wide_line_is_cut_to_the_terminal_width_with_an_ellipsis() {
        let text = render_hits(&[hit(&"x".repeat(200), 1.0)], Painter::PLAIN, Some(40));
        let line = text.lines().last().unwrap();
        assert_eq!(line.chars().count(), 40, "{line}");
        assert!(line.ends_with('…'), "{line}");
    }

    #[test]
    fn blank_lines_do_not_spend_the_excerpt() {
        let text = render_hits(&[hit("a\n\n\n\nb", 1.0)], Painter::PLAIN, None);
        assert!(text.ends_with("    a\n    b"), "{text}");
    }

    #[test]
    fn a_graph_hit_names_the_entities_that_led_to_it() {
        let mut found = hit("body", 0.5);
        found.via = vec!["Alice".to_string(), "Acme".to_string()];
        let text = render_hits(&[found], Painter::PLAIN, None);
        assert!(text.contains("via Alice, Acme"), "{text}");
    }

    #[test]
    fn a_hit_shows_where_it_was_mined_from() {
        let mut found = hit("body", 0.5);
        found.drawer.source.uri = Some("docs/guide.md".to_string());
        found.drawer.tags = vec!["docs".to_string()];
        let text = render_hits(&[found], Painter::PLAIN, None);
        assert!(text.contains("docs/guide.md  [docs]"), "{text}");
    }

    #[test]
    fn an_empty_wake_up_context_says_what_is_missing() {
        let context = WakeUpContext {
            diary: None,
            recent_highlights: Vec::new(),
            generated_at: chrono::Utc::now(),
        };
        let text = render_wake_up(&context, Painter::PLAIN, None);
        assert!(text.contains("Last diary entry\nNone."), "{text}");
        assert!(text.contains("Recent highlights\nNone."), "{text}");
    }

    #[test]
    fn a_wake_up_context_lists_the_diary_entry_then_the_highlights() {
        let context = WakeUpContext {
            diary: Some(drawer("yesterday I fixed it")),
            recent_highlights: vec![drawer("decided to use SurrealDB")],
            generated_at: chrono::Utc::now(),
        };
        let text = render_wake_up(&context, Painter::PLAIN, None);
        let diary = text.find("yesterday I fixed it").expect("diary shown");
        let highlight = text
            .find("decided to use SurrealDB")
            .expect("highlight shown");
        assert!(diary < highlight, "{text}");
    }

    #[test]
    fn diary_entries_are_shown_whole() {
        let entries = [drawer("one\ntwo\nthree\nfour\nfive\nsix")];
        let text = render_diary(&entries, Painter::PLAIN);
        assert!(text.ends_with("one\ntwo\nthree\nfour\nfive\nsix"), "{text}");
        assert_eq!(render_diary(&[], Painter::PLAIN), "No diary entries.");
    }

    #[test]
    fn coloured_hits_are_the_plain_hits_with_escapes_stripped() {
        let hits = [hit("body", 0.5)];
        let coloured = render_hits(&hits, Painter::forced(), None);
        assert!(coloured.contains('\u{1b}'));
        assert_eq!(
            console::strip_ansi_codes(&coloured),
            render_hits(&hits, Painter::PLAIN, None)
        );
    }
}
