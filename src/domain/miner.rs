//! A miner: a named, persistent definition of *what to mine and how*, kept as `[[miners]]` in `memcastle.toml`.
//!
//! Pure types and rules, no I/O. A miner is configuration, not mined data: the cursor and the documents stay on the
//! source (`crate::domain::SourceRef`), which the adapter derives from the miner's `source` and `locator`.
//! That is what lets a miner be renamed, disabled, re-scoped or given another trigger without losing where its
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

/// What starts a miner.
///
/// Only [`TriggerKind::Manual`] is acted on today (`memcastle miner run`); the others are stored and validated so
/// a definition written now keeps its meaning when triggers arrive (#189), and the daemon says it does not act on
/// them yet instead of pretending to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// Only when asked.
    #[default]
    Manual,
    /// When the source announces something new.
    Event,
    /// On a timetable.
    Schedule,
}

/// A miner's trigger: its kind, and whatever else the kind needs, kept verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinerTrigger {
    /// What starts the miner.
    #[serde(rename = "type", default)]
    pub kind: TriggerKind,
    /// The kind's own settings (an interval, a webhook name), opaque here.
    #[serde(flatten)]
    pub settings: Map<String, Value>,
}

impl MinerTrigger {
    /// Whether this is the default, which is not written to the file.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.kind == TriggerKind::Manual && self.settings.is_empty()
    }
}

/// One `[[miners]]` entry.
///
/// Unknown top-level keys are an error (a typo in `enable` must not silently leave a miner enabled);
/// `scope`, `config` and the trigger's settings are open, so a source can define its own keys.
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
    /// What to include: a filter the source understands. Empty means everything the locator reaches.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub scope: Map<String, Value>,
    /// What starts the miner.
    #[serde(default, skip_serializing_if = "MinerTrigger::is_default")]
    pub trigger: MinerTrigger,
    /// Source-specific settings that are not a filter.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub config: Map<String, Value>,
}

fn default_enabled() -> bool {
    true
}

impl MinerDefinition {
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
        for (key, value) in &self.scope {
            if !scope_value_is_valid(value) {
                return Err(format!(
                    "`scope.{key}` must be a string, a number, a boolean or a list of strings"
                ));
            }
        }
        for (table, map) in [
            ("scope", &self.scope),
            ("config", &self.config),
            ("trigger", &self.trigger.settings),
        ] {
            check_open_table(table, map)?;
        }
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

/// A scope value is a scalar, or a list of strings: the shapes a filter can be compared and narrowed on.
fn scope_value_is_valid(value: &Value) -> bool {
    match value {
        Value::String(_) | Value::Bool(_) | Value::Number(_) => true,
        Value::Array(items) => items.iter().all(Value::is_string),
        Value::Null | Value::Object(_) => false,
    }
}

/// An open table (`scope`, `config`, trigger settings) may hold anything TOML can, except a key that reads like a
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

/// Why a new scope is wider than the old one, one sentence per reason; empty when it is the same or narrower.
///
/// A scope is a filter, so wider means more reaches the palace: a filter removed, a value added to a list, or a value
/// changed to something that cannot be shown to be inside the old one. A *new* filter, or a value removed from a
/// list, only narrows.
#[must_use]
pub fn scope_broadening(old: &Map<String, Value>, new: &Map<String, Value>) -> Vec<String> {
    let mut reasons = Vec::new();
    for (key, before) in old {
        let Some(after) = new.get(key) else {
            reasons.push(format!("`{key}` is no longer filtered"));
            continue;
        };
        match (before, after) {
            (Value::Array(before), Value::Array(after)) => {
                for item in after.iter().filter(|item| !before.contains(item)) {
                    reasons.push(format!("`{key}` now also includes {item}"));
                }
            }
            (before, after) if before != after => {
                reasons.push(format!("`{key}` changed from {before} to {after}"));
            }
            _ => {}
        }
    }
    reasons
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
        assert_eq!(m.trigger.kind, TriggerKind::Manual);
        assert!(m.scope.is_empty() && m.config.is_empty());
        assert_eq!(m.validate(), Ok(()));
    }

    #[test]
    fn the_documented_example_parses_with_scope_trigger_and_credential() {
        let m = miner(
            r#"
name = "signal-personal"
source = "signal"
enabled = true

[credential]
type = "env"
name = "SIGNAL_TOKEN"

[scope]
contacts = ["+336"]
groups = ["MemCastle"]

[trigger]
type = "event"
webhook = "signal"
"#,
        );
        assert_eq!(m.scope["contacts"], json!(["+336"]));
        assert_eq!(m.trigger.kind, TriggerKind::Event);
        assert_eq!(m.trigger.settings["webhook"], json!("signal"));
        assert_eq!(
            m.credential,
            Some(CredentialRef::Env {
                name: "SIGNAL_TOKEN".to_string()
            })
        );
        assert_eq!(m.validate(), Ok(()));
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
            "name = \"a\"\nsource = \"b\"\nlocator = \"/x\"\n[scope]\nk = [\"v\"]\n[trigger]\ntype = \"schedule\"\nevery = \"1d\"\n[config]\nn = 3\n",
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
        for table in ["scope", "config", "trigger"] {
            let text = format!("name = \"a\"\nsource = \"b\"\n[{table}]\napi_key = \"x\"\n");
            let error = miner(&text).validate().expect_err("a secret key");
            assert!(error.contains("looks like a secret"), "{table}: {error}");
            assert!(error.contains("credential"), "must say what to do: {error}");
        }
        let nested = miner("name = \"a\"\nsource = \"b\"\n[config.auth]\nPassword = \"x\"\n");
        assert!(nested.validate().is_err(), "a nested key counts too");
    }

    #[test]
    fn a_scope_value_must_be_a_scalar_or_a_list_of_strings() {
        let mut m = miner("name = \"a\"\nsource = \"b\"\n");
        for bad in [json!({"a": 1}), json!([1, 2]), json!(null)] {
            m.scope = map(&json!({"k": bad}));
            assert!(m.validate().is_err(), "{bad}");
        }
        m.scope = map(&json!({"s": "x", "n": 3, "b": true, "l": ["a", "b"]}));
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
    fn scope_broadening_is_judged_per_key() {
        let old = map(&json!({"groups": ["a", "b"], "contact": "alice"}));
        let cases: [(Value, usize, &str); 7] = [
            (
                json!({"groups": ["a", "b"], "contact": "alice"}),
                0,
                "identical",
            ),
            (
                json!({"groups": ["a"], "contact": "alice"}),
                0,
                "a value removed from a list",
            ),
            (
                json!({"groups": ["a", "b"], "contact": "alice", "since": "2024"}),
                0,
                "a filter added",
            ),
            (
                json!({"groups": ["a", "b", "c"], "contact": "alice"}),
                1,
                "a value added to a list",
            ),
            (json!({"groups": ["a", "b"]}), 1, "a filter dropped"),
            (
                json!({"groups": ["a", "b"], "contact": "bob"}),
                1,
                "a scalar changed",
            ),
            (json!({}), 2, "everything dropped"),
        ];
        for (new, expected, why) in cases {
            assert_eq!(scope_broadening(&old, &map(&new)).len(), expected, "{why}");
        }
        assert!(scope_broadening(&Map::new(), &map(&json!({"a": "b"}))).is_empty());
    }
}
