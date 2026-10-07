//! Invariant 13 (AGENTS.md) for source triggers (docs/adr/043): a trigger only decides *when* to ask for a mining run,
//! so the module that runs them knows nothing of the palace, the jobs or any source; only the application layer and the
//! daemon's composition root can reach it; no interface a remote caller uses can start, change or fire one; and the
//! webhook listener never keeps or logs what it was sent.
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

/// `code` with the text of every string literal removed, so a rule about identifiers is not tripped by a message.
fn without_strings(code: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match (in_string, c) {
            (true, '\\') => {
                chars.next();
            }
            (_, '"') => in_string = !in_string,
            (false, c) => out.push(c),
            (true, _) => {}
        }
    }
    out
}

#[test]
fn the_trigger_module_reaches_no_palace_no_jobs_no_source_no_credential_and_no_interface() {
    let files = rust_files("src/trigger");
    assert!(!files.is_empty());
    for file in files {
        let source = shipped_code(&file);
        for forbidden in [
            "crate::store",
            "crate::jobs",
            "crate::mining",
            "crate::source",
            "crate::distribution",
            "crate::credential",
            "crate::client",
            "crate::app",
            "crate::api",
            "crate::mcp",
            "crate::dbadmin",
            "surrealdb",
            "wasmtime",
            "\"directory\"",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} reaches `{forbidden}`: the trigger module knows only the domain and what the host hands it",
                relative(&file)
            );
        }
    }
}

#[test]
fn only_the_application_layer_and_the_daemons_root_name_the_trigger_module() {
    for file in rust_files("src") {
        let name = relative(&file);
        let allowed = name.starts_with("src/app/")
            || name.starts_with("src/trigger/")
            || name == "src/server/mod.rs"
            || name == "src/lib.rs";
        if allowed {
            continue;
        }
        let source = shipped_code(&file);
        // `memcastle::trigger::<kind>` is also how a diagnostic code is spelt, so only a path into the module counts.
        for forbidden in [
            "crate::trigger",
            "use memcastle::trigger",
            "memcastle::trigger::Supervisor",
            "memcastle::trigger::Host",
            "memcastle::trigger::Runtime",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} names `{forbidden}`: the CLI, MCP, REST, mining and jobs reach triggers only through `app`"
            );
        }
    }
}

#[test]
fn no_remote_interface_can_start_change_or_fire_a_trigger_except_the_rest_routes_and_the_cli() {
    // MCP reads triggers and nothing else: what runs unattended is the user's decision, like what a miner mines.
    let mcp = shipped_code(&root().join("src/mcp/mod.rs"));
    for forbidden in [
        "set_trigger",
        "set_trigger_enabled",
        "remove_trigger",
        "reload_triggers",
        "fire_trigger",
        "recover_triggers",
    ] {
        assert!(
            !mcp.contains(forbidden),
            "src/mcp/mod.rs calls `{forbidden}`: MCP may only read triggers"
        );
    }
    // The command line is an HTTP client, like every other command (invariant 1).
    for file in ["src/main.rs", "src/cli.rs"] {
        let source = shipped_code(&root().join(file));
        assert!(
            !source.contains("memcastle::trigger") && !source.contains("memcastle::app::Trigger"),
            "{file} names the trigger module or its services: `memcastle trigger` is an HTTP call to the daemon"
        );
    }
}

#[test]
fn the_main_router_serves_no_webhook_delivery_and_the_public_routes_are_unchanged() {
    // A delivery goes to the separate, opt-in listener, so every route on the main router stays behind the
    // authentication layer (invariant 6): no `/hooks` route, and `is_public` knows nothing of triggers.
    for file in rust_files("src/api") {
        let source = shipped_code(&file);
        assert!(
            !source.contains("/hooks"),
            "{} serves a webhook route: deliveries belong to the trigger listener, never the API's router",
            relative(&file)
        );
    }
    let auth = shipped_code(&root().join("src/api/auth.rs"));
    assert!(!auth.contains("trigger") && !auth.contains("hook"));
}

#[test]
fn the_webhook_listener_never_logs_or_keeps_what_it_was_sent() {
    let code = shipped_code(&root().join("src/trigger/webhook.rs"));
    // Every tracing call, up to the end of its statement, with the message text removed: what is left names the values
    // it records, and none of them may be the body, the headers or the secret.
    for macro_call in ["debug!(", "warn!(", "info!(", "error!(", "trace!("] {
        for segment in code.split(macro_call).skip(1) {
            let statement = without_strings(segment.split(';').next().unwrap_or(""));
            for forbidden in ["body", "headers", "secret", "signature", "presented"] {
                assert!(
                    !statement.contains(forbidden),
                    "a `{macro_call}` call records `{forbidden}`: a delivery's content and proof are never logged"
                );
            }
        }
    }
    // Compared in constant time, and never with `==`.
    assert!(code.contains("verify_slice") && code.contains("ct_eq"));
    // Nothing the listener reads is handed to the host: a request for a run carries a trigger and a delivery id only.
    for segment in code.split("FireRequest {").skip(1) {
        let request = segment.split('}').next().unwrap_or("");
        assert!(
            !request.contains("body"),
            "the body must not travel past the signature check"
        );
    }
}

#[test]
fn a_trigger_asks_for_a_run_only_through_the_one_path_a_person_uses() {
    // `request_trigger_run` is the one place a trigger's request becomes a job, and it goes through the same function
    // `memcastle miner run` does (`request_miner_run`), so a trigger can cause nothing a person could not ask for.
    let triggers = shipped_code(&root().join("src/app/triggers.rs"));
    assert!(triggers.contains("request_miner_run"));
    assert!(
        !triggers.contains("submit_mine") && !triggers.contains("scheduler.submit"),
        "a trigger must not queue a job by any path of its own"
    );
}
