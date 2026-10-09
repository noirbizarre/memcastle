//! A miner: a named, persistent definition of *what to mine and how*, kept as `[[miners]]` in `memcastle.toml`.
//!
//! Pure types and rules, no I/O. A miner is configuration, not mined data: the cursor and the documents stay on the
//! source (`crate::domain::SourceRef`), which the adapter derives from the miner's `source` and `locator`.
//! That is what lets a miner be renamed, disabled or re-scoped without losing where its
//! source stopped. See `docs/adr/037-persistent-miner-configuration.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::CredentialRef;

/// The longest miner name, so one fits a table column and a URL segment.
pub const MAX_MINER_NAME_LEN: usize = 64;

/// What a miner's own settings may not be named: a secret belongs behind a [`CredentialRef`], so a key that reads
/// like one is refused rather than written to a world-readable file.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "credential",
];

/// Names a miner cannot have: the API gives them a meaning of their own (`POST /api/miners/reload`).
const RESERVED_NAMES: &[&str] = &["reload"];

/// Whether `name` is a well-formed miner name: lowercase letters, digits, `-` and `_`, starting with a letter or a
/// digit, so it is safe as a URL segment, a CLI argument and a job's `requested_by`.
#[must_use]
pub fn is_valid_miner_name(name: &str) -> bool {
    let mut chars = name.chars();
    !RESERVED_NAMES.contains(&name)
        && name.len() <= MAX_MINER_NAME_LEN
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// One `[[miners]]` entry.
///
/// Unknown top-level keys are an error (a typo in `enable` must not silently leave a miner enabled);
/// `options` is open, so a source can define its own keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinerDefinition {
    /// The miner's name: its identity in the configuration, in the CLI and in the API.
    pub name: String,
    /// The source adapter this miner runs (`directory`, or an installed source's name).
    pub source: String,
    /// Whether the miner may run.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// The part of the source to read, as the adapter understands it (a path, a channel, a repository).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    /// The wing the mined drawers go to, when it should not be the adapter's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wing: Option<String>,
    /// Where the source's credential is read from; never the credential itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialRef>,
    /// Saved source options; the source declares which changes can select more material.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub options: Map<String, Value>,
}

fn default_enabled() -> bool {
    true
}

impl MinerDefinition {
    /// What a run of this miner passes to its source as options: the saved values flattened to the
    /// strings every client sends (`memcastle mine <source> key=value`), so a miner run and the same run by hand are the
    /// same request.
    ///
    /// A scalar is its text, a list of strings is comma-joined; nested tables have no one-string form.
    ///
    /// # Errors
    ///
    /// A sentence naming the key and what to change.
    pub fn options(&self) -> Result<super::Options, String> {
        let mut options = super::Options::new();
        for (key, value) in &self.options {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Bool(flag) => flag.to_string(),
                Value::Number(number) => number.to_string(),
                Value::Array(items) if items.iter().all(Value::is_string) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
                Value::Null | Value::Array(_) | Value::Object(_) => {
                    return Err(format!(
                        "`options.{key}` is not a string, number, boolean or list of strings, so a run cannot pass it to the source"
                    ));
                }
            };
            options.insert(key.clone(), text);
        }
        Ok(options)
    }

    /// Check what can be checked without knowing the source: names, shapes and the rules that keep a secret out of
    /// the file. What needs the source (is it installed, does its locator make sense) is checked at activation.
    ///
    /// # Errors
    ///
    /// Returns what is wrong, as a sentence that names the field and the fix.
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_miner_name(&self.name) {
            return Err(format!(
                "`{}` is not a valid miner name; use 1-{MAX_MINER_NAME_LEN} lowercase letters, digits, `-` or `_`, \
                 starting with a letter or digit (`reload` is reserved)",
                self.name
            ));
        }
        if self.source.trim().is_empty() {
            return Err("`source` is empty; name the source adapter this miner runs".to_string());
        }
        for (field, value) in [("locator", &self.locator), ("wing", &self.wing)] {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                return Err(format!("`{field}` is empty; remove it or give it a value"));
            }
        }
        match &self.credential {
            Some(CredentialRef::Env { name }) if name.trim().is_empty() => {
                return Err("`credential.name` is empty; name the environment variable".to_string());
            }
            Some(CredentialRef::File { path }) if path.trim().is_empty() => {
                return Err("`credential.path` is empty; give the file's path".to_string());
            }
            _ => {}
        }
        for (key, value) in &self.options {
            if !scope_value_is_valid(value) {
                return Err(format!(
                    "`options.{key}` must be a string, a number, a boolean or a list of strings"
                ));
            }
        }
        check_open_table("options", &self.options)?;
        Ok(())
    }
}

