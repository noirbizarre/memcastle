//! A trigger: a named, persistent, **opt-in** rule for *when* to ask for a mining run, kept as `[[triggers]]` in
//! `memcastle.toml`.
//!
//! Pure types and rules, no I/O. A trigger never acquires anything and never reads a source: it ends in the same
//! request `memcastle miner run` makes, so the pipeline alone decides what is new (and an unchanged revision costs a
//! discovery pass, not a duplicate). What a source *can* be triggered by is its capability, declared in its manifest
//! and shown to the user; whether it *is* triggered is the user's, and starts `enabled = false`.
//! See `docs/adr/043-source-triggers.md`.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{CredentialRef, JobId};

/// The longest trigger name, so one fits a table column and a URL segment (`/hooks/<name>`).
pub const MAX_TRIGGER_NAME_LEN: usize = 64;

/// Names a trigger cannot have: the API gives them a meaning of their own (`POST /api/triggers/reload`).
const RESERVED_NAMES: &[&str] = &["reload"];

/// The shortest `every`: a burst is already coalesced, so this only keeps a typo (`1` for `1h`) from hammering a source.
const MIN_EVERY: Duration = Duration::from_secs(1);
/// The shortest and longest `debounce`: shorter than a file write's own burst is no debounce, longer than this delays a
/// change by more than anyone meant.
const MIN_DEBOUNCE: Duration = Duration::from_millis(100);
const MAX_DEBOUNCE: Duration = Duration::from_secs(3600);
/// The default `debounce` of a `watch` trigger: an editor's save is several events within a moment.
const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(2);

/// Whether `name` is a well-formed trigger name: the shape of a miner name, so it is safe as a URL segment, a CLI
/// argument and a job's `requested_by`.
#[must_use]
pub fn is_valid_trigger_name(name: &str) -> bool {
    !RESERVED_NAMES.contains(&name)
        && name.len() <= MAX_TRIGGER_NAME_LEN
        // Reserved names are a subset of what the miner rule accepts, so the shape rule is reused as is.
        && super::is_valid_miner_name(name)
}

/// How a trigger decides to ask for a run.
///
/// `manual` is not here: every miner is manually triggerable (`memcastle miner run`, `memcastle trigger fire`), which
/// is the same request, so there is nothing to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMechanism {
    /// On a timetable: every so often, optionally at a time of day.
    Schedule,
    /// Every so often, backing off while the source keeps failing.
    Poll,
    /// When an external service calls the daemon's webhook listener.
    Webhook,
    /// When something changes on the local file system.
    Watch,
}

impl TriggerMechanism {
    /// Every mechanism, in the order they are listed to people.
    pub const ALL: [Self; 4] = [Self::Schedule, Self::Poll, Self::Webhook, Self::Watch];

    /// The name this mechanism goes by in the file, the API and a source's manifest.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Poll => "poll",
            Self::Webhook => "webhook",
            Self::Watch => "watch",
        }
    }

    /// Whether the host provides this for every source.
    ///
    /// A timetable and a poll only mean "mine again", which every source supports. A webhook needs the source to say
    /// what a delivery means, and a watch needs it to say which files matter, so a source declares those.
    #[must_use]
    pub fn is_host_provided(self) -> bool {
        matches!(self, Self::Schedule | Self::Poll)
    }

    /// Parse the name a manifest or a command line uses.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
}

impl std::fmt::Display for TriggerMechanism {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One `[[triggers]]` entry.
///
/// Every trigger starts disabled: writing one down, installing its source or enabling its miner never starts it. The
/// settings after the common keys depend on the mechanism and are checked by [`TriggerDefinition::plan`]: an unknown
/// one is refused there, so a typo is as loud as it would be under `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerDefinition {
    /// The trigger's name: its identity in the configuration, in the CLI and in the API.
    pub name: String,
    /// The miner it asks to run.
    pub miner: String,
    /// How it decides to.
    #[serde(rename = "type")]
    pub kind: TriggerMechanism,
    /// Whether it may act. Off unless the user turned it on.
    #[serde(default)]
    pub enabled: bool,
    /// Where a `webhook` trigger's shared secret is read from; never the secret itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialRef>,
    /// The mechanism's own settings (`every`, `path`, `header`, ...), kept verbatim.
    #[serde(flatten)]
    pub settings: Map<String, Value>,
}

