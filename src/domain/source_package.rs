//! The installable source package: what a source declares, what the user agreed to, and where it is in its lifecycle.
//!
//! Pure types, no I/O (docs/adr/026). A package is a WebAssembly Component plus a [`SourceManifest`]; MemCastle runs
//! it behind the same `SourceAdapter` contract as a built-in source. Everything here is data and decisions about
//! data: reading a package from disk is `crate::source`, running one is `crate::mining::wasm`, the rows are
//! `crate::store`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{SourceCapabilities, sha256_hex};

/// The version of the source contract (the WIT world `memcastle:source`) this MemCastle implements.
///
/// While the major version is `0`, a source built for `0.N` runs only on a host that implements `0.N`; from `1.0` a
/// source runs on any host of the same major version whose minor is at least the source's. A patch version is
/// documentation only and never affects compatibility. See [`contract_compatibility`].
pub const CONTRACT_VERSION: &str = "0.3.0";

/// What a source package declares about itself: `memcastle-source.toml`.
///
/// Every table refuses unknown keys, so a misspelt permission is an error at build time rather than a source that
/// silently runs without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    /// The version of this manifest format (docs/publishing-sources.md). Absent means 1; a number this MemCastle does
    /// not know is refused as incompatible, so a manifest that means something new is never half-understood.
    #[serde(default = "default_format")]
    pub format: u32,
    /// Identity: the name users give as `--source`, its version and a line saying what it reads.
    pub source: ManifestSource,
    /// Which contract and which MemCastle versions it was built for.
    pub compatibility: Compatibility,
    /// What it can do, with the same meaning as for a built-in source.
    #[serde(default)]
    pub capabilities: SourceCapabilities,
    /// What it asks the host for. Nothing is granted that is not listed here, and nothing listed here is granted
    /// without the user's consent at install.
    #[serde(default)]
    pub permissions: Permissions,
    /// Resource ceilings it asks for, clamped by the host's own.
    #[serde(default)]
    pub limits: ResourceLimits,
    /// How `memcastle source build` produces the component. Development-time only: the daemon never runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildSection>,
    /// Where `memcastle source test` finds its conformance cases. Development-time only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<TestSection>,
}

/// `[source]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSource {
    /// The source's name: lowercase letters, digits and `-`.
    pub name: String,
    /// The package's own semantic version.
    pub version: String,
    /// One line saying what the source reads.
    pub description: String,
    /// The SPDX identifier of the licence the source is distributed under, for people choosing what to install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// A page about the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Where its code is, so a package can be traced to what it was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

/// The longest a source name may be: it is a path segment, a CLI argument and a column in a table.
pub const MAX_SOURCE_NAME_LEN: usize = 48;

/// Whether `name` is a well-formed source name: 1 to [`MAX_SOURCE_NAME_LEN`] lowercase letters, digits or `-`, not
/// starting or ending with `-`.
///
/// A name is a directory under the sources directory and a segment of a registry's package URL, so restricting its
/// alphabet is what keeps `../x` out of both.
#[must_use]
pub fn is_valid_source_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SOURCE_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// The manifest format this MemCastle reads.
pub const MANIFEST_FORMAT: u32 = 1;

fn default_format() -> u32 {
    MANIFEST_FORMAT
}

/// `[compatibility]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    /// The contract version the component was built against, as `MAJOR.MINOR` or `MAJOR.MINOR.PATCH`.
    pub contract: String,
    /// The MemCastle versions it runs on, as a semver requirement (`">=0.3, <0.4"`).
    pub memcastle: String,
}

/// What a source asks the host for. Deliberately small: the host offers no write access, no ambient environment and
/// no shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Permissions {
    /// Directories the source may read.
    pub filesystem: FilesystemPermissions,
    /// Whether the source may open network connections. All or nothing: the host cannot yet restrict by host name.
    pub network: bool,
    /// Programs the source may run through the host (by exact name, no shell, no other program).
    pub process: Vec<String>,
    /// Environment variables the source may read; it sees no others.
    pub env: Vec<String>,
    /// The OAuth sign-in the source needs, when it has one (docs/adr/039): the host runs the flow, keeps the tokens, and
    /// hands the source a fresh access token through `host.access-token`. Absent is absent from the consent digest too,
    /// so a source that asks for no sign-in keeps the digest it was consented under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthRequirement>,
}

