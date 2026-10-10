//! The plugin distribution and package readers cannot become a second installer or storage writer.

use std::path::Path;

fn shipped(path: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join(path)).unwrap();
    text.lines()
        .take_while(|line| !line.starts_with("#[cfg(test)]"))
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn plugin_package_and_distribution_do_not_open_a_palace_run_a_job_or_register_an_agent() {
    for file in [
        "src/plugin/manifest.rs",
        "src/plugin/package.rs",
        "src/plugin/signing.rs",
        "src/distribution/plugin.rs",
    ] {
        let source = shipped(file);
        for forbidden in [
            "crate::store",
            "crate::jobs",
            "crate::client",
            "SurrealStore",
            "crate::integration::install",
            "crate::integration::command",
        ] {
            assert!(!source.contains(forbidden), "{file} reaches {forbidden}");
        }
    }
}

#[test]
fn plugin_package_never_fetches_code_and_only_the_application_installs_what_distribution_returns() {
    let package = shipped("src/plugin/package.rs");
    for forbidden in [
        "reqwest",
        "TcpStream",
        "crate::distribution",
        "tokio::process",
        "std::process::Command",
    ] {
        assert!(
            !package.contains(forbidden),
            "plugin package parser reaches {forbidden}"
        );
    }
    let api = shipped("src/api/plugins.rs");
    assert!(!api.contains("crate::store") && !api.contains("crate::integration"));
    let mcp = shipped("src/mcp/mod.rs");
    assert!(!mcp.contains("install_plugin") && !mcp.contains("uninstall_plugin"));
}

#[test]
fn plugin_distribution_and_ownership_changes_only_enter_through_app_services() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for folder in [
        "src/api",
        "src/mcp",
        "src/client",
        "src/mining",
        "src/source",
        "src/integration",
        "src/trigger",
    ] {
        let mut pending = vec![root.join(folder)];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
                let source = shipped(relative);
                for forbidden in [
                    "crate::distribution::plugin",
                    "crate::store::plugins",
                    ".publish_plugin_with_sources(",
                    ".delete_plugin_with_sources(",
                ] {
                    assert!(
                        !source.contains(forbidden),
                        "{relative} bypasses app with {forbidden}"
                    );
                }
            }
        }
    }
}