/// How a `webhook` delivery proves who sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebhookAuth {
    /// An HMAC-SHA-256 of the raw body, keyed with the shared secret, in a header (GitHub's way).
    HmacSha256,
    /// The shared secret itself in a header (Todoist's and many others' way). Compared in constant time.
    Token,
}

/// How a signature is written in its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureEncoding {
    /// Lowercase or uppercase hexadecimal.
    Hex,
    /// Standard base64.
    Base64,
}

/// What a `webhook` trigger checks on a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookPlan {
    /// How the sender proves itself.
    pub auth: WebhookAuth,
    /// The header that carries the signature or the token.
    pub header: String,
    /// What precedes the signature in that header (`sha256=`), removed before it is decoded.
    pub prefix: String,
    /// How the signature is encoded.
    pub encoding: SignatureEncoding,
    /// The header that carries a delivery's unique id, when the sender sets one: a redelivery of the same id is
    /// answered but not run again.
    pub delivery_header: Option<String>,
}

/// A trigger's settings, parsed and checked: what the supervisor runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerPlan {
    /// Every `every`, or at `at` (UTC) on every `every`-th day.
    Schedule {
        /// The interval.
        every: Duration,
        /// The time of day, for a timetable in whole days.
        at: Option<NaiveTime>,
    },
    /// Every `every`, backing off up to `max_backoff` while failing.
    Poll {
        /// The interval.
        every: Duration,
        /// The longest wait between attempts while failing.
        max_backoff: Duration,
    },
    /// On a delivery to the webhook listener.
    Webhook(WebhookPlan),
    /// On a change under `path`, once it has been quiet for `debounce`.
    Watch {
        /// The file or directory.
        path: PathBuf,
        /// How long it must stay quiet.
        debounce: Duration,
        /// Whether changes below a directory count.
        recursive: bool,
    },
}