/// Whether `url` is an endpoint a token may be sent to: `https`, or `http` to a loopback address.
#[must_use]
pub fn is_secure_endpoint(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Credentials in the URL would be logged by anything that logs a URL.
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    match scheme {
        "https" => true,
        "http" => {
            let host = if let Some(bracketed) = authority.strip_prefix('[') {
                bracketed.split(']').next().unwrap_or("")
            } else {
                authority.split(':').next().unwrap_or("")
            };
            matches!(host, "localhost" | "127.0.0.1" | "::1")
        }
        _ => false,
    }
}

/// `[permissions.oauth]`: the OAuth endpoints a source signs in at, and what it asks to do there.
///
/// A public client only: no client secret is ever written in a manifest. Which flows the source supports follows from
/// which endpoints it names: a device authorization endpoint allows the device flow, an authorization endpoint allows
/// the browser flow with PKCE. Everything here is shown to the user at install, and changing any of it asks again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthRequirement {
    /// The public client identifier MemCastle presents to the provider.
    pub client_id: String,
    /// What the source asks to be allowed to do. Sorted by [`Permissions::normalized`].
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Where an authorization code or refresh token is exchanged for an access token.
    pub token_url: String,
    /// The browser flow's authorization endpoint (authorization code with PKCE, RFC 7636).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    /// The device flow's endpoint (RFC 8628), for a machine with no browser of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_authorization_url: Option<String>,
}

impl OAuthRequirement {
    /// Whether the device flow is available.
    #[must_use]
    pub fn supports_device(&self) -> bool {
        self.device_authorization_url.is_some()
    }

    /// Whether the browser flow is available.
    #[must_use]
    pub fn supports_browser(&self) -> bool {
        self.authorize_url.is_some()
    }

    /// The first thing wrong with this requirement, as a message that names the field.
    ///
    /// Endpoints must be `https`: a token or an authorization code never crosses the network in the clear. Plain `http` is
    /// accepted only to a loopback address, which is how a provider is faked in a test and never leaves the machine.
    ///
    /// # Errors
    ///
    /// A message naming the field that is wrong.
    pub fn validate(&self) -> Result<(), String> {
        if self.client_id.trim().is_empty() {
            return Err("permissions.oauth.client_id must not be empty".to_string());
        }
        if self.authorize_url.is_none() && self.device_authorization_url.is_none() {
            return Err("permissions.oauth needs `authorize_url`, `device_authorization_url`, or both, so that there \
                        is a way to sign in"
                .to_string());
        }
        let endpoints = [
            ("token_url", Some(self.token_url.as_str())),
            ("authorize_url", self.authorize_url.as_deref()),
            (
                "device_authorization_url",
                self.device_authorization_url.as_deref(),
            ),
        ];
        for (field, url) in endpoints {
            let Some(url) = url else { continue };
            if !is_secure_endpoint(url) {
                return Err(format!(
                    "permissions.oauth.{field} `{url}` must be an `https://` URL (plain `http://` only to localhost)"
                ));
            }
        }
        for scope in &self.scopes {
            // A scope is one token of a space-separated list on the wire, so a space would split it in two.
            if scope.is_empty() || scope.contains(char::is_whitespace) {
                return Err(format!(
                    "permissions.oauth.scopes `{scope}` must be one word with no spaces"
                ));
            }
        }
        Ok(())
    }

    /// A digest of everything a stored credential was obtained under.
    ///
    /// A credential kept under a different digest than the installed manifest's is treated as missing, so an update that
    /// changes the client, the endpoints or the scopes never keeps using tokens granted under the old terms.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut scopes = self.scopes.clone();
        scopes.sort();
        scopes.dedup();
        let canonical = serde_json::json!({
            "client_id": self.client_id,
            "scopes": scopes,
            "token_url": self.token_url,
            "authorize_url": self.authorize_url,
            "device_authorization_url": self.device_authorization_url,
        });
        sha256_hex(canonical.to_string().as_bytes())
    }

    /// The host name tokens are exchanged with, for a person to recognise at consent.
    #[must_use]
    pub fn provider(&self) -> String {
        let rest = self
            .token_url
            .split_once("://")
            .map_or(self.token_url.as_str(), |(_, rest)| rest);
        rest.split(['/', '?', '#'])
            .next()
            .unwrap_or(rest)
            .to_string()
    }
}

