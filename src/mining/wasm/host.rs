//! What a WebAssembly source is given to run with: the generated bindings, the per-call store state, and the host
//! functions.
//!
//! Every call into a source gets a fresh `Store`, built from the manifest's permissions and nothing else: no
//! preopened directory, environment variable, socket or program exists unless it was granted. A fresh store per call
//! is also what keeps sources stateless (docs/adr/026): nothing a call leaves behind is visible to the next.

use std::path::PathBuf;
use std::time::Duration;

use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::domain::Permissions;

use super::process::{self, ProcessError, ProcessGrant};

wasmtime::component::bindgen!({
    path: "wit/memcastle-source.wit",
    world: "source",
});

/// The state of one call into a source.
pub(super) struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    /// The memory ceiling, enforced by the store.
    pub limits: StoreLimits,
    /// What `run-process` may do.
    process: ProcessGrant,
    /// The first permission the source asked for and was refused, so that a failure that follows can be reported as
    /// a permission problem rather than as whatever error the source made of it.
    pub denied: Option<String>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// The shared types interface has no functions, but the generated linker still asks for the (empty) trait.
impl memcastle::source::types::Host for HostState {}

impl memcastle::source::host::Host for HostState {
    fn run_process(
        &mut self,
        program: String,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> Result<memcastle::source::host::ProcessOutput, String> {
        match process::run(&self.process, &program, &args, stdin.as_deref()) {
            Ok(output) => Ok(memcastle::source::host::ProcessOutput {
                status: output.status,
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            Err(ProcessError::Denied(message)) => {
                self.denied.get_or_insert_with(|| message.clone());
                Err(message)
            }
            Err(ProcessError::Failed(message)) => Err(message),
        }
    }
}

/// The directories a source may read, given what the manifest asked for and the place being mined.
///
/// `locator` is the one directory the source is asked to mine; `~/` is the home directory. Entries that do not exist
/// are dropped, so a source asking for `~/.pi/agent/sessions` on a machine without Pi simply sees no such directory.
pub(super) fn readable_directories(
    permissions: &Permissions,
    locator: Option<&str>,
) -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    for entry in &permissions.filesystem.read {
        let path = if entry == "locator" {
            locator.map(PathBuf::from)
        } else if let Some(rest) = entry.strip_prefix("~/") {
            dirs::home_dir().map(|home| home.join(rest))
        } else {
            Some(PathBuf::from(entry))
        };
        let Some(path) = path else { continue };
        // Canonical, so a symlink in the granted path cannot name something other than what is shown at consent.
        let Ok(path) = std::fs::canonicalize(&path) else {
            continue;
        };
        if path.is_dir() && !directories.contains(&path) {
            directories.push(path);
        }
    }
    directories
}

/// A call's store state, with exactly `permissions` granted (or none, for `normalize`, which is pure by contract).
///
/// # Errors
///
/// A message when a granted directory cannot be opened.
pub(super) fn state(
    permissions: &Permissions,
    locator: Option<&str>,
    memory_bytes: usize,
    timeout: Duration,
) -> Result<HostState, String> {
    let mut wasi = WasiCtxBuilder::new();
    for directory in readable_directories(permissions, locator) {
        // Guest path = host path, so a source written against ordinary paths (`std::fs` on the locator it was
        // given) works unchanged and the same code runs natively. Read-only: the host offers no write access.
        let guest = directory.display().to_string();
        wasi.preopened_dir(&directory, &guest, DirPerms::READ, FilePerms::READ)
            .map_err(|source| format!("cannot open `{guest}` for the source: {source}"))?;
    }
    let mut env = Vec::new();
    for name in &permissions.env {
        if let Ok(value) = std::env::var(name) {
            wasi.env(name, &value);
            env.push((name.clone(), value));
        }
    }
    if permissions.network {
        wasi.inherit_network().allow_ip_name_lookup(true);
    }
    Ok(HostState {
        wasi: wasi.build(),
        table: ResourceTable::new(),
        limits: wasmtime::StoreLimitsBuilder::new()
            .memory_size(memory_bytes)
            .trap_on_grow_failure(true)
            .build(),
        process: ProcessGrant {
            allowed: permissions.process.clone(),
            env,
            timeout,
        },
        denied: None,
    })
}

/// Whether `path` is under one of the granted directories; used by tests and documentation of the grant.
#[cfg(test)]
pub(super) fn is_granted(directories: &[PathBuf], path: &std::path::Path) -> bool {
    directories
        .iter()
        .any(|directory| path.starts_with(directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FilesystemPermissions;

    fn reading(entries: &[&str]) -> Permissions {
        Permissions {
            filesystem: FilesystemPermissions {
                read: entries.iter().map(|s| (*s).to_string()).collect(),
            },
            ..Permissions::default()
        }
    }

    #[test]
    fn the_locator_grant_opens_the_directory_being_mined_and_nothing_else() {
        let mined = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let directories = readable_directories(&reading(&["locator"]), mined.path().to_str());

        assert_eq!(directories.len(), 1);
        assert!(is_granted(
            &directories,
            &mined.path().canonicalize().unwrap()
        ));
        assert!(!is_granted(
            &directories,
            &other.path().canonicalize().unwrap()
        ));
    }

    #[test]
    fn nothing_is_opened_without_a_filesystem_grant() {
        let mined = tempfile::tempdir().unwrap();
        assert!(readable_directories(&Permissions::default(), mined.path().to_str()).is_empty());
    }

    #[test]
    fn a_granted_directory_that_does_not_exist_is_dropped_rather_than_an_error() {
        assert!(readable_directories(&reading(&["/definitely/not/here"]), None).is_empty());
        assert!(readable_directories(&reading(&["locator"]), None).is_empty());
    }
}
