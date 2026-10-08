//! Readable explanations of graph assertions, with their decisions still visible without `--json`.

use crate::domain::Relationship;
use crate::term::Painter;

/// Render the assertions and the provenance of each lifecycle decision for a terminal.
#[must_use]
pub fn render_history(facts: &[Relationship], painter: Painter) -> String {
    facts
        .iter()
        .map(|fact| {
            let mut lines = vec![format!(
                "{} {} {} → {} ({})",
                fact.id,
                fact.from,
                fact.predicate,
                fact.to,
                fact.lifecycle
                    .as_ref()
                    .map_or("unknown".to_string(), |l| format!("{:?}", l.state)
                        .to_lowercase())
            )];
            if let Some(lifecycle) = &fact.lifecycle {
                for link in &lifecycle.links {
                    lines.push(format!(
                        "  {} {} → {}: {}",
                        painter.dim(link.kind.as_str()),
                        link.from,
                        link.to,
                        link.reason
                    ));
                }
                if let Some(preferred) = lifecycle.preferred {
                    lines.push(format!(
                        "  {} {}",
                        painter.dim("Preferred (unresolved):"),
                        preferred
                    ));
                }
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        EntityId, FactLifecycle, FactLink, FactLinkKind, FactLinkOrigin, FactState, RelationshipId,
    };
    use chrono::Utc;

    #[test]
    fn a_disputed_fact_shows_both_the_reason_and_that_preference_is_not_a_resolution() {
        let id = RelationshipId::new();
        let other = RelationshipId::new();
        let fact = Relationship {
            id,
            from: EntityId::new(),
            to: EntityId::new(),
            predicate: "located_in".into(),
            confidence: 0.8,
            valid_from: Utc::now(),
            valid_to: None,
            provenance: None,
            assertion: None,
            lifecycle: Some(FactLifecycle {
                state: FactState::Conflicting,
                links: vec![FactLink {
                    from: id,
                    to: other,
                    kind: FactLinkKind::Contradicts,
                    origin: FactLinkOrigin::Explicit,
                    reason: "two sources disagree".into(),
                    at: Utc::now(),
                    evidence: None,
                }],
                preferred: Some(id),
                basis: None,
            }),
        };
        let rendered = render_history(&[fact], Painter::PLAIN);
        assert!(rendered.contains("(conflicting)"));
        assert!(rendered.contains("contradicts"));
        assert!(rendered.contains("two sources disagree"));
        assert!(rendered.contains("Preferred (unresolved):"));
    }
}
