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
pub const CONTRACT_VERSION: &str = "0.1.0";

/// What a source package declares about itself: `memcastle-source.toml`.
///
/// Every table refuses unknown keys, so a misspelt permission is an error at build time rather than a source that
/// silently runs without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
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
    /// The provider name: lowercase letters, digits and `-`.
    pub name: String,
    /// The package's own semantic version.
    pub version: String,
    /// One line saying what the source reads.
    pub description: String,
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
        }
    }

    /// Whether the source asks for nothing at all, so there is nothing to consent to.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.filesystem.read.is_empty()
            && !self.network
            && self.process.is_empty()
            && self.env.is_empty()
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

/// An event the source's state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageTransitionError {
    /// Where the source was.
    pub state: SourcePackageState,
    /// What was attempted.
    pub event: SourcePackageEvent,
}

impl std::fmt::Display for PackageTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a {:?} source cannot {:?}", self.state, self.event)
    }
}

impl std::error::Error for PackageTransitionError {}

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
        assert!(error.to_string().contains("Enabled"));
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

    #[test]
    fn a_manifest_with_an_unknown_permission_key_is_refused() {
        let parsed: std::result::Result<Permissions, _> =
            toml::from_str("filesystems = { read = [\"/\"] }");
        assert!(parsed.is_err());
    }

    #[test]
    fn contracts_are_compatible_by_minor_before_one_point_zero() {
        assert!(contract_compatibility("0.1").is_ok());
        assert!(contract_compatibility("0.1.7").is_ok());
        assert!(contract_compatibility("0.2").is_err());
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
        }
        .describe();
        for word in ["locator", "network", "git", "TOKEN"] {
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
}
