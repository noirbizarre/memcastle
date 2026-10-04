//! Helpers for the `wasm_*` test binaries, which build WebAssembly components with Cargo (docs/development.md).
//!
//! Every test is its own process under nextest, so nothing here can be shared in memory: what the processes share is
//! one Cargo target directory and one lockfile, and both exist to keep each build down to compiling a single small
//! crate.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The reference source, which also supplies the lockfile every scaffolded project is seeded with.
fn reference_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("sources/directory")
}

/// Where every project built by a `wasm_*` test builds, so Cargo compiles `wit-bindgen` and friends once for all of
/// them instead of once per test.
pub fn shared_target() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("source-projects")
}

/// Point a scaffolded project's build at the shared target directory.
///
/// A debug build, because a project is built a dozen times and nothing about the sandbox depends on optimisation, so
/// the release profile's LTO would only make the suite wait. Without this a project builds in release into its own
/// `target/` and recompiles every dependency from scratch, which is most of a minute.
pub fn share_target(manifest: &str, name: &str) -> String {
    let target = shared_target();
    manifest
        .replace("\"--release\", ", "")
        .replace(
            "\"--target-dir\", \"target\"]",
            &format!("\"--target-dir\", '{}']", target.display()),
        )
        .replace(
            &format!(
                "output = \"target/wasm32-wasip2/release/{}.wasm\"",
                name.replace('-', "_")
            ),
            &format!(
                "output = '{}'",
                target
                    .join("wasm32-wasip2/debug")
                    .join(format!("{}.wasm", name.replace('-', "_")))
                    .display()
            ),
        )
}

/// Give a freshly scaffolded project the reference source's `Cargo.lock`.
///
/// A scaffold ships none, so its first build updates the crates.io index and resolves 35 packages, holding Cargo's
/// package-cache lock for the duration; every build in the suite then queues behind it, and on a runner it is also a
/// network round trip each time. The reference source has the same dependencies, so its lockfile resolves them with
/// no network: Cargo only renames the root package in it.
pub fn seed_lockfile(project: &Path) {
    let lock = project.join("Cargo.lock");
    if !lock.exists() {
        std::fs::copy(reference_dir().join("Cargo.lock"), lock)
            .expect("the reference lockfile copies");
    }
}

/// Make an `init`-ed project in `dir/name` build into the shared target directory, from the shared lockfile.
pub fn share_target_of(dir: &Path, name: &str) {
    let project = dir.join(name);
    let manifest = project.join("memcastle-source.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, share_target(&text, name)).unwrap();
    seed_lockfile(&project);
}

/// The reference source `sources/directory`, built as a component, and the manifest text it is installed with.
///
/// It is built in place, with a debug profile and into the shared target directory, and its own `target/` and
/// `dist/` are left alone: the release build with LTO that `memcastle source build` makes (and that
/// `mise run sources:check` and CI's per-source job still make) recompiles every dependency with optimisations, and
/// every test process used to wait for it. What these tests prove, that the host runs a component, does not depend on
/// the profile.
///
/// Every process that calls this runs `cargo build`, and Cargo's build-directory lock lets one compile while the others
/// wait and then find nothing left to do.
pub fn reference_component() -> (Vec<u8>, String) {
    let dir = reference_dir();
    let target = shared_target();
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--target-dir"])
        .arg(&target)
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .status()
        .expect("cargo starts");
    assert!(
        status.success(),
        "the reference source builds; `rustup target add wasm32-wasip2` provides its target"
    );
    let component =
        std::fs::read(target.join("wasm32-wasip2/debug/memcastle_source_directory.wasm"))
            .expect("the build produced the reference component");
    let manifest = std::fs::read_to_string(dir.join("memcastle-source.toml")).unwrap();
    (component, manifest)
}
