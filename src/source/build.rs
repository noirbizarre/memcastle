//! Building, testing and packaging a source project: what `memcastle source build`, `test` and `package` do.
//!
//! All of it is local and needs no daemon. Building delegates to the project's own toolchain, named in the manifest's
//! `[build]` section, because how a Rust, TypeScript or Python project becomes a component is theirs to decide; what
//! MemCastle checks is the result.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::MiningConfig;
use crate::domain::SourceManifest;
use crate::error::{Error, Result};
use crate::mining::registry::BUILTIN_NAMES;
use crate::mining::wasm::WasmAdapter;

use super::conformance::{self, Report};
use super::package::{self, SourcePackage};
use super::{COMPONENT_FILE, MANIFEST_FILE, manifest};

/// Where a project's build products go.
const DIST: &str = "dist";

/// A source project on disk: its directory, manifest, and the manifest's text.
pub struct Project {
    /// The project's directory.
    pub dir: PathBuf,
    /// The validated manifest.
    pub manifest: SourceManifest,
    manifest_text: String,
}

impl Project {
    /// Open the project in `dir`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when there is no manifest and [`Error::SourceManifestInvalid`] when it is wrong.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_FILE);
        let manifest_text = std::fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::SourceManifestInvalid {
                    message: format!(
                        "{} has no `{MANIFEST_FILE}`; run this in a source project, or create one with `memcastle source init`",
                        dir.display()
                    ),
                }
            } else {
                Error::io(path.display().to_string(), source)
            }
        })?;
        let manifest = manifest::parse(&manifest_text, &BUILTIN_NAMES)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            manifest,
            manifest_text,
        })
    }

    /// Where the built component is placed.
    #[must_use]
    pub fn component_path(&self) -> PathBuf {
        self.dir.join(DIST).join(COMPONENT_FILE)
    }

    /// Where the package is written.
    #[must_use]
    pub fn archive_path(&self) -> PathBuf {
        self.dir.join(DIST).join(format!(
            "{}-{}.tar.gz",
            self.manifest.source.name, self.manifest.source.version
        ))
    }

    /// Run the project's build and place the component at [`Project::component_path`].
    ///
    /// The build's own output goes straight to the terminal: it is what the person building needs to read.
    ///
    /// # Errors
    ///
    /// [`Error::SourceBuildFailed`] when the manifest has no `[build]` section, the build command cannot run or fails,
    /// or its output is not a WebAssembly component.
    pub fn build(&self) -> Result<PathBuf> {
        let Some(build) = &self.manifest.build else {
            return Err(Error::SourceBuildFailed {
                message: format!(
                    "`{MANIFEST_FILE}` has no `[build]` section saying how to produce the component"
                ),
            });
        };
        let (program, args) =
            build
                .command
                .split_first()
                .ok_or_else(|| Error::SourceBuildFailed {
                    message: "`build.command` is empty".to_string(),
                })?;
        let status = Command::new(program)
            .args(args)
            .current_dir(&self.dir)
            .status()
            .map_err(|source| Error::SourceBuildFailed {
                message: format!(
                    "`{}` could not be started: {source}. Install the toolchain this template needs (see README.md)",
                    build.command.join(" ")
                ),
            })?;
        if !status.success() {
            return Err(Error::SourceBuildFailed {
                message: format!("`{}` exited with {status}", build.command.join(" ")),
            });
        }
        let built = self.dir.join(&build.output);
        let bytes = std::fs::read(&built).map_err(|source| Error::SourceBuildFailed {
            message: format!(
                "the build succeeded but `{}` is not there ({source}); fix `build.output` in `{MANIFEST_FILE}`",
                built.display()
            ),
        })?;
        check_is_component(&bytes, &built)?;
        let target = self.component_path();
        // Written to a file of its own and renamed over the target, so a process reading `dist/source.wasm` while
        // another builds (a test binary beside `packaging/sources/build.sh`) sees the old component or the new one,
        // never an empty or half-written file. The process id keeps concurrent builders off each other's file.
        let temporary = self
            .dir
            .join(DIST)
            .join(format!(".{COMPONENT_FILE}.{}.tmp", std::process::id()));
        std::fs::create_dir_all(self.dir.join(DIST))
            .and_then(|()| std::fs::write(&temporary, &bytes))
            .and_then(|()| std::fs::rename(&temporary, &target))
            .map_err(|source| {
                // A failed write or rename must not leave a stray file in a directory that gets packaged.
                let _ = std::fs::remove_file(&temporary);
                Error::io(target.display().to_string(), source)
            })?;
        Ok(target)
    }

    /// Run the conformance cases the manifest names against the component at [`Project::component_path`], loaded the
    /// way the daemon loads it.
    ///
    /// # Errors
    ///
    /// [`Error::SourceBuildFailed`] when there is no built component, and the loading and case-reading errors of
    /// [`WasmAdapter::load`] and [`conformance::run_all`]. A case that fails is in the report.
    pub async fn test(&self, mining: &MiningConfig) -> Result<Report> {
        let Some(test) = &self.manifest.test else {
            return Err(Error::SourceManifestInvalid {
                message: format!(
                    "`{MANIFEST_FILE}` has no `[test]` section naming the conformance cases"
                ),
            });
        };
        let component = self.read_component()?;
        let manifest = self.manifest.clone();
        let mining = mining.clone();
        let adapter =
            tokio::task::spawn_blocking(move || WasmAdapter::load(&manifest, &component, &mining))
                .await
                .map_err(|e| Error::SourceFailed {
                    name: self.manifest.source.name.clone(),
                    message: e.to_string(),
                })??;
        conformance::run_all(&adapter, &self.dir.join(&test.fixtures)).await
    }

    /// Write the distributable archive from the built component, returning where it is.
    ///
    /// # Errors
    ///
    /// [`Error::SourceBuildFailed`] when there is no built component, and [`Error::SourcePackageInvalid`] or
    /// [`Error::Io`] when the archive cannot be written.
    pub fn package(&self, output: Option<&Path>) -> Result<(PathBuf, SourcePackage)> {
        let component = self.read_component()?;
        let extras: Vec<(String, Vec<u8>)> = ["README.md", "LICENSE", "LICENSE.md"]
            .iter()
            .filter_map(|name| {
                std::fs::read(self.dir.join(name))
                    .ok()
                    .map(|bytes| ((*name).to_string(), bytes))
            })
            .collect();
        let archive = package::pack(&self.manifest_text, &component, &extras)?;
        // Read back what was just made, so a package that would not install is found here and not by whoever gets it.
        let unpacked = package::inspect(&archive)?;
        let target = output.map_or_else(|| self.archive_path(), Path::to_path_buf);
        if let Some(folder) = target.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|e| Error::io(folder.display().to_string(), e))?;
        }
        std::fs::write(&target, archive).map_err(|e| Error::io(target.display().to_string(), e))?;
        Ok((target, unpacked))
    }

    fn read_component(&self) -> Result<Vec<u8>> {
        let path = self.component_path();
        std::fs::read(&path).map_err(|_| Error::SourceBuildFailed {
            message: format!(
                "there is no built component at {}; run `memcastle source build` first",
                path.display()
            ),
        })
    }
}

