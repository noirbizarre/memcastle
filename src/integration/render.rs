//! What `memcastle integration` prints: the listing, and the account of what an operation changed.
//!
//! Plain padded text rather than a table widget: the output is a handful of short rows, and it must read the same in a
//! log file as in a terminal. `--json` serves the same data to a script.

use serde::Serialize;

use crate::error::Result;
use crate::term::Painter;

use super::catalog::{Broken, Catalog};
use super::install::{self, Action, ChangeKind, Context, Outcome, State, Status};

/// Everything `integration list` knows.
#[derive(Debug, Clone, Serialize)]
pub struct ListReport {
    /// The assets root the integrations were found under.
    pub assets_root: String,
    /// How that root was chosen: `override`, `installed`.
    pub assets_source: String,
    /// Where installed copies live.
    pub install_dir: String,
    /// Each integration shipped.
    pub integrations: Vec<Status>,
    /// Directories whose manifest was refused.
    pub broken: Vec<Broken>,
}

/// Look at every integration in `catalog`.
///
/// # Errors
///
/// [`crate::Error::Io`] when an installed copy's directory cannot be read.
pub fn list(catalog: &Catalog, ctx: &Context<'_>) -> Result<ListReport> {
    let integrations = catalog
        .integrations()
        .iter()
        .map(|shipped| install::inspect(shipped, catalog, ctx))
        .collect::<Result<Vec<_>>>()?;
    Ok(ListReport {
        assets_root: catalog.root().display().to_string(),
        assets_source: match catalog.source() {
            crate::assets::AssetSource::Override(_) => "override",
            crate::assets::AssetSource::Installed(_) => "installed",
            crate::assets::AssetSource::Embedded => "embedded",
        }
        .to_string(),
        install_dir: ctx.locations.agents_dir.display().to_string(),
        integrations,
        broken: catalog.broken().to_vec(),
    })
}

fn paint_state(state: State, painter: Painter, text: &str) -> String {
    match state {
        State::Installed => painter.ok(text),
        State::NotInstalled => painter.dim(text),
        State::Outdated | State::Modified => painter.warn(text),
        State::Incompatible | State::Unavailable => painter.error(text),
    }
}