impl TriggerDefinition {
    /// Parse and check this trigger's settings for its mechanism.
    ///
    /// # Errors
    ///
    /// A sentence naming the setting and what to change.
    pub fn plan(&self) -> Result<TriggerPlan, String> {
        let allowed: &[&str] = match self.kind {
            TriggerMechanism::Schedule => &["every", "at"],
            TriggerMechanism::Poll => &["every", "max_backoff"],
            TriggerMechanism::Webhook => {
                &["auth", "header", "prefix", "encoding", "delivery_header"]
            }
            TriggerMechanism::Watch => &["path", "debounce", "recursive"],
        };
        if let Some(unknown) = self
            .settings
            .keys()
            .find(|key| !allowed.contains(&key.as_str()))
        {
            return Err(format!(
                "`{unknown}` is not a setting of a `{}` trigger; it takes {}",
                self.kind,
                allowed
                    .iter()
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // A secret only means something to a webhook; one on another kind would suggest protection that is not there.
        if self.kind != TriggerMechanism::Webhook && self.credential.is_some() {
            return Err(format!(
                "a `{}` trigger has no use for a `credential`; only a `webhook` trigger checks a shared secret",
                self.kind
            ));
        }
        match self.kind {
            TriggerMechanism::Schedule => self.schedule_plan(),
            TriggerMechanism::Poll => {
                let every = self.every()?;
                let max_backoff = match self.text("max_backoff")? {
                    Some(text) => {
                        let wait =
                            parse_duration(text).map_err(|e| format!("`max_backoff`: {e}"))?;
                        if wait < every {
                            return Err(format!(
                                "`max_backoff` ({text}) is shorter than `every`; a backoff only lengthens the wait"
                            ));
                        }
                        wait
                    }
                    // Eight intervals: long enough to stop hammering a failing source, short enough to notice it recovered.
                    None => every.saturating_mul(8),
                };
                Ok(TriggerPlan::Poll { every, max_backoff })
            }
            TriggerMechanism::Webhook => self.webhook_plan(),
            TriggerMechanism::Watch => self.watch_plan(),
        }
    }

    fn text(&self, key: &str) -> Result<Option<&str>, String> {
        match self.settings.get(key) {
            None => Ok(None),
            Some(Value::String(text)) if !text.trim().is_empty() => Ok(Some(text.as_str())),
            Some(_) => Err(format!("`{key}` must be a non-empty string")),
        }
    }

    fn every(&self) -> Result<Duration, String> {
        let text = self.text("every")?.ok_or_else(|| {
            format!(
                "a `{}` trigger needs `every`, like `every = \"1h\"` (units: s, m, h, d)",
                self.kind
            )
        })?;
        let every = parse_duration(text).map_err(|e| format!("`every`: {e}"))?;
        if every < MIN_EVERY {
            return Err("`every` must be at least 1s".to_string());
        }
        Ok(every)
    }

    fn schedule_plan(&self) -> Result<TriggerPlan, String> {
        let every = self.every()?;
        let at = match self.text("at")? {
            None => None,
            Some(text) => {
                let time = NaiveTime::parse_from_str(text, "%H:%M").map_err(|_| {
                    format!("`at` ({text}) must be a time of day like `03:30` (UTC)")
                })?;
                // A time of day only makes sense on a timetable of whole days: "every 90 minutes at 03:30" has no meaning.
                if !every.as_secs().is_multiple_of(86_400) {
                    return Err(
                        "`at` needs `every` in whole days (`every = \"1d\"`), since it names a time of day".to_string(),
                    );
                }
                Some(time)
            }
        };
        Ok(TriggerPlan::Schedule { every, at })
    }

    fn webhook_plan(&self) -> Result<TriggerPlan, String> {
        let auth = match self.text("auth")? {
            None | Some("hmac-sha256") => WebhookAuth::HmacSha256,
            Some("token") => WebhookAuth::Token,
            Some(other) => {
                return Err(format!("`auth` ({other}) must be `hmac-sha256` or `token`"));
            }
        };
        let encoding = match self.text("encoding")? {
            None | Some("hex") => SignatureEncoding::Hex,
            Some("base64") => SignatureEncoding::Base64,
            Some(other) => return Err(format!("`encoding` ({other}) must be `hex` or `base64`")),
        };
        let header = match (self.text("header")?, auth) {
            (Some(header), _) => header.to_string(),
            (None, WebhookAuth::HmacSha256) => "x-hub-signature-256".to_string(),
            (None, WebhookAuth::Token) => "x-memcastle-token".to_string(),
        };
        let prefix = match (self.settings.get("prefix"), auth) {
            (Some(Value::String(prefix)), _) => prefix.clone(),
            (Some(_), _) => return Err("`prefix` must be a string".to_string()),
            (None, WebhookAuth::HmacSha256) => "sha256=".to_string(),
            (None, WebhookAuth::Token) => String::new(),
        };
        let delivery_header = self.text("delivery_header")?.map(str::to_string);
        for name in std::iter::once(&header).chain(delivery_header.as_ref()) {
            if !is_header_name(name) {
                return Err(format!("`{name}` is not a valid HTTP header name"));
            }
        }
        match &self.credential {
            Some(CredentialRef::Env { name }) if !name.trim().is_empty() => {}
            Some(CredentialRef::File { path }) if !path.trim().is_empty() => {}
            Some(CredentialRef::Oauth) => {
                return Err(
                    "a webhook's `credential` is a shared secret, so it is `env` or `file`, not `oauth`".to_string(),
                );
            }
            _ => {
                return Err(
                    "a `webhook` trigger needs a `credential` naming where its shared secret is kept, like \
                     `credential = { type = \"env\", name = \"MY_WEBHOOK_SECRET\" }`"
                        .to_string(),
                );
            }
        }
        Ok(TriggerPlan::Webhook(WebhookPlan {
            auth,
            header: header.to_ascii_lowercase(),
            prefix,
            encoding,
            delivery_header: delivery_header.map(|h| h.to_ascii_lowercase()),
        }))
    }

    fn watch_plan(&self) -> Result<TriggerPlan, String> {
        let path = self.text("path")?.ok_or_else(|| {
            "a `watch` trigger needs `path`, the absolute path to watch".to_string()
        })?;
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(format!(
                "`path` ({}) must be absolute: a daemon has no working directory a relative path could mean",
                path.display()
            ));
        }
        let debounce = match self.text("debounce")? {
            Some(text) => {
                let wait = parse_duration(text).map_err(|e| format!("`debounce`: {e}"))?;
                if !(MIN_DEBOUNCE..=MAX_DEBOUNCE).contains(&wait) {
                    return Err("`debounce` must be between 100ms and 1h".to_string());
                }
                wait
            }
            None => DEFAULT_DEBOUNCE,
        };
        let recursive = match self.settings.get("recursive") {
            None => true,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return Err("`recursive` must be true or false".to_string()),
        };
        Ok(TriggerPlan::Watch {
            path,
            debounce,
            recursive,
        })
    }

    /// Check what can be checked without knowing the miner or the source: the name, the shape of the settings and the
    /// rule that keeps a secret out of the file. That the miner exists and its source supports the mechanism is checked
    /// at activation.
    ///
    /// # Errors
    ///
    /// Returns what is wrong, as a sentence that names the field and the fix.
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_trigger_name(&self.name) {
            return Err(format!(
                "`{}` is not a valid trigger name; use 1-{MAX_TRIGGER_NAME_LEN} lowercase letters, digits, `-` or `_`, \
                 starting with a letter or digit (`reload` is reserved)",
                self.name
            ));
        }
        if !super::is_valid_miner_name(&self.miner) {
            return Err(format!(
                "`miner` ({}) is not a miner name; name the miner this trigger asks to run",
                self.miner
            ));
        }
        for key in self.settings.keys() {
            let lower = key.to_ascii_lowercase();
            if [
                "password",
                "passwd",
                "secret",
                "token",
                "api_key",
                "apikey",
                "private_key",
            ]
            .iter()
            .any(|word| lower.contains(word))
            {
                return Err(format!(
                    "`{key}` looks like a secret, which must not be written to the configuration file; put the \
                     secret in an environment variable or a file and point `credential` at it"
                ));
            }
        }
        self.plan().map(|_| ())
    }
}

/// Validate every trigger and that no two share a name.
///
/// # Errors
///
/// Returns the first problem, naming the trigger it is about.
pub fn validate_triggers(triggers: &[TriggerDefinition]) -> Result<(), String> {
    for (index, trigger) in triggers.iter().enumerate() {
        trigger
            .validate()
            .map_err(|reason| format!("trigger `{}`: {reason}", trigger.name))?;
        if triggers[..index].iter().any(|t| t.name == trigger.name) {
            return Err(format!(
                "trigger `{}` is defined twice; names are unique, so rename or remove one",
                trigger.name
            ));
        }
    }
    Ok(())
}

/// A header name is a token: letters, digits and a few punctuation marks, never a space or a colon.
fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Parse `500ms`, `30s`, `5m`, `2h` or `1d` (a whole number and a unit).
///
/// # Errors
///
/// A sentence saying what a duration looks like.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .filter(|&at| at > 0)
        .ok_or_else(|| format!("`{text}` is not a duration; write a number and a unit, like `30s`, `5m`, `2h` or `1d`"))?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("`{text}` is too large a duration"))?;
    let millis_per_unit: u64 = match unit {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => {
            return Err(format!(
                "`{text}` has the unit `{unit}`; use `ms`, `s`, `m`, `h` or `d`"
            ));
        }
    };
    number
        .checked_mul(millis_per_unit)
        .map(Duration::from_millis)
        .ok_or_else(|| format!("`{text}` is too large a duration"))
}

