//! Invariants 1, 6, 10 and 12 (AGENTS.md) for OAuth credentials (docs/adr/039): the sign-in, the stored tokens and their
//! renewal live in one module that knows nothing of the palace, the jobs or the WebAssembly runtime, that only the
//! application layer and the daemon's composition root can reach, and that no interface a remote caller uses can name.
//!
//! These checks read the source text, the same way the `prek` architecture hooks do, because a rule nothing checks is
//! only a comment.

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
    match code.find("#[cfg(test)]") {
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

fn relative(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn the_credential_module_reaches_no_palace_no_jobs_no_runtime_and_no_interface() {
    let files = rust_files("src/credential");
    assert!(!files.is_empty());
    for file in files {
        let source = shipped_code(&file);
        for forbidden in [
            "crate::store",
            "crate::jobs",
            "crate::mining",
            "crate::source",
            "crate::distribution",
            "crate::client",
            "crate::app",
            "crate::api",
            "crate::mcp",
            "crate::server",
            "crate::dbadmin",
            "surrealdb",
            "wasmtime",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} reaches `{forbidden}`: the credential module knows only the domain, the configuration and the error type",
                relative(&file)
            );
        }
    }
}

#[test]
fn only_the_application_layer_and_the_daemons_root_name_the_credential_module() {
    for file in rust_files("src") {
        let name = relative(&file);
        let allowed = name.starts_with("src/app/")
            || name.starts_with("src/credential/")
            || name == "src/server/mod.rs"
            || name == "src/lib.rs";
        if allowed {
            continue;
        }
        let source = shipped_code(&file);
        // `memcastle::credential::<kind>` is also how a diagnostic code is spelt, so only a path into the module counts.
        for forbidden in [
            "crate::credential",
            "use memcastle::credential",
            "memcastle::credential::Credentials",
            "memcastle::credential::store::",
            "memcastle::credential::oauth::",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} names `{forbidden}`: the CLI, MCP, REST, mining and jobs reach credentials only through `app`, or through the `AccessTokens` trait"
            );
        }
    }
}

#[test]
fn the_command_line_never_names_the_credential_module() {
    // The rule above, held to the two files that are the command line by their name, so that loosening it for a layer
    // cannot loosen it for them.
    for file in ["src/main.rs", "src/cli.rs"] {
        let source = shipped_code(&root().join(file));
        assert!(
            !source.contains("credential::"),
            "{file} names the credential module: `memcastle source auth` is an HTTP call to the daemon"
        );
    }
}

#[test]
fn a_token_leaves_the_credential_module_only_where_it_is_stored_or_sent_to_its_provider() {
    // `Secret::expose` is the one way to read a token; where it is called is where a token could leak, so each is named.
    let allowed = ["src/credential/mod.rs", "src/credential/oauth.rs"];
    for file in rust_files("src/credential") {
        let name = relative(&file);
        if name.ends_with("/tests.rs") || name.ends_with("/fake.rs") {
            continue;
        }
        let source = shipped_code(&file);
        if source.contains(".expose()") {
            assert!(
                allowed.contains(&name.as_str()),
                "{name} reads a token with `expose()`; only {allowed:?} may, to store it or to send it to the provider"
            );
        }
    }
}

#[test]
fn nothing_logs_or_formats_a_token_in_the_credential_module() {
    for file in rust_files("src/credential") {
        let name = relative(&file);
        if name.ends_with("/tests.rs") || name.ends_with("/fake.rs") {
            continue;
        }
        let source = shipped_code(&file);
        for line in source.lines() {
            let logs = [
                "tracing::",
                "info!(",
                "debug!(",
                "warn!(",
                "error!(",
                "trace!(",
                "println!(",
                "eprintln!(",
            ]
            .iter()
            .any(|macro_| line.contains(macro_));
            if logs {
                for word in [
                    "refresh_token",
                    "access_token",
                    "device_code",
                    "verifier",
                    "tokens",
                ] {
                    assert!(
                        !line.contains(word),
                        "{name} logs something named `{word}`: {line}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_mining_runtime_and_the_jobs_see_sign_in_only_as_the_access_tokens_trait() {
    for dir in ["src/mining", "src/jobs"] {
        for file in rust_files(dir) {
            let source = shipped_code(&file);
            for forbidden in [
                "credential::Credentials",
                "credential::store",
                "credential::oauth",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{} names `{forbidden}`; it may use `domain::AccessTokens` and nothing else of sign-in",
                    relative(&file)
                );
            }
        }
    }
}
