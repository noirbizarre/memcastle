//! Invariant 9 (AGENTS.md): source-specific code stays out of the mining pipeline, and adapters stay out of
//! storage.
//!
//! The unified source model only pays off if the pipeline never learns what any one source is: the day
//! `pipeline.rs` names an adapter or reads a file itself, the next adapter needs a second pipeline, which is the
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

/// Every `.rs` file under `dir`, at any depth.
fn rust_files_recursive(dir: &str) -> Vec<String> {
    fn walk(root: &Path, relative: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(root.join(relative)).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = format!("{relative}/{name}");
            if entry.path().is_dir() {
                walk(root, &path, out);
            } else if name.ends_with(".rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root(), dir, &mut files);
    files.sort();
    files
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

/// The source names the pipeline must never mention, in every spelling they appear in:
/// the built-in `directory` adapter and the `pi` and `opencode` sources, which are WebAssembly components now.
const ADAPTER_NAMES: &[&str] = &[
    "\"directory\"",
    "DirectoryAdapter",
    "\"pi\"",
    "\"opencode\"",
];

#[test]
fn the_pipeline_and_the_chunker_name_no_adapter_and_read_no_files() {
    for file in [
        "src/mining/pipeline.rs",
        "src/mining/chunk.rs",
        "src/mining/adapter.rs",
    ] {
        let source = code(file);
        for forbidden in ADAPTER_NAMES.iter().chain(
            [
                "std::fs",
                "tokio::fs",
                "adapters::",
                "File::open",
                "read_dir",
                // Nor does it know that some sources are WebAssembly: a source is a source, whichever it is.
                "wasmtime",
                "WasmAdapter",
                "registry::",
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
    // The domain model knows no source by name either (AGENTS.md invariant 9).
    for forbidden in ADAPTER_NAMES {
        assert!(
            !source.contains(forbidden),
            "src/domain/source.rs mentions `{forbidden}`: the domain model must name no source (AGENTS.md invariant 9)"
        );
    }
}

#[test]
fn adapters_never_reach_storage_or_the_job_machinery() {
    // The WebAssembly host is an adapter too: it runs a source, and a source must never reach the store, however it
    // is implemented. Recursive, so a nested module cannot hide from the check.
    let files = rust_files_recursive("src/mining/adapters")
        .into_iter()
        .chain(rust_files_recursive("src/mining/wasm"));
    for file in files {
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

/// `path`'s code before its `#[cfg(test)]` module: what ships, not what the tests do to set up.
fn shipped_code(path: &str) -> String {
    let source = code(path);
    match source.find("#[cfg(test)]") {
        Some(end) => source[..end].to_string(),
        None => source,
    }
}

#[test]
fn extraction_names_no_source_and_never_writes_what_it_reads() {
    // Extraction consumes what the unified Source model filed (docs/adr/024). It must not learn what any one
    // adapter is, must not write a source's bookkeeping, and must never create, replace or delete a drawer: derived
    // information adds graph records beside canonical memory, it does not rewrite it.
    for file in rust_files("src/extract") {
        let source = shipped_code(&file);
        for forbidden in ADAPTER_NAMES.iter().chain(
            [
                "adapters::",
                "save_source_cursor",
                "save_source_document",
                "get_or_create_source",
                "create_drawer",
                "supersede_drawer",
                "delete_drawer",
                "set_drawer_embedding",
            ]
            .iter(),
        ) {
            assert!(
                !source.contains(forbidden),
                "{file} contains `{forbidden}`; extraction must not name a source or write a drawer"
            );
        }
    }
}

#[test]
fn deduplication_names_no_source_and_touches_no_file_or_source_bookkeeping() {
    // The mining pipeline calls deduplication for every chunk it stores (docs/adr/025), so the one thing it must not
    // do is teach the pipeline where a chunk came from. It compares drawers by what they say, in the store and the
    // graph, and never reads a file or writes a source's cursor or document records.
    for file in rust_files("src/dedup") {
        let source = shipped_code(&file);
        for forbidden in ADAPTER_NAMES.iter().chain(
            [
                "adapters::",
                "std::fs",
                "tokio::fs",
                "File::open",
                "read_dir",
                "save_source_cursor",
                "save_source_document",
                "get_or_create_source",
            ]
            .iter(),
        ) {
            assert!(
                !source.contains(forbidden),
                "{file} contains `{forbidden}`; deduplication must not name a source or read a file"
            );
        }
    }
}

#[test]
fn the_matching_policy_is_pure_domain_code() {
    // Whether two drawers or two entities are the same thing is a domain decision (docs/adr/025): it must stay
    // computable without a database, a runtime or the filesystem, which is also what keeps it unit-testable.
    for file in ["src/domain/fingerprint.rs", "src/domain/resolution.rs"] {
        let source = shipped_code(file);
        for forbidden in [
            "surrealdb",
            "tokio",
            "reqwest",
            "std::fs",
            "std::env",
            "crate::store",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`; the matching policy must stay pure"
            );
        }
    }
}

#[test]
fn only_the_webassembly_host_names_the_runtime() {
    // One module owns the engine, so swapping or sandboxing it again is one place, and nothing else can start
    // running guest code with authority of its own.
    for file in rust_files_recursive("src") {
        if file.starts_with("src/mining/wasm/") {
            continue;
        }
        let source = shipped_code(&file);
        for forbidden in ["wasmtime", "wasmtime_wasi"] {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`: only src/mining/wasm/ may run WebAssembly (AGENTS.md invariant 9)"
            );
        }
    }
}

#[test]
fn source_tooling_never_reaches_storage_or_the_job_machinery() {
    // `memcastle source init|build|test|package` run without a daemon (AGENTS.md invariant 1): the module behind
    // them must not be able to open a palace.
    for file in rust_files_recursive("src/source") {
        let source = shipped_code(&file);
        for forbidden in [
            "crate::store",
            "crate::jobs",
            "SurrealStore",
            "JobContext",
            "surrealdb",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`: source tooling is local and touches neither the store nor the jobs"
            );
        }
    }
}

#[test]
fn the_webassembly_host_grants_nothing_beyond_what_a_manifest_lists() {
    // No ambient authority (AGENTS.md invariant 10): every capability a guest has is built from the manifest's
    // permissions in `host::state`, so the calls that would hand over the daemon's own environment, standard
    // streams, arguments or a writable directory must not exist at all.
    let host = shipped_code("src/mining/wasm/host.rs");
    for forbidden in [
        "inherit_env",
        "inherit_stdio",
        "inherit_stdin",
        "inherit_stdout",
        "inherit_stderr",
        "inherit_args",
        "DirPerms::all",
        "FilePerms::all",
        "DirPerms::MUTATE",
        "FilePerms::WRITE",
        "allow_blocking_current_thread",
    ] {
        assert!(
            !host.contains(forbidden),
            "src/mining/wasm/host.rs mentions `{forbidden}`: a source gets only what its manifest lists"
        );
    }
    // The one network switch is behind the manifest's own flag.
    let lines: Vec<&str> = host.lines().collect();
    let switches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("inherit_network"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(switches.len(), 1, "exactly one place may open the network");
    assert!(
        lines[switches[0].saturating_sub(1)].contains("permissions.network"),
        "the network is opened only inside `if permissions.network`"
    );
}

#[test]
fn source_distribution_reaches_neither_storage_nor_the_job_machinery_nor_the_runtime() {
    // `crate::distribution` (docs/adr/033) fetches and verifies archives and hands back bytes: what to do with them is
    // `crate::app`'s. If it could open a palace or run a component, a registry would be a path to either.
    for file in rust_files_recursive("src/distribution") {
        let source = shipped_code(&file);
        for forbidden in [
            "crate::store",
            "crate::jobs",
            "crate::app",
            "crate::mining",
            "SurrealStore",
            "JobContext",
            "surrealdb",
            "wasmtime",
        ] {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`: distribution fetches and verifies, and decides nothing about installing"
            );
        }
    }
}

#[test]
fn only_the_application_layer_and_configuration_call_into_distribution() {
    // Fetching a package from the network is a daemon-side, administrative act (AGENTS.md, invariants 1 and 10): the
    // CLI asks the daemon over HTTP, and nothing in `api`, `mcp`, `mining` or `client` reaches a registry on its own.
    // `config` may only parse a location, to refuse a bad one at load.
    for file in rust_files_recursive("src") {
        let allowed = file.starts_with("src/distribution/")
            || file.starts_with("src/app/")
            || file == "src/config/mod.rs"
            || file == "src/lib.rs";
        if allowed {
            continue;
        }
        let source = shipped_code(&file);
        assert!(
            !source.contains("crate::distribution") && !source.contains("memcastle::distribution"),
            "{file} reaches the distribution module; only `app` may, so a registry is never fetched from anywhere else"
        );
    }
    for file in ["src/main.rs", "src/cli.rs"] {
        let source = shipped_code(file);
        assert!(
            !source.contains("distribution::"),
            "{file} reaches the distribution module; the CLI asks the daemon, over HTTP"
        );
    }
}

#[test]
fn local_source_tooling_and_adapters_never_open_the_network_themselves() {
    // `memcastle source init|build|test|package|index|keygen` run offline on a developer's machine, and an adapter
    // reaches the outside world only through the sandbox its manifest declares (invariant 10). A second HTTP client in
    // either would be an unreviewed network path.
    for dir in ["src/source", "src/mining"] {
        for file in rust_files_recursive(dir) {
            let source = shipped_code(&file);
            for forbidden in ["reqwest", "tokio::net", "std::net::TcpStream"] {
                assert!(
                    !source.contains(forbidden),
                    "{file} mentions `{forbidden}`: only `distribution` and the HTTP providers talk to the network"
                );
            }
        }
    }
}

#[test]
fn every_package_arriving_from_a_registry_is_checked_against_the_trust_policy() {
    // The signature check is one call in `Registry::fetch`; a refactor that drops it would leave every other test
    // passing against an index that happens to be honest, so the call itself is pinned (the behaviour is held by
    // `tests/wasm_registry.rs`).
    let fetch = shipped_code("src/distribution/mod.rs");
    assert!(
        fetch.contains("policy.check(name, &archive, entry.signature.as_ref())"),
        "`Registry::fetch` must apply the trust policy to every archive: nothing a registry serves is exempt"
    );
    assert!(
        fetch.contains("archive_digest != entry.sha256"),
        "`Registry::fetch` must compare the archive's digest with the index's before anything reads it"
    );
}

#[test]
fn the_bundle_is_read_from_disk_and_reaches_neither_storage_nor_the_job_machinery_nor_the_network()
{
    // A bundled source is installed from the start and run in place (docs/adr/039): finding and reading the release's own
    // packages is a directory read. If it could open a palace or a connection, "shipped with MemCastle" would no longer
    // be what makes it trusted without a signature.
    let source = shipped_code("src/mining/bundled.rs");
    for forbidden in [
        "crate::store",
        "crate::jobs",
        "crate::distribution",
        "SurrealStore",
        "JobContext",
        "surrealdb",
        "reqwest",
        "wasmtime",
    ] {
        assert!(
            !source.contains(forbidden),
            "src/mining/bundled.rs mentions `{forbidden}`: the bundle is read from disk and nothing else"
        );
    }
}

#[test]
fn a_registry_archive_never_skips_the_trust_policy_because_of_where_it_came_from() {
    // The one exemption from the trust policy used to be an index that shipped beside the binary. The bundle is not an
    // index any more, so `Registry::fetch` must not know an origin at all: there is no kind of archive to exempt.
    let fetch = shipped_code("src/distribution/mod.rs");
    assert!(
        !fetch.contains("SourceOrigin"),
        "src/distribution/mod.rs names an origin; every archive it fetches is held to the same policy"
    );
}