/// Validate every miner and that no two share a name.
///
/// # Errors
///
/// Returns the first problem, naming the miner it is about.
pub fn validate_miners(miners: &[MinerDefinition]) -> Result<(), String> {
    for (index, miner) in miners.iter().enumerate() {
        miner
            .validate()
            .map_err(|reason| format!("miner `{}`: {reason}", miner.name))?;
        if miners[..index]
            .iter()
            .any(|earlier| earlier.name == miner.name)
        {
            return Err(format!(
                "miner `{}` is defined twice; names are unique, so rename or remove one",
                miner.name
            ));
        }
    }
    Ok(())
}

/// An option value is a scalar or a list of strings: every run option has a string representation.
fn scope_value_is_valid(value: &Value) -> bool {
    match value {
        Value::String(_) | Value::Bool(_) | Value::Number(_) => true,
        Value::Array(items) => items.iter().all(Value::is_string),
        Value::Null | Value::Object(_) => false,
    }
}

/// The options table may hold anything TOML can, except a key that reads like a
/// secret, and never a null (TOML has none, so it could not be written back).
fn check_open_table(table: &str, map: &Map<String, Value>) -> Result<(), String> {
    for (key, value) in map {
        let lower = key.to_ascii_lowercase();
        if SECRET_WORDS.iter().any(|word| lower.contains(word)) {
            return Err(format!(
                "`{table}.{key}` looks like a secret, which must not be written to the configuration file; \
                 put the secret in an environment variable or a file and point `credential` at it"
            ));
        }
        match value {
            Value::Null => {
                return Err(format!(
                    "`{table}.{key}` is null; TOML has no null, remove the key"
                ));
            }
            Value::Object(inner) => check_open_table(&format!("{table}.{key}"), inner)?,
            _ => {}
        }
    }
    Ok(())
}

/// Explain every change not proven to narrow what a source reads. Unknown semantics require explicit approval.
#[must_use]
pub fn option_broadening(
    old: &super::Options,
    new: &super::Options,
    specs: &[super::OptionSpec],
) -> Vec<String> {
    use super::OptionBreadth;
    let mut reasons = Vec::new();
    // One key may occur in both maps; compare it only once rather than deduplicating diagnostic text.
    for key in old
        .keys()
        .chain(new.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if old.get(key) == new.get(key) {
            continue;
        }
        let before = old.get(key).map(String::as_str);
        let after = new.get(key).map(String::as_str);
        let breadth = specs
            .iter()
            .find(|spec| spec.name == *key)
            .map_or(OptionBreadth::Unknown, |spec| spec.breadth);
        let safe = match (breadth, before, after) {
            (OptionBreadth::Include, _, Some(next)) => {
                before.is_none_or(|prior| parts(next).is_subset(&parts(prior)))
            }
            (OptionBreadth::OptInInclude, Some(prior), Some(next)) => {
                parts(next).is_subset(&parts(prior))
            }
            (OptionBreadth::OptInInclude, Some(_), None) => true,
            (OptionBreadth::Exclude, Some(prior), Some(next)) => {
                parts(prior).is_subset(&parts(next))
            }
            (OptionBreadth::Exclude, None, Some(_)) => true,
            (OptionBreadth::Since, _, Some(next)) => super::parse_since(next).is_ok_and(|later| {
                before.is_none_or(|prior| {
                    super::parse_since(prior).is_ok_and(|earlier| later >= earlier)
                })
            }),
            _ => false,
        };
        if !safe {
            reasons.push(format!(
                "`{key}` changed from {} to {}; this may mine more material",
                before.unwrap_or("<unset>"),
                after.unwrap_or("<unset>")
            ));
        }
    }
    reasons
}

