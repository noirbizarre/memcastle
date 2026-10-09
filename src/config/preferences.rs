//! Partial preference layers keep absence distinct from explicitly choosing `normal` or `[]`.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::{ConnectorPreference, PreferenceCriterion, PreferenceLevel, SourcePreferences};
use crate::error::{Error, Result};

/// Only fields present in a layer replace the settings beneath them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PreferenceLayer {
    /// Replacement global fallback, when supplied.
    pub default: Option<PreferenceLevel>,
    /// Sparse connector overrides.
    pub sources: BTreeMap<String, ConnectorLayer>,
}

/// Sparse settings for one connector.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorLayer {
    /// Explicitly set the connector level, even to `normal`.
    pub level: Option<PreferenceLevel>,
    /// Replaces inherited rules when supplied; an empty array clears them.
    pub criteria: Option<Vec<PreferenceCriterion>>,
}

impl PreferenceLayer {
    /// Apply only fields present in this layer.
    pub fn apply(&self, policy: &mut SourcePreferences) {
        if let Some(level) = self.default {
            policy.default = level;
        }
        for (name, layer) in &self.sources {
            let entry: &mut ConnectorPreference = policy.sources.entry(name.clone()).or_default();
            if let Some(level) = layer.level {
                entry.level = Some(level);
            }
            if let Some(criteria) = &layer.criteria {
                entry.criteria.clone_from(criteria);
            }
        }
    }

    /// Reject malformed criteria before a daemon serves a policy that silently cannot match.
    pub fn validate(&self) -> Result<()> {
        for (source, layer) in &self.sources {
            if source.trim().is_empty() {
                return Err(Error::config(
                    "preferences.sources: connector name cannot be empty",
                ));
            }
            let mut seen = HashSet::new();
            for rule in layer.criteria.iter().flatten() {
                if rule.path.split('.').any(|key| {
                    key.is_empty()
                        || !key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                }) {
                    return Err(Error::config(format!(
                        "preferences.sources.{source}: invalid metadata path `{}`; use dot-separated keys",
                        rule.path
                    )));
                }
                if !matches!(
                    rule.equals,
                    serde_json::Value::String(_)
                        | serde_json::Value::Bool(_)
                        | serde_json::Value::Number(_)
                ) {
                    return Err(Error::config(format!(
                        "preferences.sources.{source}: `{}` must compare a string, boolean or number",
                        rule.path
                    )));
                }
                if !seen.insert((rule.path.clone(), rule.equals.to_string())) {
                    return Err(Error::config(format!(
                        "preferences.sources.{source}: duplicate criterion for `{}` and {}",
                        rule.path, rule.equals
                    )));
                }
            }
        }
        Ok(())
    }
}

/// An optional palace-local preferences-only document.
pub fn read_local(path: &Path) -> Result<Option<PreferenceLayer>> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Local {
        preferences: PreferenceLayer,
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::io(path.display().to_string(), source)),
    };
    let local: Local = toml::from_str(&text)
        .map_err(|error| Error::config(format!("{}: {error}", path.display())))?;
    local.preferences.validate()?;
    Ok(Some(local.preferences))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palace_layers_inherit_unspecified_connector_fields_and_can_clear_criteria() {
        let global: PreferenceLayer = toml::from_str("default = 'low'\n[sources.pi]\nlevel = 'high'\n[[sources.pi.criteria]]\npath = 'kind'\nequals = 'decision'\nlevel = 'high'").unwrap();
        let palace: PreferenceLayer = toml::from_str("[sources.pi]\nlevel = 'normal'").unwrap();
        let mut policy = SourcePreferences::default();
        global.validate().unwrap();
        global.apply(&mut policy);
        palace.apply(&mut policy);
        assert_eq!(policy.default, PreferenceLevel::Low);
        assert_eq!(policy.sources["pi"].level, Some(PreferenceLevel::Normal));
        assert_eq!(policy.sources["pi"].criteria.len(), 1);
        let clear: PreferenceLayer = toml::from_str("[sources.pi]\ncriteria = []").unwrap();
        clear.apply(&mut policy);
        assert!(policy.sources["pi"].criteria.is_empty());
    }

    #[test]
    fn malformed_paths_and_duplicate_rules_are_refused() {
        let layer: PreferenceLayer = toml::from_str(
            "[[sources.pi.criteria]]\npath = 'session..kind'\nequals = 'x'\nlevel = 'low'",
        )
        .unwrap();
        assert!(layer.validate().is_err());
    }

    #[test]
    fn boolean_and_numeric_metadata_criteria_parse_without_string_coercion() {
        let layer: PreferenceLayer = toml::from_str("[[sources.example.criteria]]\npath = 'approved'\nequals = true\nlevel = 'high'\n[[sources.example.criteria]]\npath = 'revision'\nequals = 2\nlevel = 'low'").unwrap();
        layer.validate().unwrap();
        let rules = layer.sources["example"].criteria.as_ref().unwrap();
        assert_eq!(rules[0].equals, serde_json::json!(true));
        assert_eq!(rules[1].equals, serde_json::json!(2));
    }

    #[test]
    fn a_local_file_overrides_only_its_named_settings() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("preferences.toml");
        std::fs::write(&file, "[preferences.sources.pi]\nlevel = 'low'").unwrap();
        let layer = read_local(&file).unwrap().unwrap();
        let mut policy = SourcePreferences {
            default: PreferenceLevel::High,
            ..Default::default()
        };
        layer.apply(&mut policy);
        assert_eq!(policy.default, PreferenceLevel::High);
        assert_eq!(policy.sources["pi"].level, Some(PreferenceLevel::Low));
    }

    #[test]
    fn selected_palace_file_overrides_global_and_inline_palace_without_erasing_other_sources() {
        let dir = tempfile::tempdir().unwrap();
        let palace = dir.path().join("palace");
        std::fs::create_dir(&palace).unwrap();
        std::fs::write(
            palace.join("preferences.toml"),
            "[preferences.sources.pi]\nlevel = 'normal'",
        )
        .unwrap();
        let config_file = dir.path().join("config.toml");
        std::fs::write(&config_file, format!(
            "[palace]\npath = '{}'\n[preferences]\ndefault = 'low'\n[preferences.sources.pi]\nlevel = 'high'\n[preferences.sources.opencode]\nlevel = 'high'\n[palace.preferences.sources.pi]\nlevel = 'low'",
            palace.display(),
        )).unwrap();
        let config =
            super::super::Config::load(Some(&config_file), &super::super::Overrides::default())
                .unwrap();
        let policy = config.effective_preferences();
        assert_eq!(policy.default, PreferenceLevel::Low);
        assert_eq!(policy.sources["pi"].level, Some(PreferenceLevel::Normal));
        assert_eq!(
            policy.sources["opencode"].level,
            Some(PreferenceLevel::High)
        );
    }
}
