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
        std::fs::create_dir_all(self.dir.join(DIST))
            .and_then(|()| std::fs::write(&target, &bytes))
            .map_err(|source| Error::io(target.display().to_string(), source))?;
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
            "[source]\nname = \"demo\"\nversion = \"0.1.0\"\ndescription = \"d\"\n[compatibility]\ncontract = \"0.1\"\nmemcastle = \">=0.1\"\n",
        )
        .unwrap();
        let error = Project::open(dir.path())
            .unwrap()
            .build()
            .unwrap_err()
            .to_string();
        assert!(error.contains("[build]"), "{error}");
    }
}
