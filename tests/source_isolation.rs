//! Invariant 9 (AGENTS.md): source-specific code stays out of the mining pipeline, and adapters stay out of
//! storage.
//!
//! The unified source model only pays off if the pipeline never learns what any one source is: the day
//! `pipeline.rs` names a provider or reads a file itself, the next adapter needs a second pipeline, which is the
//! incompatible second abstraction #85 exists to prevent. These checks read the source text, the same way the
//! `prek` architecture hooks do, because a rule nothing checks is only a comment.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `path`'s source with comment lines removed, so prose may mention what the code must not do.
fn code(path: &str) -> String {
    let text = std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file directly under `dir`.
fn rust_files(dir: &str) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(root().join(dir))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .map(|path| format!("{dir}/{}", path.file_name().unwrap().to_string_lossy()))
        .collect();
    files.sort();
    files
}

/// The provider names the pipeline must never mention, in every spelling they appear in.
const PROVIDER_NAMES: &[&str] = &[
    "pi-sessions",
    "pi_sessions",
    "\"directory\"",
    "DirectoryAdapter",
    "PiSessions",
];

#[test]
fn the_pipeline_and_the_chunker_name_no_provider_and_read_no_files() {
    for file in [
        "src/mining/pipeline.rs",
        "src/mining/chunk.rs",
        "src/mining/adapter.rs",
    ] {
        let source = code(file);
        for forbidden in PROVIDER_NAMES.iter().chain(
            [
                "std::fs",
                "tokio::fs",
                "adapters::",
                "File::open",
                "read_dir",
            ]
            .iter(),
        ) {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`: source-specific discovery and reading belong in an adapter under \
                 src/mining/adapters/, never in the pipeline (AGENTS.md invariant 9)"
            );
        }
    }
}

#[test]
fn the_source_model_in_the_domain_does_no_io() {
    let source = code("src/domain/source.rs");
    for forbidden in ["std::fs", "tokio", "surrealdb", "reqwest", "std::env"] {
        assert!(
            !source.contains(forbidden),
            "src/domain/source.rs mentions `{forbidden}`: the domain is pure types (AGENTS.md layout)"
        );
    }
}

#[test]
fn adapters_never_reach_storage_or_the_job_machinery() {
    for file in rust_files("src/mining/adapters") {
        let source = code(&file);
        for forbidden in [
            "crate::store",
            "crate::jobs",
            "SurrealStore",
            "JobContext",
            "surrealdb",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`: an adapter finds and reads documents, and the pipeline persists them"
            );
        }
    }
}

#[test]
fn the_pipeline_is_the_only_writer_of_source_records() {
    // Anything else that wrote them would bypass the commit order (drawers, then document, then cursor) that keeps
    // the cursor from running ahead of the data.
    let allowed = ["src/mining/pipeline.rs", "src/store/sources.rs"];
    for dir in [
        "src/mining",
        "src/mining/adapters",
        "src/app",
        "src/api",
        "src/mcp",
        "src/client",
        "src/jobs",
    ] {
        for file in rust_files(dir) {
            if allowed.contains(&file.as_str()) || Path::new(&file).ends_with("mining/mod.rs") {
                continue;
            }
            let source = code(&file);
            for writer in [
                "save_source_cursor",
                "save_source_document",
                "get_or_create_source",
            ] {
                assert!(
                    !source.contains(writer),
                    "{file} calls `{writer}`; only the mining pipeline writes sources"
                );
            }
        }
    }
}