/// When a schedule next fires.
///
/// Without `at`, one `every` after `last` (or after `now` for a trigger that never fired, so enabling it waits a whole
/// interval and never fires on the spot). With `at`, the time of day (UTC) `every` days after `last`'s day, or the next
/// such time after `now` for a trigger that never fired. A result in the past is "overdue": the caller fires once, not
/// once per missed slot.
#[must_use]
pub fn next_due(
    every: Duration,
    at: Option<NaiveTime>,
    last: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let every_chrono = chrono::Duration::from_std(every).unwrap_or(chrono::Duration::days(1));
    let Some(at) = at else {
        return last.unwrap_or(now) + every_chrono;
    };
    let at_on = |day: DateTime<Utc>| Utc.from_utc_datetime(&day.date_naive().and_time(at));
    match last {
        Some(last) => at_on(last) + every_chrono,
        None => {
            let today = at_on(now);
            if today > now {
                today
            } else {
                today + chrono::Duration::days(1)
            }
        }
    }
}

/// What the daemon remembers about one trigger, never whether it is enabled (that is the file's alone, so no restart
/// can override a user's choice).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerState {
    /// The trigger this is about.
    pub name: String,
    /// When it last asked for a run, whatever came of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<DateTime<Utc>>,
    /// The job it last asked for or merged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_job: Option<JobId>,
    /// How many runs it asked for.
    #[serde(default)]
    pub fired: u64,
    /// How many requests were merged into a run already waiting.
    #[serde(default)]
    pub coalesced: u64,
    /// How many deliveries were refused as repeats.
    #[serde(default)]
    pub duplicates: u64,
    /// For a timetable: when it next fires, kept so a restart neither forgets the wait nor fires early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due: Option<DateTime<Utc>>,
    /// The last thing that went wrong: what failed, never a payload or a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When it went wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<DateTime<Utc>>,
    /// How many times in a row it has gone wrong; a success resets it.
    #[serde(default)]
    pub consecutive_failures: u32,
}