fn parts(value: &str) -> std::collections::BTreeSet<&str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn map(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().expect("an object")
    }

    fn miner(toml_text: &str) -> MinerDefinition {
        toml::from_str(toml_text).expect("a miner")
    }

    #[test]
    fn a_minimal_miner_is_enabled_manual_and_unscoped() {
        let m = miner("name = \"docs\"\nsource = \"directory\"\n");
        assert!(m.enabled);
        assert!(m.options.is_empty());
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn the_documented_example_parses_with_options_and_credential() {
        let m = miner(
            r#"
name = "signal-personal"
source = "signal"
enabled = true

[credential]
type = "env"
name = "SIGNAL_TOKEN"

[options]
contacts = ["+336"]
groups = ["MemCastle"]
"#,
        );
        assert_eq!(m.options["contacts"], json!(["+336"]));
        assert_eq!(
            m.credential,
            Some(CredentialRef::Env {
                name: "SIGNAL_TOKEN".to_string()
            })
        );
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn the_removed_miner_trigger_table_is_refused_and_named() {
        let error = toml::from_str::<MinerDefinition>(
            "name = \"a\"\nsource = \"b\"\n[trigger]\ntype = \"schedule\"\n",
        )
        .expect_err("triggers are their own entries now")
        .to_string();
        assert!(error.contains("trigger"), "{error}");
    }

    #[test]
    fn a_misspelt_key_is_refused_rather_than_ignored() {
        let result =
            toml::from_str::<MinerDefinition>("name = \"a\"\nsource = \"b\"\nenable = false\n");
        assert!(result.is_err(), "`enable` must not leave the miner enabled");
    }

    #[test]
    fn a_miner_round_trips_through_toml() {
        let m = miner(
            "name = \"a\"\nsource = \"b\"\nlocator = \"/x\"\n[options]\nk = [\"v\"]\nn = 3\n",
        );
        let text = toml::to_string(&m).expect("serialises");
        assert_eq!(toml::from_str::<MinerDefinition>(&text).expect("parses"), m);
    }

    #[test]
    fn names_are_checked_for_shape() {
        for good in ["a", "signal-personal", "x_1", "0day"] {
            assert!(is_valid_miner_name(good), "{good}");
        }
        for bad in [
            "",
            "-a",
            "_a",
            "A",
            "a b",
            "a/b",
            "é",
            "reload",
            &"a".repeat(65),
        ] {
            assert!(!is_valid_miner_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_secret_looking_key_is_refused_in_every_open_table() {
        let text = "name = \"a\"\nsource = \"b\"\n[options]\napi_key = \"x\"\n";
        let error = miner(text).validate().expect_err("a secret key");
        assert!(error.contains("looks like a secret"), "{error}");
        assert!(error.contains("credential"), "must say what to do: {error}");
        let nested = miner("name = \"a\"\nsource = \"b\"\n[options.auth]\nPassword = \"x\"\n");
        assert!(nested.validate().is_err(), "a nested key counts too");
    }

    #[test]
    fn an_option_value_must_be_a_scalar_or_a_list_of_strings() {
        let mut m = miner("name = \"a\"\nsource = \"b\"\n");
        for bad in [json!({"a": 1}), json!([1, 2]), json!(null)] {
            m.options = map(&json!({"k": bad}));
            assert!(m.validate().is_err(), "{bad}");
        }
        m.options = map(&json!({"s": "x", "n": 3, "b": true, "l": ["a", "b"]}));
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn an_oauth_credential_is_a_reference_with_nothing_to_name_and_nothing_secret_to_keep() {
        let m = miner("name = \"a\"\nsource = \"b\"\ncredential = { type = \"oauth\" }\n");
        assert_eq!(m.credential, Some(CredentialRef::Oauth));
        assert_eq!(m.validate(), Ok(()));
        assert_eq!(
            serde_json::to_value(&m.credential).unwrap(),
            json!({"type": "oauth"})
        );
    }

    #[test]
    fn empty_required_fields_name_the_field() {
        let mut m = miner("name = \"a\"\nsource = \"b\"\n");
        m.source = " ".to_string();
        assert!(m.validate().expect_err("empty source").contains("`source`"));
        m.source = "b".to_string();
        m.locator = Some(String::new());
        assert!(
            m.validate()
                .expect_err("empty locator")
                .contains("`locator`")
        );
        m.locator = None;
        m.credential = Some(CredentialRef::Env {
            name: String::new(),
        });
        assert!(
            m.validate()
                .expect_err("empty env")
                .contains("credential.name")
        );
    }

    #[test]
    fn changing_positive_negative_and_date_filters_uses_the_sources_declared_semantics() {
        let old = super::super::Options::from([
            ("include".into(), "a,b".into()),
            ("exclude".into(), "private".into()),
            ("since".into(), "2026-01".into()),
            ("mode".into(), "web".into()),
        ]);
        let specs = [
            super::super::OptionSpec::new("include", "", super::super::OptionKind::String)
                .with_breadth(super::super::OptionBreadth::Include),
            super::super::OptionSpec::new("exclude", "", super::super::OptionKind::String)
                .with_breadth(super::super::OptionBreadth::Exclude),
            super::super::OptionSpec::new("since", "", super::super::OptionKind::Date)
                .with_breadth(super::super::OptionBreadth::Since),
        ];
        let mut narrowed = old.clone();
        narrowed.insert("include".into(), "a".into());
        narrowed.insert("exclude".into(), "private,internal".into());
        narrowed.insert("since".into(), "2026-02".into());
        assert!(option_broadening(&old, &narrowed, &specs).is_empty());
        let mut wider = old.clone();
        wider.insert("include".into(), "a,b,c".into());
        wider.remove("exclude");
        wider.insert("since".into(), "2025-01".into());
        wider.insert("mode".into(), "export".into());
        assert_eq!(option_broadening(&old, &wider, &specs).len(), 4);
        assert_eq!(option_broadening(&old, &old, &[]).len(), 0);
        assert_eq!(option_broadening(&old, &narrowed, &[]).len(), 3);
    }

    #[test]
    fn an_opt_in_selection_is_broader_when_first_enabled() {
        let specs =
            [
                super::super::OptionSpec::new("wiki_include", "", super::super::OptionKind::String)
                    .with_breadth(super::super::OptionBreadth::OptInInclude),
            ];
        let none = super::super::Options::new();
        let one = super::super::Options::from([("wiki_include".into(), "acme/docs".into())]);
        assert_eq!(option_broadening(&none, &one, &specs).len(), 1);
        assert!(option_broadening(&one, &none, &specs).is_empty());
    }
}