/// Whether `bytes` is a WebAssembly *component*, as opposed to a plain module.
///
/// A module is what most toolchains produce by default, and loading one fails with a message about missing imports
/// that does not say the real problem; this says it, with the way out.
fn check_is_component(bytes: &[u8], path: &Path) -> Result<()> {
    // `\0asm`, then version 0x0d and layer 1 for a component (a core module is version 1, layer 0).
    match bytes.get(..8) {
        Some([0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00]) => Ok(()),
        Some([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]) => Err(Error::SourceBuildFailed {
            message: format!(
                "{} is a core WebAssembly module, not a component; build for `wasm32-wasip2`, or wrap it with `wasm-tools component new`",
                path.display()
            ),
        }),
        _ => Err(Error::SourceBuildFailed {
            message: format!("{} is not a WebAssembly binary", path.display()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MiningConfig;

    #[test]
    fn a_core_module_is_told_apart_from_a_component_and_the_error_says_how_to_fix_it() {
        let path = Path::new("x.wasm");
        assert!(check_is_component(b"\0asm\x0d\0\x01\0rest", path).is_ok());
        let module = check_is_component(b"\0asm\x01\0\0\0rest", path)
            .unwrap_err()
            .to_string();
        assert!(module.contains("wasm32-wasip2"), "{module}");
        assert!(check_is_component(b"#!/bin/sh", path).is_err());
        assert!(check_is_component(b"\0as", path).is_err());
    }

    #[test]
    fn opening_a_directory_without_a_manifest_says_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let error = Project::open(dir.path()).err().unwrap().to_string();
        assert!(error.contains("source init"), "{error}");
    }

    #[test]
    fn a_manifest_without_a_build_section_cannot_be_built_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MANIFEST_FILE),
            "[source]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"d\"\n[compatibility]\ncontract = \"0.2\"\nmemcastle = \">=0.1\"\n",
        )
        .unwrap();
        let error = Project::open(dir.path())
            .unwrap()
            .build()
            .unwrap_err()
            .to_string();
        assert!(error.contains("[build]"), "{error}");
    }

    const MANIFEST: &str = "[source]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"d\"\n\n[compatibility]\ncontract = \"0.2\"\nmemcastle = \">=0.1\"\n";

    fn project(extra: &str) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILE), format!("{MANIFEST}{extra}")).unwrap();
        let project = Project::open(dir.path()).unwrap();
        (dir, project)
    }

    #[test]
    fn a_manifest_that_cannot_be_read_for_another_reason_than_absence_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the manifest should be: it exists, and cannot be read as a file.
        std::fs::create_dir(dir.path().join(MANIFEST_FILE)).unwrap();
        assert!(matches!(Project::open(dir.path()), Err(Error::Io { .. })));
    }

    #[test]
    fn a_build_command_that_cannot_be_started_names_it_and_points_at_the_readme() {
        let (_dir, project) =
            project("\n[build]\ncommand = [\"memcastle-no-such-program\"]\noutput = \"x.wasm\"\n");
        let error = project.build().unwrap_err().to_string();
        assert!(
            error.contains("memcastle-no-such-program") && error.contains("README.md"),
            "{error}"
        );
    }

    #[test]
    fn a_build_that_succeeds_without_producing_the_output_says_which_file_is_missing() {
        // `cargo --version` exits successfully on every platform and writes nothing.
        let (_dir, project) =
            project("\n[build]\ncommand = [\"cargo\", \"--version\"]\noutput = \"nowhere.wasm\"\n");
        let error = project.build().unwrap_err().to_string();
        assert!(
            error.contains("nowhere.wasm") && error.contains("build.output"),
            "{error}"
        );
    }

    #[test]
    fn a_build_that_fails_reports_how_it_exited() {
        let (_dir, project) =
            project("\n[build]\ncommand = [\"cargo\", \"--no-such-flag\"]\noutput = \"x.wasm\"\n");
        let error = project.build().unwrap_err().to_string();
        assert!(error.contains("exited with"), "{error}");
    }

    #[test]
    fn a_build_that_produces_a_core_module_is_refused_with_the_way_out() {
        let (dir, project) =
            project("\n[build]\ncommand = [\"cargo\", \"--version\"]\noutput = \"m.wasm\"\n");
        std::fs::write(dir.path().join("m.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let error = project.build().unwrap_err().to_string();
        assert!(error.contains("core WebAssembly module"), "{error}");
    }

    #[test]
    fn a_good_build_places_the_component_in_dist() {
        let (dir, project) =
            project("\n[build]\ncommand = [\"cargo\", \"--version\"]\noutput = \"c.wasm\"\n");
        std::fs::write(dir.path().join("c.wasm"), b"\0asm\x0d\0\x01\0").unwrap();
        let placed = project.build().unwrap();
        assert_eq!(placed, project.component_path());
        assert!(placed.is_file());
    }

    #[test]
    fn building_over_an_existing_component_replaces_it_and_leaves_no_temporary_file_behind() {
        let (dir, project) =
            project("\n[build]\ncommand = [\"cargo\", \"--version\"]\noutput = \"c.wasm\"\n");
        std::fs::write(dir.path().join("c.wasm"), b"\0asm\x0d\0\x01\0first").unwrap();
        project.build().unwrap();
        std::fs::write(dir.path().join("c.wasm"), b"\0asm\x0d\0\x01\0second").unwrap();

        let placed = project.build().unwrap();

        assert_eq!(std::fs::read(&placed).unwrap(), b"\0asm\x0d\0\x01\0second");
        let left: Vec<_> = std::fs::read_dir(dir.path().join(DIST))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [COMPONENT_FILE], "{left:?}");
    }

    #[tokio::test]
    async fn testing_needs_a_test_section_and_a_built_component() {
        let (_dir, without) = project("");
        let error = without
            .test(&MiningConfig::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("[test]"), "{error}");

        let (_dir, unbuilt) = project("\n[test]\nfixtures = \"fixtures\"\n");
        let error = unbuilt
            .test(&MiningConfig::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("memcastle source build"), "{error}");
    }

    #[test]
    fn packaging_needs_a_built_component_and_then_writes_the_archive_with_the_readme() {
        let (dir, project) = project("");
        let error = project.package(None).unwrap_err().to_string();
        assert!(error.contains("memcastle source build"), "{error}");

        std::fs::create_dir(dir.path().join("dist")).unwrap();
        std::fs::write(project.component_path(), b"\0asm\x0d\0\x01\0").unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let target = dir.path().join("out/custom.tar.gz");

        let (written, package) = project.package(Some(&target)).unwrap();

        assert_eq!(written, target);
        assert_eq!(
            package.extras.len(),
            1,
            "the README travels with the package"
        );
        assert!(target.is_file());
        assert_eq!(
            project.archive_path().file_name().unwrap(),
            "demo-0.1.0.tar.gz"
        );
    }
}