/// `[permissions.filesystem]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilesystemPermissions {
    /// Absolute directories (`~/` is the home directory), or the word `locator`: the directory the source is asked
    /// to mine, whatever it is.
    pub read: Vec<String>,
}

impl Permissions {
    /// The same permissions with every list sorted and de-duplicated, so two manifests that ask for the same things
    /// agree on [`Permissions::consent_digest`] whatever order they list them in.
    #[must_use]
    pub fn normalized(&self) -> Self {
        let sorted = |list: &[String]| {
            let mut list = list.to_vec();
            list.sort();
            list.dedup();
            list
        };
        Self {
            filesystem: FilesystemPermissions {
                read: sorted(&self.filesystem.read),
            },
            network: self.network,
            process: sorted(&self.process),
            env: sorted(&self.env),
            oauth: self.oauth.as_ref().map(|oauth| OAuthRequirement {
                scopes: sorted(&oauth.scopes),
                ..oauth.clone()
            }),
        }
    }

    /// Whether the source asks for nothing at all, so there is nothing to consent to.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filesystem.read.is_empty()
            && !self.network
            && self.process.is_empty()
            && self.env.is_empty()
            && self.oauth.is_none()
    }

    /// The digest that stands for the user agreeing to exactly these permissions for the source called `name`.
    ///
    /// Bound to the name so that consent given to one source is not a blank cheque for another, and to the
    /// permissions so that an upgrade that asks for more needs asking again.
    #[must_use]
    pub fn consent_digest(&self, name: &str) -> String {
        let canonical = serde_json::json!({ "name": name, "permissions": self.normalized() });
        sha256_hex(canonical.to_string().as_bytes())
    }

    /// One line a person can read before agreeing.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "none".to_string();
        }
        let mut parts = Vec::new();
        if !self.filesystem.read.is_empty() {
            parts.push(format!(
                "read files under {}",
                self.filesystem.read.join(", ")
            ));
        }
        if self.network {
            parts.push("use the network".to_string());
        }
        if !self.process.is_empty() {
            parts.push(format!("run {}", self.process.join(", ")));
        }
        if !self.env.is_empty() {
            parts.push(format!(
                "read environment variables {}",
                self.env.join(", ")
            ));
        }
        if let Some(oauth) = &self.oauth {
            let scopes = if oauth.scopes.is_empty() {
                String::new()
            } else {
                format!(" (scopes {})", oauth.scopes.join(", "))
            };
            parts.push(format!(
                "sign in with OAuth at {}{scopes}",
                oauth.provider()
            ));
        }
        parts.join("; ")
    }
}

/// `[limits]`: what a source asks for, never more than the host allows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ResourceLimits {
    /// Linear memory the component may use, in MiB.
    pub memory_mib: Option<u32>,
    /// How long one call (`discover`, `read`, ...) may run, in seconds.
    pub timeout_secs: Option<u64>,
}

/// `[build]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSection {
    /// The program and arguments that produce the component, run from the package's directory.
    pub command: Vec<String>,
    /// Where the component appears afterwards, relative to the package's directory.
    pub output: String,
}

/// `[test]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSection {
    /// The directory of conformance cases, relative to the package's directory.
    pub fixtures: String,
}

/// The `(major, minor)` of a contract version written as `MAJOR.MINOR` or `MAJOR.MINOR.PATCH`.
///
/// # Errors
///
/// A sentence saying what is wrong with `raw`.
pub fn contract_version(raw: &str) -> std::result::Result<(u64, u64), String> {
    let mut parts = raw.trim().split('.');
    let mut number = |what: &str| {
        parts
            .next()
            .ok_or_else(|| format!("contract `{raw}` has no {what} version"))?
            .parse::<u64>()
            .map_err(|_| format!("contract `{raw}` is not a version like `0.1`"))
    };
    let major = number("major")?;
    let minor = number("minor")?;
    Ok((major, minor))
}