impl TriggerState {
    /// What a trigger that has never done anything has.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            last_fired_at: None,
            last_job: None,
            fired: 0,
            coalesced: 0,
            duplicates: 0,
            next_due: None,
            last_error: None,
            last_error_at: None,
            consecutive_failures: 0,
        }
    }

    /// Record a failure.
    pub fn failed(&mut self, error: impl Into<String>, at: DateTime<Utc>) {
        self.last_error = Some(error.into());
        self.last_error_at = Some(at);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// Record that the trigger works, which clears the failure but keeps the last error for the record.
    pub fn succeeded(&mut self) {
        self.consecutive_failures = 0;
    }
}

/// A delivery the daemon accepted, kept so an at-least-once sender's repeat is recognised and a crash between
/// accepting and queueing loses nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerDelivery {
    /// The trigger it came to.
    pub trigger: String,
    /// The sender's id for it.
    pub key: String,
    /// When it was accepted.
    pub received_at: DateTime<Utc>,
    /// The job it became, or was merged into; `None` while it is accepted but not yet queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobId>,
}

/// What asking for a run came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum FireOutcome {
    /// A mining job was queued.
    Queued {
        /// The job.
        job: JobId,
    },
    /// A run for the same source was already waiting, so this request joined it instead of queueing another: whatever
    /// changed is read when that job runs. This is what keeps a burst of events from becoming a burst of jobs.
    Coalesced {
        /// The job waiting.
        job: JobId,
    },
    /// A delivery with this id was already accepted: answered, not run again.
    Duplicate,
}

/// Where a trigger stands, worked out each time and never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerStatus {
    /// Switched off, which is how every trigger starts.
    Disabled,
    /// Enabled and working.
    Active,
    /// Enabled, but its last attempts failed; the reason is on the trigger.
    Failing,
    /// Enabled, but something it needs is missing, so it is not running; the reason says what.
    Unavailable,
}

/// A source's declaration that it supports a trigger mechanism: a capability, never an activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTrigger {
    /// One line saying what the mechanism means for this source (which events, which files).
    pub description: String,
}

/// A mechanism a source supports, as `GET /api/sources` and `memcastle sources` show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerSpec {
    /// The mechanism.
    pub kind: TriggerMechanism,
    /// What it means for this source; empty for one the host provides to every source.
    #[serde(default)]
    pub description: String,
}

