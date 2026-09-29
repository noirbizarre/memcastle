//! The daemon registry file: operational metadata for discovery, never the
//! source of truth for "is a daemon running" (see the architecture doc's
//! daemon-lifecycle section — that question is always answered by a live
//! HTTP request, e.g. `GET /api/health`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// What a running daemon records about itself, and what `client` reads back
/// to find it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    /// The daemon process's PID — a liveness *hint*, checked before trusting
    /// the rest of this file, never trusted on its own (a PID can be
    /// reused).
    pub pid: u32,
    /// The address its HTTP API/MCP listener is bound to.
    pub bind_addr: String,
    /// When it started, as RFC3339.
    pub started_at: String,
    /// The daemon binary's version.
    pub version: String,
}

/// Where the registry file for `palace_path` lives:
/// `$XDG_STATE_HOME/memcastle/run/<sha256(canonical path)[..16]>/daemon.json`
/// (`~/.local/state/memcastle/run/...` by default). Keyed by the palace's
/// canonical path (not its configured, possibly-relative one) so two configs
/// that name the same directory differently still agree on one file.
/// It lives in the state directory, not beside the palace, because it is
/// runtime metadata rather than palace data: copying or backing up a palace
/// must not carry a stale daemon record along.
#[must_use]
pub fn registry_path(palace_path: &Path) -> PathBuf {
    let canonical =
        std::fs::canonicalize(palace_path).unwrap_or_else(|_| palace_path.to_path_buf());
    let digest = crate::domain::sha256_hex(canonical.display().to_string().as_bytes());
    let short = &digest[..16];

    crate::config::paths::run_dir()
        .join(short)
        .join("daemon.json")
}

/// Write the registry file, creating its parent directory if needed.
///
/// # Errors
///
/// Returns [`Error::Io`] if the directory or file cannot be written.
pub fn write(palace_path: &Path, info: &RuntimeInfo) -> Result<()> {
    let path = registry_path(palace_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|source| Error::io(parent.display().to_string(), source))?;
    }
    let json = serde_json::to_string_pretty(info)
        .map_err(|source| Error::serialization("the daemon registry file", source))?;
    std::fs::write(&path, json).map_err(|source| Error::io(path.display().to_string(), source))
}

/// Remove the registry file. Not an error if it's already gone.
pub fn remove(palace_path: &Path) {
    let path = registry_path(palace_path);
    let _ = std::fs::remove_file(path);
}

/// Read the registry file for `palace_path`, if one exists and its PID
/// still corresponds to a live process. Liveness is checked with a signal-0
/// probe on Unix; on other platforms the file's mere existence is trusted
/// (a stale file there is a rarer, lower-stakes failure mode than a
/// deliberately-conservative daemon-discovery bug).
#[must_use]
pub fn read_if_live(palace_path: &Path) -> Option<RuntimeInfo> {
    match inspect(palace_path) {
        Registry::Live(info) => Some(info),
        Registry::Absent | Registry::Stale(_) => None,
    }
}

/// What the registry file for a palace says right now.
#[derive(Debug, Clone)]
pub enum Registry {
    /// No readable registry file: no daemon has registered, or it stopped cleanly.
    Absent,
    /// A file whose PID is a live process.
    Live(RuntimeInfo),
    /// A file whose PID is dead: the daemon was killed without cleaning up.
    /// Reported by `status` so a crash is diagnosed rather than looking like
    /// a daemon that never ran.
    Stale(RuntimeInfo),
}

impl Registry {
    /// Short name for scripts: `absent`, `live` or `stale`.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Live(_) => "live",
            Self::Stale(_) => "stale",
        }
    }

    /// The recorded runtime info, whether or not its process is alive.
    #[must_use]
    pub fn info(&self) -> Option<&RuntimeInfo> {
        match self {
            Self::Absent => None,
            Self::Live(info) | Self::Stale(info) => Some(info),
        }
    }
}

/// Classify the registry file for `palace_path`. An unreadable or malformed
/// file counts as absent: it cannot be trusted to name an address or a PID.
#[must_use]
pub fn inspect(palace_path: &Path) -> Registry {
    let path = registry_path(palace_path);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Registry::Absent;
    };
    let Ok(info) = serde_json::from_str::<RuntimeInfo>(&text) else {
        return Registry::Absent;
    };
    if is_alive(info.pid) {
        Registry::Live(info)
    } else {
        Registry::Stale(info)
    }
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // Signal 0: no signal is actually sent, only existence/permission is
    // checked — the standard, side-effect-free liveness probe.
    // SAFETY: signal 0 performs no action on the target process; it only
    // reports whether it could be signalled, which is exactly the existence
    // check this function makes.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn is_alive(_pid: u32) -> bool {
    true
}