/// Whether a source built for `declared` runs on this host, which implements [`CONTRACT_VERSION`].
///
/// # Errors
///
/// A sentence saying why not, for the error that reports it.
pub fn contract_compatibility(declared: &str) -> std::result::Result<(), String> {
    let host =
        contract_version(CONTRACT_VERSION).map_err(|_| "unparsable host contract".to_string())?;
    if contracts_agree(host, contract_version(declared)?) {
        Ok(())
    } else {
        Err(format!(
            "it was built for contract {declared}, and this MemCastle implements {CONTRACT_VERSION}"
        ))
    }
}

/// Whether a source built for `contract` and requiring `requirement` of MemCastle runs on the MemCastle `running`.
///
/// The one policy behind installing, loading and choosing a version from a registry index, so they cannot disagree
/// about what "compatible" means.
///
/// # Errors
///
/// A sentence saying why not, for the error that reports it.
pub fn version_compatibility(
    contract: &str,
    requirement: &str,
    running: &semver::Version,
) -> std::result::Result<(), String> {
    contract_compatibility(contract)?;
    let parsed = semver::VersionReq::parse(requirement)
        .map_err(|e| format!("its MemCastle requirement `{requirement}` is not valid: {e}"))?;
    // A pre-release of the running version (`0.3.0-rc.1`) is held to the release's requirement: otherwise every
    // release candidate would be incompatible with a source built for the release.
    let release = semver::Version::new(running.major, running.minor, running.patch);
    if parsed.matches(&release) {
        Ok(())
    } else {
        Err(format!(
            "it requires MemCastle {requirement}, and this is {running}"
        ))
    }
}

/// The policy itself, on `(major, minor)` pairs, so both halves of it can be tested whatever the host implements today.
fn contracts_agree((host_major, host_minor): (u64, u64), (major, minor): (u64, u64)) -> bool {
    if host_major == 0 {
        // Before 1.0 every minor release may break the contract.
        major == 0 && minor == host_minor
    } else {
        major == host_major && minor <= host_minor
    }
}

/// Where an installed source is in its lifecycle, as stored.
///
/// `unavailable` is deliberately not here: it is a fact about the files and the running MemCastle (a missing or
/// altered component, an incompatible version), so it is computed on every look and never persisted, where it could
/// go stale. See [`SourceState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePackageState {
    /// Installed and not yet enabled: it cannot be mined until the user turns it on.
    Installed,
    /// Available to mine.
    Enabled,
    /// Turned off by the user; installed files and history are kept.
    Disabled,
}

/// What can happen to an installed source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePackageEvent {
    /// The user turns it on.
    Enable,
    /// The user turns it off.
    Disable,
}

impl std::fmt::Display for SourcePackageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The wire names, like `SourceState`'s: a message that said `Enabled` would not match what `source list` shows.
        f.write_str(match self {
            Self::Installed => "installed",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        })
    }
}

impl std::fmt::Display for SourcePackageEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
        })
    }
}

/// An event the source's state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a source that is {state} cannot be asked to {event}")]
pub struct PackageTransitionError {
    /// Where the source was.
    pub state: SourcePackageState,
    /// What was attempted.
    pub event: SourcePackageEvent,
}

impl SourcePackageState {
    /// The state after `event`, or why not.
    ///
    /// The only way a stored source's state changes, so the lifecycle is one table that a test can enumerate.
    ///
    /// # Errors
    ///
    /// [`PackageTransitionError`] when the source is already where the event would put it.
    pub fn apply(
        self,
        event: SourcePackageEvent,
    ) -> std::result::Result<Self, PackageTransitionError> {
        match (self, event) {
            (Self::Installed | Self::Disabled, SourcePackageEvent::Enable) => Ok(Self::Enabled),
            (Self::Installed | Self::Enabled, SourcePackageEvent::Disable) => Ok(Self::Disabled),
            (Self::Enabled, SourcePackageEvent::Enable)
            | (Self::Disabled, SourcePackageEvent::Disable) => {
                Err(PackageTransitionError { state: self, event })
            }
        }
    }
}

