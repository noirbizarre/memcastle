//! Invariant 1 and 8 (AGENTS.md) for the integration installer: it is local tooling over files and the agents' own
//! commands, and it is not a way for anything remote to change what an agent runs.
//!
//! `memcastle integration` works with no daemon, no palace and no network (docs/adr/034), so the module behind it must
//! not be able to open a palace, reach the daemon or fetch anything, and nothing a remote caller can reach (MCP, REST)
//! may call it. These checks read the source text, the same way the `prek` architecture hooks do, because a rule
//! nothing checks is only a comment.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `path`'s code with comment lines removed, up to its `#[cfg(test)]` module: what ships.
fn shipped_code(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let code = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    // The test module is gated `#[cfg(test)]`, or `#[cfg(all(test, unix))]` where it needs the Unix-only fake agents.
    match ["#[cfg(test)]", "#[cfg(all(test"]
        .iter()
        .filter_map(|marker| code.find(marker))
        .min()
    {
        Some(end) => code[..end].to_string(),
        None => code,
    }
}

fn rust_files(dir: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root().join(dir)];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn the_installer_reaches_no_storage_no_jobs_no_daemon_and_no_network() {
    for file in rust_files("src/integration") {
        let source = shipped_code(&file);
        for forbidden in [
            // Invariant 1: it works before a daemon has ever run, so it cannot open a palace or call one.
            "crate::store",
            "crate::jobs",
            "crate::client",
            "crate::app",
            "SurrealStore",
            "JobContext",
            "surrealdb",
            // And it fetches nothing: everything it installs is already on this machine.
            "reqwest",
            "TcpStream",
            "hyper::",
            "crate::distribution",
            // Invariant 8's other forbidden reach.
            "/api/db",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} mentions `{forbidden}`: the installer is local file tooling",
                file.display()
            );
        }
    }
}

#[test]
fn nothing_remote_can_install_an_integration() {
    // Installing one changes what code an agent runs, so it is a local administrative act: no MCP tool and no REST
    // route may reach the installer, and the application layer they call into does not know it exists.
    for dir in ["src/mcp", "src/api", "src/app", "src/server"] {
        for file in rust_files(dir) {
            let source = shipped_code(&file);
            for forbidden in ["crate::integration", "memcastle::integration"] {
                assert!(
                    !source.contains(forbidden),
                    "{} calls the integration installer; it is reachable from the CLI only",
                    file.display()
                );
            }
        }
    }
}

#[test]
fn the_installer_never_writes_a_credential_or_an_agents_settings_file() {
    // The token is the user's to supply, and an `mcp.memcastle` entry would show every tool twice next to the plugin.
    // Registration is Pi's own commands and one marked OpenCode plugin file: no settings file of either is opened.
    for file in rust_files("src/integration") {
        let source = shipped_code(&file);
        for forbidden in [
            "AUTH_TOKEN",
            "auth.token",
            "Secret",
            "opencode.json",
            "opencode.jsonc",
            "settings.json",
            "mcp.memcastle",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} mentions `{forbidden}`: the installer leaves agent settings and credentials alone",
                file.display()
            );
        }
    }
}

#[test]
fn only_the_binary_calls_the_installer() {
    // The library modules other than `integration` itself have no business with it, so a route added later cannot
    // quietly start installing.
    for file in rust_files("src") {
        let relative = file
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            // Forward slashes whatever the platform, so the prefixes below match on Windows too.
            .replace('\\', "/");
        if relative.starts_with("src/integration/")
            || relative == "src/lib.rs"
            || relative == "src/main.rs"
        {
            continue;
        }
        let source = shipped_code(&file);
        assert!(
            !source.contains("crate::integration"),
            "{relative} calls the integration installer; only the binary's `integration` command may"
        );
    }
}
