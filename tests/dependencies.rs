//! The release binary's storage footprint, checked against `Cargo.lock`.
//!
//! ADR-001 makes SurrealKV the only embedded engine. Nothing stops a feature
//! flag on `surrealdb` or `surrealkit` from quietly pulling RocksDB (a C++
//! build, and tens of megabytes) into every release, so the lockfile is the
//! place to catch it: it lists everything any build of this repository could
//! link, and is what release builds are pinned to with `--locked`.

use std::collections::BTreeSet;

/// Storage engines a SurrealDB feature flag could drag in, none of which a
/// MemCastle release may link.
const FORBIDDEN: &[&str] = &[
    "rocksdb",
    "librocksdb-sys",
    "tikv-client",
    "foundationdb",
    "indxdb",
    "speedb",
];

/// The SurrealDB engine crates the lockfile may contain: SurrealKV for the
/// palace, the in-memory engine for tests (a dev-dependency), and the crate
/// that dispatches between them by URL scheme.
const ALLOWED_ENGINES: &[&str] = &[
    "surrealdb-kvs-surrealkv",
    "surrealdb-kvs-mem",
    "surrealdb-kvs-any",
];

/// The package names in a `Cargo.lock`.
fn package_names(lock: &str) -> BTreeSet<String> {
    let lock: toml::Table = lock.parse().expect("Cargo.lock is TOML");
    lock.get("package")
        .and_then(toml::Value::as_array)
        .expect("Cargo.lock lists packages")
        .iter()
        .filter_map(|package| package.get("name")?.as_str().map(str::to_owned))
        .collect()
}

/// Every reason `names` is not an acceptable SurrealKV-only dependency set.
fn storage_violations(names: &BTreeSet<String>) -> Vec<String> {
    let mut violations: Vec<String> = FORBIDDEN
        .iter()
        .filter(|forbidden| names.contains(**forbidden))
        .map(|forbidden| format!("`{forbidden}` is a storage engine MemCastle must not link"))
        .collect();
    violations.extend(
        names
            .iter()
            .filter(|name| name.starts_with("surrealdb-kvs-"))
            .filter(|name| !ALLOWED_ENGINES.contains(&name.as_str()))
            .map(|name| format!("`{name}` is a SurrealDB engine crate that is not allowed")),
    );
    if !names.contains("surrealkv") {
        violations.push("`surrealkv`, the embedded engine, is missing".to_owned());
    }
    violations
}

fn lockfile() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock"))
        .expect("read Cargo.lock")
}

#[test]
fn the_embedded_storage_engine_is_surrealkv_and_no_other_backend_is_linked() {
    let violations = storage_violations(&package_names(&lockfile()));

    assert!(
        violations.is_empty(),
        "the dependency tree no longer matches ADR-001 (SurrealKV only):\n  {}\n\
         check the `surrealdb` and `surrealkit` features in Cargo.toml",
        violations.join("\n  ")
    );
}

#[test]
fn a_lockfile_that_pulls_in_rocksdb_is_reported() {
    let lock = r#"
        [[package]]
        name = "surrealkv"
        version = "0.1.0"

        [[package]]
        name = "librocksdb-sys"
        version = "0.1.0"

        [[package]]
        name = "surrealdb-kvs-rocksdb"
        version = "0.1.0"
    "#;

    let violations = storage_violations(&package_names(lock));

    assert_eq!(violations.len(), 2, "{violations:?}");
    assert!(violations.iter().any(|v| v.contains("librocksdb-sys")));
    assert!(
        violations
            .iter()
            .any(|v| v.contains("surrealdb-kvs-rocksdb"))
    );
}

#[test]
fn a_lockfile_without_surrealkv_is_reported() {
    let violations = storage_violations(&package_names(
        "[[package]]\nname = \"surrealdb\"\nversion = \"1.0.0\"\n",
    ));

    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("surrealkv"));
}

/// Application SDKs and native database bindings that a mining source might want, none of which MemCastle's core may
/// compile (docs/adr/026): a source that needs one is a WebAssembly component that brings its own, so the core binary
/// stays free of what only one origin needs.
const SOURCE_SPECIFIC: &[&str] = &[
    // Native database bindings: a local-chat-database source would reach for these.
    "rusqlite",
    "libsqlite3-sys",
    "sqlx",
    "diesel",
    "postgres",
    "mysql",
    "mongodb",
    // Version-control and forge SDKs.
    "git2",
    "libgit2-sys",
    "gix",
    "octocrab",
    // Chat, mail and issue-tracker SDKs.
    "slack-morphism",
    "serenity",
    "teloxide",
    "imap",
    "lettre",
    "jira_query",
    "notion-client",
];

/// Every reason `names` links something only one mining source would need.
fn source_specific_violations(names: &BTreeSet<String>) -> Vec<String> {
    SOURCE_SPECIFIC
        .iter()
        .filter(|forbidden| names.contains(**forbidden))
        .map(|forbidden| {
            format!(
                "`{forbidden}` is a source-specific SDK or native binding; a source that needs it belongs in a \
                 WebAssembly component, not in the core"
            )
        })
        .collect()
}

#[test]
fn the_core_links_no_source_specific_sdk_or_native_database_binding() {
    let violations = source_specific_violations(&package_names(&lockfile()));

    assert!(
        violations.is_empty(),
        "MemCastle's core must build without source-specific dependencies (docs/adr/026):\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn a_lockfile_with_a_source_sdk_is_reported() {
    let names = package_names("[[package]]\nname = \"rusqlite\"\nversion = \"0.1.0\"\n");

    let violations = source_specific_violations(&names);

    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("rusqlite"));
}

#[test]
fn the_webassembly_runtime_is_built_without_the_features_a_source_host_never_uses() {
    // The host compiles components and runs them; a profiler, a debugger stub or a text-format parser is surface and
    // build time a source host does not need, and `default-features = false` is what keeps them out.
    let manifest: toml::Table =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .expect("read Cargo.toml")
            .parse()
            .expect("Cargo.toml is TOML");
    let dependencies = manifest["dependencies"].as_table().expect("dependencies");
    for name in ["wasmtime", "wasmtime-wasi"] {
        let dependency = dependencies[name]
            .as_table()
            .unwrap_or_else(|| panic!("`{name}` has a table form"));
        assert_eq!(
            dependency
                .get("default-features")
                .and_then(toml::Value::as_bool),
            Some(false),
            "`{name}` must be declared with `default-features = false`"
        );
    }
    let features: Vec<&str> = dependencies["wasmtime"]["features"]
        .as_array()
        .expect("wasmtime lists its features")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    for unwanted in [
        "profiling",
        "debug",
        "wat",
        "coredump",
        "cache",
        "gc",
        "threads",
    ] {
        assert!(
            !features.contains(&unwanted),
            "wasmtime must not enable `{unwanted}`"
        );
    }
}