/// The lifecycle state a source reports: the stored one, or `unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// Installed and not yet enabled.
    Installed,
    /// Available to mine.
    Enabled,
    /// Turned off.
    Disabled,
    /// Installed but unable to run here: the component is missing or was altered, or it does not fit this MemCastle.
    Unavailable,
}

impl From<SourcePackageState> for SourceState {
    fn from(state: SourcePackageState) -> Self {
        match state {
            SourcePackageState::Installed => Self::Installed,
            SourcePackageState::Enabled => Self::Enabled,
            SourcePackageState::Disabled => Self::Disabled,
        }
    }
}

impl std::fmt::Display for SourceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Installed => "installed",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
        })
    }
}

/// Where a source comes from, which is also how it was installed (docs/adr/033).
///
/// Every kind shares one lifecycle (list, enable, disable, remove); the origin only says what an update looks at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOrigin {
    /// Compiled into MemCastle.
    #[default]
    Builtin,
    /// A package archive or project directory the user installed from their own disk. It has no upstream, so
    /// `update` leaves it alone.
    Package,
    /// A package shipped alongside MemCastle (`share/memcastle/sources`): independently packaged, not linked in.
    Bundled,
    /// A package installed from a configured registry index.
    Registry,
}

impl std::fmt::Display for SourceOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Builtin => "built in",
            Self::Package => "local",
            Self::Bundled => "bundled",
            Self::Registry => "registry",
        })
    }
}

impl SourceOrigin {
    /// What a record without an origin (installed before registries existed) was: a package from disk.
    #[must_use]
    pub const fn installed_default() -> Self {
        Self::Package
    }
}