/// Pad `text` to `width` columns by its visible length, so colour codes added afterwards do not skew the columns.
fn pad(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

/// The listing as text.
#[must_use]
pub fn render_list(report: &ListReport, painter: Painter) -> String {
    let mut lines = Vec::new();
    if report.integrations.is_empty() {
        lines.push(painter.dim("No integrations are shipped in these assets."));
    } else {
        let width = |f: fn(&Status) -> usize| report.integrations.iter().map(f).max().unwrap_or(0);
        let id_w = width(|s| s.id.len()).max("INTEGRATION".len());
        let agent_w = width(|s| s.agent.program().len()).max("AGENT".len());
        let ship_w = width(|s| s.shipped_version.len()).max("SHIPPED".len());
        let inst_w =
            width(|s| s.installed_version.as_deref().unwrap_or("-").len()).max("INSTALLED".len());
        let state_w = width(|s| s.state.to_string().len()).max("STATE".len());
        lines.push(painter.heading(&format!(
            "{}  {}  {}  {}  {}  AGENT VERSION",
            pad("INTEGRATION", id_w),
            pad("AGENT", agent_w),
            pad("SHIPPED", ship_w),
            pad("INSTALLED", inst_w),
            pad("STATE", state_w),
        )));
        for status in &report.integrations {
            let state = pad(&status.state.to_string(), state_w);
            lines.push(format!(
                "{}  {}  {}  {}  {}  {}",
                pad(&status.id, id_w),
                pad(status.agent.program(), agent_w),
                pad(&status.shipped_version, ship_w),
                pad(status.installed_version.as_deref().unwrap_or("-"), inst_w),
                paint_state(status.state, painter, &state),
                status.agent_version.as_deref().unwrap_or("not found"),
            ));
        }
        for status in report
            .integrations
            .iter()
            .filter(|s| !s.problems.is_empty())
        {
            lines.push(String::new());
            lines.push(format!("{}:", painter.heading(&status.id)));
            for problem in &status.problems {
                lines.push(format!("  {}", painter.warn(problem)));
            }
        }
    }
    for broken in &report.broken {
        lines.push(painter.error(&format!("{}: {}", broken.id, broken.message)));
    }
    lines.push(String::new());
    lines.push(painter.dim(&format!(
        "assets: {} ({}); installed copies: {}",
        report.assets_root, report.assets_source, report.install_dir
    )));
    lines.join("\n")
}

/// What an install, update or remove did, as text.
#[must_use]
pub fn render_outcome(outcome: &Outcome, painter: Painter) -> String {
    let version = outcome.version.as_deref().unwrap_or("");
    let headline = match outcome.action {
        Action::Installed => painter.ok(&format!("Installed {} {version}", outcome.id)),
        Action::Updated => painter.ok(&format!("Updated {} to {version}", outcome.id)),
        Action::Unchanged => painter.dim(&format!(
            "{} {version} is already installed and up to date; nothing changed",
            outcome.id
        )),
        Action::Removed => painter.ok(&format!("Removed {} {version}", outcome.id)),
        Action::AlreadyAbsent => {
            painter.dim(&format!("{} is not installed; nothing changed", outcome.id))
        }
    };
    let mut lines = vec![headline];
    for change in &outcome.changes {
        let label = match change.kind {
            ChangeKind::Copied => painter.ok("copied      "),
            ChangeKind::Replaced => painter.ok("replaced    "),
            ChangeKind::Registered => painter.ok("registered  "),
            ChangeKind::Unregistered => painter.ok("unregistered"),
            ChangeKind::Deleted => painter.ok("deleted     "),
            ChangeKind::LeftAlone => painter.warn("left alone  "),
        };
        lines.push(format!("  {label} {}", change.target));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::manifest::AgentKind;

    fn status(id: &str, state: State, installed: Option<&str>) -> Status {
        Status {
            id: id.to_string(),
            description: "d".to_string(),
            agent: AgentKind::Pi,
            shipped_version: "0.1.0".to_string(),
            installed_version: installed.map(str::to_string),
            state,
            agent_version: Some("1.0.1".to_string()),
            memcastle_requirement: ">=0.2".to_string(),
            agent_requirement: None,
            problems: Vec::new(),
        }
    }

    fn report(integrations: Vec<Status>) -> ListReport {
        ListReport {
            assets_root: "/usr/share/memcastle".to_string(),
            assets_source: "installed".to_string(),
            install_dir: "/home/u/.local/share/memcastle/agents".to_string(),
            integrations,
            broken: Vec::new(),
        }
    }

    #[test]
    fn the_listing_shows_each_integration_its_versions_and_where_the_assets_came_from() {
        let text = render_list(
            &report(vec![
                status("pi", State::Installed, Some("0.1.0")),
                status("opencode", State::NotInstalled, None),
            ]),
            Painter::PLAIN,
        );

        assert!(text.contains("INTEGRATION"), "{text}");
        assert!(
            text.contains("opencode") && text.contains("not installed"),
            "{text}"
        );
        assert!(text.contains("/usr/share/memcastle (installed)"), "{text}");
    }

    #[test]
    fn a_problem_is_listed_under_the_integration_it_belongs_to() {
        let mut modified = status("pi", State::Modified, Some("0.1.0"));
        modified
            .problems
            .push("dist/index.js was changed after installation".to_string());

        let text = render_list(&report(vec![modified]), Painter::PLAIN);

        assert!(
            text.contains("pi:\n  dist/index.js was changed after installation"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_listing_says_so_instead_of_printing_a_bare_header() {
        let text = render_list(&report(Vec::new()), Painter::PLAIN);

        assert!(text.contains("No integrations are shipped"), "{text}");
        assert!(!text.contains("INTEGRATION"), "{text}");
    }

    #[test]
    fn an_unchanged_outcome_says_nothing_changed_and_lists_no_changes() {
        let outcome = Outcome {
            id: "pi".to_string(),
            action: Action::Unchanged,
            version: Some("0.1.0".to_string()),
            directory: "/d".to_string(),
            changes: Vec::new(),
        };

        let text = render_outcome(&outcome, Painter::PLAIN);

        assert_eq!(
            text,
            "pi 0.1.0 is already installed and up to date; nothing changed"
        );
    }
}