/// The mechanisms a source supports: those the host gives every source, then the ones it declared.
#[must_use]
pub fn supported_triggers(declared: &[(TriggerMechanism, String)]) -> Vec<TriggerSpec> {
    TriggerMechanism::ALL
        .into_iter()
        .filter_map(|kind| {
            let declared = declared.iter().find(|(k, _)| *k == kind);
            (kind.is_host_provided() || declared.is_some()).then(|| TriggerSpec {
                kind,
                description: declared.map(|(_, d)| d.clone()).unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn trigger(toml_text: &str) -> TriggerDefinition {
        toml::from_str(toml_text).expect("a trigger")
    }

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().expect("a timestamp")
    }

    #[test]
    fn a_trigger_is_disabled_unless_the_file_says_otherwise() {
        let t = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"5m\"\n");
        assert!(!t.enabled);
        assert_eq!(t.validate(), Ok(()));
    }

    #[test]
    fn durations_take_a_unit_and_refuse_everything_else() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Ok(Duration::from_secs(86_400)));
        for bad in ["", "5", "m", "5x", "-5m", "1.5h", "99999999999999999999d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_misspelt_setting_is_refused_rather_than_ignored() {
        let t = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"schedule\"\nevery = \"1d\"\nevr = \"2d\"\n",
        );
        let error = t.validate().expect_err("an unknown key");
        assert!(
            error.contains("`evr`") && error.contains("`every`"),
            "{error}"
        );
    }

    #[test]
    fn each_mechanism_says_what_it_is_missing() {
        for (text, wants) in [
            ("type = \"schedule\"", "needs `every`"),
            ("type = \"poll\"", "needs `every`"),
            ("type = \"watch\"", "needs `path`"),
            ("type = \"webhook\"", "needs a `credential`"),
        ] {
            let t = trigger(&format!("name = \"t\"\nminer = \"m\"\n{text}\n"));
            let error = t.validate().expect_err(text);
            assert!(error.contains(wants), "{text}: {error}");
        }
    }

    #[test]
    fn a_time_of_day_needs_a_timetable_in_whole_days() {
        let ok = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"schedule\"\nevery = \"1d\"\nat = \"03:30\"\n",
        );
        assert_eq!(ok.validate(), Ok(()));
        let bad = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"schedule\"\nevery = \"90m\"\nat = \"03:30\"\n",
        );
        assert!(
            bad.validate()
                .expect_err("at with minutes")
                .contains("whole days")
        );
        let worse = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"schedule\"\nevery = \"1d\"\nat = \"25:99\"\n",
        );
        assert!(worse.validate().is_err());
    }

    #[test]
    fn a_watch_path_must_be_absolute_and_its_debounce_sane() {
        let relative = trigger("name = \"t\"\nminer = \"m\"\ntype = \"watch\"\npath = \"notes\"\n");
        assert!(
            relative
                .validate()
                .expect_err("relative")
                .contains("absolute")
        );
        // Absolute on every platform: `/n` has no drive letter, so it is relative on Windows.
        let absolute = std::env::temp_dir().display().to_string();
        let quick = trigger(&format!(
            "name = \"t\"\nminer = \"m\"\ntype = \"watch\"\npath = '{absolute}'\ndebounce = \"1ms\"\n"
        ));
        let error = quick
            .validate()
            .expect_err("a debounce shorter than a write burst");
        assert!(
            error.contains("debounce"),
            "refused for its debounce and not its path: {error}"
        );
        let good = trigger(&format!(
            "name = \"t\"\nminer = \"m\"\ntype = \"watch\"\npath = '{absolute}'\n"
        ));
        assert!(matches!(
            good.plan(),
            Ok(TriggerPlan::Watch { recursive: true, debounce, .. }) if debounce == DEFAULT_DEBOUNCE
        ));
    }

    #[test]
    fn a_webhook_needs_an_env_or_file_secret_and_never_an_oauth_one() {
        let env = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\ncredential = { type = \"env\", name = \"HOOK\" }\n",
        );
        assert!(
            matches!(env.plan(), Ok(TriggerPlan::Webhook(p)) if p.auth == WebhookAuth::HmacSha256 && p.prefix == "sha256=")
        );
        let oauth = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\ncredential = { type = \"oauth\" }\n",
        );
        assert!(oauth.validate().is_err());
        let token = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\nauth = \"token\"\nheader = \"X-Todoist-Token\"\n\
             credential = { type = \"file\", path = \"/run/secrets/hook\" }\n",
        );
        assert!(
            matches!(token.plan(), Ok(TriggerPlan::Webhook(p)) if p.header == "x-todoist-token" && p.prefix.is_empty())
        );
    }

    #[test]
    fn only_a_webhook_has_a_credential() {
        let t = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\ncredential = { type = \"env\", name = \"X\" }\n",
        );
        assert!(
            t.validate()
                .expect_err("a poll credential")
                .contains("only a `webhook`")
        );
    }

    #[test]
    fn a_secret_looking_setting_is_refused_with_the_way_out() {
        let t =
            trigger("name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\nsigning_secret = \"x\"\n");
        let error = t.validate().expect_err("a secret key");
        assert!(
            error.contains("looks like a secret") && error.contains("credential"),
            "{error}"
        );
    }

    #[test]
    fn names_are_checked_for_shape_and_the_reserved_one_is_refused() {
        for good in ["a", "docs-watch", "x_1"] {
            assert!(is_valid_trigger_name(good), "{good}");
        }
        for bad in ["", "-a", "A", "a/b", "reload", &"a".repeat(65)] {
            assert!(!is_valid_trigger_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_duplicate_name_is_refused() {
        let t = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\n");
        let error = validate_triggers(&[t.clone(), t]).expect_err("a duplicate");
        assert!(error.contains("defined twice"), "{error}");
    }

    #[test]
    fn a_trigger_round_trips_through_toml() {
        let t = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\nenabled = true\ndelivery_header = \"x-id\"\n\
             [credential]\ntype = \"env\"\nname = \"HOOK\"\n",
        );
        let text = toml::to_string(&t).expect("serialises");
        assert_eq!(
            toml::from_str::<TriggerDefinition>(&text).expect("parses"),
            t,
            "{text}"
        );
        assert_eq!(t.settings["delivery_header"], json!("x-id"));
    }

    #[test]
    fn a_schedule_without_a_time_of_day_waits_one_interval_from_the_last_run_or_from_now() {
        let now = at("2030-01-01T10:00:00Z");
        let hour = Duration::from_secs(3600);
        assert_eq!(next_due(hour, None, None, now), at("2030-01-01T11:00:00Z"));
        assert_eq!(
            next_due(hour, None, Some(at("2030-01-01T09:30:00Z")), now),
            at("2030-01-01T10:30:00Z")
        );
    }

    #[test]
    fn a_schedule_at_a_time_of_day_fires_at_that_time_in_utc() {
        let day = Duration::from_secs(86_400);
        let three_thirty = NaiveTime::from_hms_opt(3, 30, 0);
        // Never fired, and 03:30 has not come yet today.
        assert_eq!(
            next_due(day, three_thirty, None, at("2030-01-01T02:00:00Z")),
            at("2030-01-01T03:30:00Z")
        );
        // Never fired, and 03:30 is past: tomorrow's, so enabling never fires on the spot.
        assert_eq!(
            next_due(day, three_thirty, None, at("2030-01-01T04:00:00Z")),
            at("2030-01-02T03:30:00Z")
        );
        // Fired this morning: the next one is a day on, whatever the clock says now.
        assert_eq!(
            next_due(
                day,
                three_thirty,
                Some(at("2030-01-01T03:30:05Z")),
                at("2030-01-01T12:00:00Z")
            ),
            at("2030-01-02T03:30:00Z")
        );
    }

    #[test]
    fn a_failure_is_counted_and_a_success_clears_the_count_but_not_the_record() {
        let mut state = TriggerState::new("t");
        state.failed("no route", at("2030-01-01T00:00:00Z"));
        state.failed("no route", at("2030-01-01T00:01:00Z"));
        assert_eq!(state.consecutive_failures, 2);
        state.succeeded();
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.last_error.as_deref(), Some("no route"));
    }

    #[test]
    fn a_timetable_and_a_poll_are_there_for_every_source_and_the_rest_are_declared() {
        let none = supported_triggers(&[]);
        assert_eq!(
            none.iter().map(|s| s.kind).collect::<Vec<_>>(),
            [TriggerMechanism::Schedule, TriggerMechanism::Poll]
        );
        let watch = supported_triggers(&[(TriggerMechanism::Watch, "the notes".to_string())]);
        assert_eq!(watch.len(), 3);
        assert_eq!(watch[2].description, "the notes");
    }

    #[test]
    fn a_poll_ceiling_must_parse_and_may_not_be_shorter_than_the_interval() {
        let ok = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\nmax_backoff = \"1h\"\n",
        );
        assert!(matches!(
            ok.plan(),
            Ok(TriggerPlan::Poll { max_backoff, .. }) if max_backoff == Duration::from_secs(3600)
        ));
        let short = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1h\"\nmax_backoff = \"1m\"\n",
        );
        assert!(
            short
                .validate()
                .expect_err("shorter")
                .contains("shorter than `every`")
        );
        let junk = trigger(
            "name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1h\"\nmax_backoff = \"soon\"\n",
        );
        assert!(junk.validate().expect_err("junk").contains("max_backoff"));
        let default = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\n");
        assert!(matches!(
            default.plan(),
            Ok(TriggerPlan::Poll { max_backoff, .. }) if max_backoff == Duration::from_secs(480)
        ));
    }

    #[test]
    fn a_setting_of_the_wrong_type_is_named() {
        let number = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = 5\n");
        assert!(
            number
                .validate()
                .expect_err("a number")
                .contains("non-empty string")
        );
        let blank = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \" \"\n");
        assert!(
            blank
                .validate()
                .expect_err("blank")
                .contains("non-empty string")
        );
        let tiny = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"500ms\"\n");
        assert!(
            tiny.validate()
                .expect_err("under a second")
                .contains("at least 1s")
        );
        let junk = trigger("name = \"t\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"often\"\n");
        assert!(
            junk.validate()
                .expect_err("not a duration")
                .contains("`every`")
        );
    }

    #[test]
    fn a_webhook_names_what_is_wrong_with_each_of_its_settings() {
        let secret = "credential = { type = \"env\", name = \"HOOK\" }\n";
        for (extra, wants) in [
            ("auth = \"md5\"\n", "`auth`"),
            ("encoding = \"rot13\"\n", "`encoding`"),
            ("prefix = 5\n", "`prefix` must be a string"),
            ("header = \"bad header\"\n", "not a valid HTTP header name"),
            (
                "delivery_header = \"a:b\"\n",
                "not a valid HTTP header name",
            ),
        ] {
            let t = trigger(&format!(
                "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\n{extra}{secret}"
            ));
            let error = t.validate().expect_err(extra);
            assert!(error.contains(wants), "{extra}: {error}");
        }
        for credential in [
            "credential = { type = \"env\", name = \" \" }",
            "credential = { type = \"file\", path = \"\" }",
        ] {
            let t = trigger(&format!(
                "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\n{credential}\n"
            ));
            assert!(
                t.validate()
                    .expect_err(credential)
                    .contains("needs a `credential`")
            );
        }
        let base64 = trigger(&format!(
            "name = \"t\"\nminer = \"m\"\ntype = \"webhook\"\nencoding = \"base64\"\nprefix = \"\"\n{secret}"
        ));
        assert!(matches!(
            base64.plan(),
            Ok(TriggerPlan::Webhook(p)) if p.encoding == SignatureEncoding::Base64 && p.prefix.is_empty()
        ));
    }

    #[test]
    fn the_miner_a_trigger_names_must_be_a_miner_name_and_a_watch_flag_a_boolean() {
        let bad_miner =
            trigger("name = \"t\"\nminer = \"Not A Miner\"\ntype = \"poll\"\nevery = \"1m\"\n");
        assert!(
            bad_miner
                .validate()
                .expect_err("bad miner")
                .contains("is not a miner name")
        );
        let bad_name = trigger("name = \"T\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\n");
        assert!(
            bad_name
                .validate()
                .expect_err("bad name")
                .contains("not a valid trigger name")
        );
        let absolute = std::env::temp_dir().display().to_string();
        let flag = |value: &str| {
            trigger(&format!(
                "name = \"t\"\nminer = \"m\"\ntype = \"watch\"\npath = '{absolute}'\nrecursive = {value}\n"
            ))
        };
        assert!(matches!(
            flag("false").plan(),
            Ok(TriggerPlan::Watch {
                recursive: false,
                ..
            })
        ));
        assert!(
            flag("\"yes\"")
                .validate()
                .expect_err("not a bool")
                .contains("true or false")
        );
        let slow = trigger(&format!(
            "name = \"t\"\nminer = \"m\"\ntype = \"watch\"\npath = '{absolute}'\ndebounce = \"2d\"\n"
        ));
        assert!(slow.validate().expect_err("too slow").contains("debounce"));
    }

    #[test]
    fn a_mechanism_is_found_by_the_name_a_manifest_writes_and_prints_the_same_name() {
        for kind in TriggerMechanism::ALL {
            assert_eq!(TriggerMechanism::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
        assert_eq!(TriggerMechanism::parse("cron"), None);
        assert!(TriggerMechanism::Schedule.is_host_provided());
        assert!(!TriggerMechanism::Webhook.is_host_provided());
    }
}
