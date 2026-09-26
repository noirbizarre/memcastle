//! The daemon registry file: operational metadata for discovery, never the
//! source of truth for "is a daemon running" (see the architecture doc's
//! daemon-lifecycle section — that question is always answered by a live
//! HTTP request, e.g. `GET /api/health`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
/// `~/.memcastle/run/<sha256(canonical path)[..16]>/daemon.json`. Keyed by
/// the palace's canonical path (not its configured, possibly-relative one)
/// so two configs that name the same directory differently still agree on
/// one file.
#[must_use]
pub fn registry_path(palace_path: &Path) -> PathBuf {
    let canonical =
        std::fs::canonicalize(palace_path).unwrap_or_else(|_| palace_path.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.display().to_string().as_bytes());
    // `finalize()` returns a fixed-size byte array, not something that
    // implements `LowerHex` directly — encode by hand rather than pull in a
    // dependency just for this.
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let short = &digest[..16];

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".memcastle")
        .join("run")
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
        .map_err(|source| Error::store_malformed(source.to_string()))?;
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
    let path = registry_path(palace_path);
    let text = std::fs::read_to_string(path).ok()?;
    let info: RuntimeInfo = serde_json::from_str(&text).ok()?;
    if is_alive(info.pid) { Some(info) } else { None }
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