/// An installed source as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePackageRecord {
    /// The source's name; the key.
    pub name: String,
    /// Where it is in its lifecycle.
    pub state: SourcePackageState,
    /// SHA-256 of the installed component, checked every time it is loaded so an altered file is `unavailable`
    /// rather than run.
    pub digest: String,
    /// The manifest the user agreed to, as installed. The daemon runs on this copy, never on the one beside the
    /// component on disk.
    pub manifest: SourceManifest,
    /// When it was first installed.
    pub installed_at: DateTime<Utc>,
    /// Last change to this record.
    pub updated_at: DateTime<Utc>,
    /// Where it was installed from.
    #[serde(default = "SourceOrigin::installed_default")]
    pub origin: SourceOrigin,
    /// The index it came from, for a bundled or registry source: what `update` asks about a newer version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// SHA-256 of the archive it was installed from, when that is known: what the index published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_digest: Option<String>,
    /// The id of the key whose signature on the archive was verified at install, if there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use SourcePackageEvent::{Disable, Enable};
    use SourcePackageState::{Disabled, Enabled, Installed};

    #[test]
    fn every_state_and_event_pair_either_moves_or_is_refused_as_documented() {
        let table = [
            (Installed, Enable, Some(Enabled)),
            (Installed, Disable, Some(Disabled)),
            (Enabled, Enable, None),
            (Enabled, Disable, Some(Disabled)),
            (Disabled, Enable, Some(Enabled)),
            (Disabled, Disable, None),
        ];
        for (state, event, expected) in table {
            assert_eq!(state.apply(event).ok(), expected, "{state:?} + {event:?}");
        }
    }

    #[test]
    fn a_refused_transition_names_the_state_and_the_event() {
        let error = Enabled.apply(Enable).unwrap_err();
        assert_eq!(error.state, Enabled);
        // The names `source list` shows, not the Rust variant names.
        assert_eq!(
            error.to_string(),
            "a source that is enabled cannot be asked to enable"
        );
    }

    #[test]
    fn permissions_listed_in_another_order_give_the_same_consent_digest() {
        let one = Permissions {
            process: vec!["git".into(), "gh".into()],
            ..Permissions::default()
        };
        let other = Permissions {
            process: vec!["gh".into(), "git".into(), "gh".into()],
            ..Permissions::default()
        };
        assert_eq!(one.consent_digest("x"), other.consent_digest("x"));
    }

    #[test]
    fn consent_to_one_source_or_one_set_of_permissions_is_not_consent_to_another() {
        let none = Permissions::default();
        let network = Permissions {
            network: true,
            ..Permissions::default()
        };
        assert_ne!(none.consent_digest("x"), network.consent_digest("x"));
        assert_ne!(network.consent_digest("x"), network.consent_digest("y"));
    }

    fn oauth() -> OAuthRequirement {
        OAuthRequirement {
            client_id: "client".into(),
            scopes: vec!["write".into(), "read".into()],
            token_url: "https://auth.example.com/token".into(),
            authorize_url: Some("https://auth.example.com/authorize".into()),
            device_authorization_url: None,
        }
    }

    #[test]
    fn an_absent_sign_in_is_not_part_of_what_the_consent_digest_hashes() {
        // A digest that moved would invalidate every consent already given, for sources that ask for no sign-in.
        let permissions = Permissions {
            network: true,
            ..Permissions::default()
        };
        let canonical = serde_json::json!({ "name": "x", "permissions": permissions.normalized() });
        assert!(
            !canonical.to_string().contains("oauth"),
            "an absent sign-in must not appear in what is hashed: {canonical}"
        );
    }

    #[test]
    fn asking_for_a_sign_in_or_a_wider_one_needs_consent_again() {
        let plain = Permissions::default();
        let signed_in = Permissions {
            oauth: Some(oauth()),
            ..Permissions::default()
        };
        assert_ne!(plain.consent_digest("x"), signed_in.consent_digest("x"));
        let mut wider = oauth();
        wider.scopes.push("admin".into());
        let wider = Permissions {
            oauth: Some(wider),
            ..Permissions::default()
        };
        assert_ne!(signed_in.consent_digest("x"), wider.consent_digest("x"));
        let mut reordered = oauth();
        reordered.scopes.reverse();
        let reordered = Permissions {
            oauth: Some(reordered),
            ..Permissions::default()
        };
        assert_eq!(signed_in.consent_digest("x"), reordered.consent_digest("x"));
        assert!(!signed_in.is_empty());
    }

    #[test]
    fn the_fingerprint_follows_what_a_credential_was_granted_under_and_not_the_order_of_scopes() {
        let base = oauth();
        let mut reordered = oauth();
        reordered.scopes.reverse();
        assert_eq!(base.fingerprint(), reordered.fingerprint());
        for change in [
            |o: &mut OAuthRequirement| o.client_id = "other".into(),
            |o: &mut OAuthRequirement| o.scopes.push("admin".into()),
            |o: &mut OAuthRequirement| o.token_url = "https://evil.example.com/token".into(),
            |o: &mut OAuthRequirement| o.authorize_url = None,
        ] {
            let mut changed = oauth();
            change(&mut changed);
            assert_ne!(base.fingerprint(), changed.fingerprint());
        }
    }

    #[test]
    fn an_endpoint_is_https_or_plain_http_to_the_local_machine_and_nothing_else() {
        for ok in [
            "https://auth.example.com/token",
            "http://127.0.0.1:8080/token",
            "http://localhost/token",
            "http://[::1]:9/token",
        ] {
            assert!(is_secure_endpoint(ok), "{ok}");
        }
        for bad in [
            "http://auth.example.com/token",
            "ftp://auth.example.com/token",
            "https://user:pw@auth.example.com/token",
            "http://localhost.evil.example/token",
            "https://",
            "auth.example.com/token",
        ] {
            assert!(!is_secure_endpoint(bad), "{bad}");
        }
    }

    #[test]
    fn a_requirement_with_no_way_to_sign_in_or_an_insecure_endpoint_or_a_spaced_scope_is_refused() {
        assert!(oauth().validate().is_ok());
        type Change = fn(&mut OAuthRequirement);
        let cases: [(Change, &str); 5] = [
            (|o| o.client_id = " ".into(), "client_id"),
            (|o| o.authorize_url = None, "sign in"),
            (
                |o| o.token_url = "http://auth.example.com/token".into(),
                "token_url",
            ),
            (
                |o| o.authorize_url = Some("http://x.example.com/a".into()),
                "authorize_url",
            ),
            (|o| o.scopes.push("two words".into()), "scopes"),
        ];
        for (change, word) in cases {
            let mut requirement = oauth();
            change(&mut requirement);
            let message = requirement.validate().unwrap_err();
            assert!(message.contains(word), "{message}");
        }
    }

    #[test]
    fn a_manifest_with_an_unknown_permission_key_is_refused() {
        let parsed: std::result::Result<Permissions, _> =
            toml::from_str("filesystems = { read = [\"/\"] }");
        assert!(parsed.is_err());
    }

    #[test]
    fn contracts_are_compatible_by_minor_before_one_point_zero() {
        assert!(contract_compatibility("0.3").is_ok());
        assert!(contract_compatibility("0.3.7").is_ok());
        assert!(contract_compatibility("0.1").is_err());
        assert!(contract_compatibility("0.2").is_err());
        assert!(contract_compatibility("0.4").is_err());
        assert!(contract_compatibility("0.0").is_err());
        assert!(contract_compatibility("1.1").is_err());
        assert!(contract_compatibility("nonsense").is_err());
        assert!(contract_compatibility("0").is_err());
    }

    #[test]
    fn describing_no_permissions_says_none_and_describing_some_names_each() {
        assert_eq!(Permissions::default().describe(), "none");
        let described = Permissions {
            filesystem: FilesystemPermissions {
                read: vec!["locator".into()],
            },
            network: true,
            process: vec!["git".into()],
            env: vec!["TOKEN".into()],
            oauth: Some(oauth()),
        }
        .describe();
        for word in [
            "locator",
            "network",
            "git",
            "TOKEN",
            "OAuth",
            "auth.example.com",
            "read",
        ] {
            assert!(described.contains(word), "{described}");
        }
    }

    #[test]
    fn from_one_point_zero_a_source_runs_on_the_same_major_with_at_least_its_minor() {
        // The host implements 2.3.
        let host = (2, 3);
        assert!(contracts_agree(host, (2, 3)));
        assert!(contracts_agree(host, (2, 0)), "an older minor still runs");
        assert!(
            !contracts_agree(host, (2, 4)),
            "a newer minor is not implemented yet"
        );
        assert!(!contracts_agree(host, (1, 3)) && !contracts_agree(host, (3, 3)));
        // Before 1.0 only the identical minor does.
        assert!(contracts_agree((0, 5), (0, 5)));
        assert!(!contracts_agree((0, 5), (0, 4)) && !contracts_agree((0, 5), (1, 5)));
    }

    #[test]
    fn one_policy_decides_whether_a_contract_and_a_requirement_run_on_a_version() {
        let running = semver::Version::new(0, 3, 1);
        let current = CONTRACT_VERSION;
        assert!(version_compatibility(current, ">=0.3, <0.4", &running).is_ok());
        let too_new = version_compatibility(current, ">=0.4", &running).unwrap_err();
        assert!(
            too_new.contains(">=0.4") && too_new.contains("0.3.1"),
            "{too_new}"
        );
        assert!(version_compatibility("9.9", ">=0.1", &running).is_err());
        assert!(
            version_compatibility(current, "lots", &running)
                .unwrap_err()
                .contains("not valid")
        );
        // A release candidate is held to the release's requirement.
        let candidate = semver::Version::parse("0.3.0-rc.1").unwrap();
        assert!(version_compatibility(current, ">=0.3", &candidate).is_ok());
    }

    #[test]
    fn a_source_name_is_a_safe_path_segment_or_it_is_not_a_name() {
        for good in ["a", "claude", "pi-sessions", "x1"] {
            assert!(is_valid_source_name(good), "{good}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "A",
            "../x",
            "a/b",
            "a b",
            &"a".repeat(MAX_SOURCE_NAME_LEN + 1),
        ] {
            assert!(!is_valid_source_name(bad), "{bad}");
        }
    }

    #[test]
    fn a_record_written_before_registries_existed_reads_as_a_package_from_disk() {
        let old = serde_json::json!({
            "name": "x",
            "state": "enabled",
            "digest": "d",
            "manifest": {
                "source": {"name": "x", "version": "1.0.0", "description": "x"},
                "compatibility": {"contract": "0.3", "memcastle": ">=0.1"}
            },
            "installed_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        let record: SourcePackageRecord = serde_json::from_value(old).unwrap();
        assert_eq!(record.origin, SourceOrigin::Package);
        assert_eq!(record.manifest.format, MANIFEST_FORMAT);
        assert!(record.registry.is_none() && record.signed_by.is_none());
    }
}
