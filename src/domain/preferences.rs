//! Source authority is a bounded signal attached to evidence, never a statement of truth.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Origin;

/// Ordered, user-facing authority levels; scoring strategies can change independently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceLevel {
    /// Less authoritative evidence, still eligible for retrieval and extraction.
    Low,
    #[default]
    /// Neutral evidence.
    Normal,
    /// More authoritative evidence, not a guarantee of correctness.
    High,
}

impl PreferenceLevel {
    /// A small, mode-independent adjustment to an evidence rank (not an absolute weight).
    pub fn adjustment(self) -> f32 {
        match self {
            Self::Low => -0.08,
            Self::Normal => 0.0,
            Self::High => 0.08,
        }
    }
}

/// A connector-specific JSON scalar comparison; no connector has to expose the same keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferenceCriterion {
    /// Dot-separated metadata keys.
    pub path: String,
    /// A JSON scalar to match exactly.
    pub equals: serde_json::Value,
    /// Level when this rule matches.
    pub level: PreferenceLevel,
}

/// The resolved settings for one connector.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorPreference {
    /// Optional level, inherited from the global default when absent.
    pub level: Option<PreferenceLevel>,
    /// Connector-specific metadata rules in declaration order.
    pub criteria: Vec<PreferenceCriterion>,
}

/// Effective settings for the served palace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcePreferences {
    /// Level for sources without a matching connector or rule.
    pub default: PreferenceLevel,
    /// Preferences keyed by mining adapter name.
    pub sources: BTreeMap<String, ConnectorPreference>,
}

/// Explain which configured preference matched this evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreferenceMatch {
    /// The effective level.
    pub level: PreferenceLevel,
    /// Connector name, if the evidence came from a mining source.
    pub source: Option<String>,
    /// Metadata path when a more specific criterion matched.
    pub criterion: Option<String>,
}

impl SourcePreferences {
    /// Missing metadata and unknown connectors use the default, including for older drawers.
    pub fn resolve(&self, origin: Option<&Origin>) -> PreferenceMatch {
        let Some(origin) = origin else {
            return PreferenceMatch {
                level: self.default,
                source: None,
                criterion: None,
            };
        };
        let connector = self.sources.get(&origin.source);
        let mut resolved = PreferenceMatch {
            level: connector
                .and_then(|entry| entry.level)
                .unwrap_or(self.default),
            source: Some(origin.source.clone()),
            criterion: None,
        };
        if let Some(entry) = connector {
            // The longest matching path is the most specific; ties retain declaration order.
            let matched = entry
                .criteria
                .iter()
                .filter(|rule| {
                    origin.metadata.as_ref().and_then(|metadata| {
                        rule.path
                            .split('.')
                            .try_fold(metadata, |value, key| value.get(key))
                    }) == Some(&rule.equals)
                })
                .fold(None, |best: Option<&PreferenceCriterion>, rule| {
                    if best.is_none_or(|best| {
                        rule.path.split('.').count() > best.path.split('.').count()
                    }) {
                        Some(rule)
                    } else {
                        best
                    }
                });
            if let Some(rule) = matched {
                resolved.level = rule.level;
                resolved.criterion = Some(rule.path.clone());
            }
        }
        resolved
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::SourceId;

    #[test]
    fn a_criterion_overrides_a_connector_without_matching_unrelated_metadata() {
        let policy = SourcePreferences {
            default: PreferenceLevel::Normal,
            sources: BTreeMap::from([(
                "pi".into(),
                ConnectorPreference {
                    level: Some(PreferenceLevel::Low),
                    criteria: vec![PreferenceCriterion {
                        path: "session.kind".into(),
                        equals: json!("decision"),
                        level: PreferenceLevel::High,
                    }],
                },
            )]),
        };
        let mut origin = Origin {
            source_id: SourceId::new(),
            source: "pi".into(),
            document: "s".into(),
            chunk: 0,
            revision: "r1".into(),
            metadata: Some(json!({"session": {"kind": "decision"}})),
            occurred_at: None,
        };
        let selected = policy.resolve(Some(&origin));
        assert_eq!(selected.level, PreferenceLevel::High);
        assert_eq!(selected.criterion.as_deref(), Some("session.kind"));
        origin.metadata = Some(json!({"session": {"kind": "draft"}}));
        assert_eq!(policy.resolve(Some(&origin)).level, PreferenceLevel::Low);
        origin.source = "uninstalled".into();
        assert_eq!(policy.resolve(Some(&origin)).level, PreferenceLevel::Normal);
    }
}
