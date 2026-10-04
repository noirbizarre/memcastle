//! `memcastle source init`: a new source project that builds, passes the conformance cases and can be packaged
//! as soon as it is created.
//!
//! Every template is a complete, tiny source with the same behaviour (the `.txt` files of a flat directory) so that
//! the same fixtures check all of them, and so that what differs between templates is only the language and the
//! toolchain. The contract is copied in from the one this MemCastle implements, so a project starts on exactly the
//! contract of the MemCastle that scaffolded it.

use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::domain::CONTRACT_VERSION;
use crate::error::{Error, Result};

use super::manifest;
use super::{CONTRACT_WIT, MANIFEST_FILE};

/// Which language, runtime and toolchain a new source is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Template {
    /// Rust, compiled straight to a component for `wasm32-wasip2`.
    Rust,
    /// TypeScript, compiled to JavaScript and componentized with `jco` (an embedded JavaScript engine).
    Typescript,
    /// Python, componentized with `componentize-py` (an embedded CPython).
    Python,
    /// A Rust component that wraps a command-line program through the host's `run-process`.
    Cli,
}

impl Template {
    /// The template's name, as given to `--template`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Typescript => "typescript",
            Self::Python => "python",
            Self::Cli => "cli",
        }
    }
}

/// The files a new source consists of, as `(path, contents)`.
struct Files(Vec<(String, Vec<u8>)>);

impl Files {
    fn add(&mut self, path: &str, contents: impl Into<Vec<u8>>) {
        self.0.push((path.to_string(), contents.into()));
    }
}

/// The tree every template is tested against: a flat directory of three text files.
const FIXTURE_FILES: [(&str, &str); 3] = [
    ("a.txt", "alpha\n"),
    ("b.txt", "bravo\n"),
    ("c.txt", "charlie\n"),
];

fn case_json() -> String {
    let documents: Vec<String> = FIXTURE_FILES
        .iter()
        .map(|(name, text)| {
            format!(
                "    {{\n      \"external_id\": \"{name}\",\n      \"title\": \"{name}\",\n      \"kind\": \"file\",\n      \"segments\": [{}]\n    }}",
                serde_json::Value::String((*text).to_string())
            )
        })
        .collect();
    format!(
        "{{\n  \"name\": \"text-files\",\n  \"description\": \"A flat directory of three text files, discovered two at a time.\",\n  \"locator\": \"tree\",\n  \"limit\": 2,\n  \"documents\": [\n{}\n  ],\n  \"skipped\": []\n}}\n",
        documents.join(",\n")
    )
}

/// The `memcastle` version requirement a new source starts with: this release line, which is as far as a contract
/// can be promised to hold.
fn memcastle_requirement() -> String {
    requirement_for(
        &semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
    )
}

/// The requirement a source scaffolded by MemCastle `version` starts with: its own release line.
///
/// Before 1.0 a minor release may break the contract, so the line is the minor; from 1.0 it is the major.
fn requirement_for(version: &semver::Version) -> String {
    if version.major == 0 {
        format!(
            ">={0}.{1}.0, <{0}.{2}.0",
            version.major,
            version.minor,
            version.minor + 1
        )
    } else {
        format!(">={0}.0.0, <{1}.0.0", version.major, version.major + 1)
    }
}

fn manifest_text(name: &str, template: Template) -> String {
    let (permissions, build, output) = match template {
        Template::Rust => (
            "# Nothing is granted that is not listed. This source reads the directory it is asked to mine, and nothing else.\n[permissions.filesystem]\nread = [\"locator\"]\n".to_string(),
            "[\"cargo\", \"build\", \"--release\", \"--target\", \"wasm32-wasip2\", \"--target-dir\", \"target\"]".to_string(),
            format!("target/wasm32-wasip2/release/{}.wasm", name.replace('-', "_")),
        ),
        Template::Cli => (
            "# The programs this source may run, by exact name and without a shell. It reads no files itself.\n[permissions]\nprocess = [\"ls\", \"cat\"]\n".to_string(),
            "[\"cargo\", \"build\", \"--release\", \"--target\", \"wasm32-wasip2\", \"--target-dir\", \"target\"]".to_string(),
            format!("target/wasm32-wasip2/release/{}.wasm", name.replace('-', "_")),
        ),
        Template::Typescript => (
            "# Nothing is granted that is not listed. This source reads the directory it is asked to mine, and nothing else.\n[permissions.filesystem]\nread = [\"locator\"]\n".to_string(),
            "[\"npm\", \"run\", \"build\"]".to_string(),
            "dist/source.wasm".to_string(),
        ),
        Template::Python => (
            "# Nothing is granted that is not listed. This source reads the directory it is asked to mine, and nothing else.\n[permissions.filesystem]\nread = [\"locator\"]\n".to_string(),
            "[\"componentize-py\", \"-d\", \"wit\", \"-w\", \"source\", \"componentize\", \"app\", \"-o\", \"dist/source.wasm\"]".to_string(),
            "dist/source.wasm".to_string(),
        ),
    };
    format!(
        "# What this source is, what it needs, and how it is built. Every key is documented in docs/writing-sources.md.\n\n\
         [source]\nname = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"the text files of a directory, one document per file\"\n\n\
         [compatibility]\n# The contract (wit/memcastle-source.wit) this source is built against, and the MemCastle versions it runs on.\ncontract = \"{CONTRACT_VERSION}\"\nmemcastle = \"{}\"\n\n\
         [capabilities]\nincremental = true\nretains_raw = false\nneeds_credentials = false\n\n\
         {permissions}\n\
         [build]\ncommand = {build}\noutput = \"{output}\"\n\n\
         [test]\nfixtures = \"fixtures\"\n",
        memcastle_requirement()
    )
}

fn readme(name: &str, template: Template) -> String {
    let toolchain = match template {
        Template::Rust | Template::Cli => {
            "Needs a Rust toolchain with the `wasm32-wasip2` target (`rustup target add wasm32-wasip2`)."
        }
        Template::Typescript => {
            "Needs Node.js and npm; `npm install` fetches TypeScript and `jco`. TypeScript is not compiled to WebAssembly directly: `jco componentize` embeds a JavaScript engine in the component."
        }
        Template::Python => {
            "Needs Python and `pip install componentize-py`. Python is not compiled to WebAssembly directly: `componentize-py` embeds CPython in the component, so only the standard library and pure-Python packages are available."
        }
    };
    format!(
        "# {name}\n\nA MemCastle source, from the `{}` template.\n\n{toolchain}\n\n```sh\nmemcastle source build     # produce dist/source.wasm\nmemcastle source test      # run fixtures/ against the component\nmemcastle source package   # write dist/{name}-0.1.0.tar.gz\nmemcastle source install dist/{name}-0.1.0.tar.gz\n```\n\nThe contract is `wit/memcastle-source.wit`; `memcastle-source.toml` says what the source is, what it needs and how it is\nbuilt. See docs/writing-sources.md in the MemCastle repository.\n",
        template.name()
    )
}

/// Create a new source called `name` in `parent/name`, from `template`. Returns the new directory and the files
/// written, relative to it.
///
/// # Errors
///
/// [`Error::SourceManifestInvalid`] when `name` is not a valid source name, [`Error::InvalidInput`] when the
/// directory already exists and is not empty, and [`Error::Io`] when it cannot be written.
pub fn init(parent: &Path, name: &str, template: Template) -> Result<(PathBuf, Vec<String>)> {
    let text = manifest_text(name, template);
    // The scaffold is held to the same rules as any manifest, so a bad name fails here and not at the first build.
    manifest::parse(&text, &crate::mining::registry::BUILTIN_NAMES)?;

    let dir = parent.join(name);
    if dir.exists() && std::fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(Error::invalid_input(
            "name",
            format!(
                "{} already exists and is not empty; choose another name or remove it",
                dir.display()
            ),
        ));
    }

    let mut files = Files(Vec::new());
    files.add(MANIFEST_FILE, text);
    files.add("README.md", readme(name, template));
    files.add(
        ".gitignore",
        "target/\ndist/\nbuild/\nnode_modules/\n__pycache__/\n",
    );
    files.add("wit/memcastle-source.wit", CONTRACT_WIT);
    files.add("fixtures/text-files/case.json", case_json());
    for (file, text) in FIXTURE_FILES {
        files.add(&format!("fixtures/text-files/tree/{file}"), text);
    }
    match template {
        Template::Rust => {
            files.add(
                "Cargo.toml",
                include_str!("templates/rust-cargo.toml").replace("__NAME__", name),
            );
            files.add(
                "src/lib.rs",
                include_str!("templates/rust-lib.rs").replace("__NAME__", name),
            );
        }
        Template::Cli => {
            files.add(
                "Cargo.toml",
                include_str!("templates/rust-cargo.toml").replace("__NAME__", name),
            );
            files.add(
                "src/lib.rs",
                include_str!("templates/cli-lib.rs").replace("__NAME__", name),
            );
        }
        Template::Typescript => {
            files.add(
                "package.json",
                include_str!("templates/typescript-package.json").replace("__NAME__", name),
            );
            files.add(
                "tsconfig.json",
                include_str!("templates/typescript-tsconfig.json"),
            );
            files.add(
                "src/index.ts",
                include_str!("templates/typescript-index.ts").replace("__NAME__", name),
            );
        }
        Template::Python => {
            files.add(
                "app.py",
                include_str!("templates/python-app.py").replace("__NAME__", name),
            );
        }
    }

    let mut written = Vec::new();
    for (path, contents) in files.0 {
        let target = dir.join(&path);
        if let Some(folder) = target.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|e| Error::io(folder.display().to_string(), e))?;
        }
        std::fs::write(&target, contents)
            .map_err(|e| Error::io(target.display().to_string(), e))?;
        written.push(path);
    }
    Ok((dir, written))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_scaffolds_a_manifest_that_passes_validation() {
        for template in [
            Template::Rust,
            Template::Typescript,
            Template::Python,
            Template::Cli,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (root, files) = init(dir.path(), "my-source", template).unwrap();

            assert!(files.contains(&MANIFEST_FILE.to_string()), "{template:?}");
            let text = std::fs::read_to_string(root.join(MANIFEST_FILE)).unwrap();
            let parsed = manifest::parse(&text, &[]).unwrap();
            assert_eq!(parsed.source.name, "my-source");
            assert!(
                parsed.build.is_some() && parsed.test.is_some(),
                "{template:?}"
            );
        }
    }

    #[test]
    fn a_scaffold_carries_exactly_the_contract_this_memcastle_implements() {
        let dir = tempfile::tempdir().unwrap();
        let (root, _) = init(dir.path(), "demo", Template::Rust).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("wit/memcastle-source.wit")).unwrap(),
            CONTRACT_WIT
        );
        assert!(CONTRACT_WIT.contains(&format!("memcastle:source@{CONTRACT_VERSION}")));
    }

    #[test]
    fn the_cli_template_asks_for_programs_and_no_files_and_the_others_for_files_and_no_programs() {
        let dir = tempfile::tempdir().unwrap();
        for (template, name) in [(Template::Cli, "wrapper"), (Template::Rust, "reader")] {
            let (root, _) = init(dir.path(), name, template).unwrap();
            let parsed = manifest::parse(
                &std::fs::read_to_string(root.join(MANIFEST_FILE)).unwrap(),
                &[],
            )
            .unwrap();
            let permissions = parsed.permissions;
            assert_eq!(template == Template::Cli, !permissions.process.is_empty());
            assert_eq!(
                template == Template::Rust,
                !permissions.filesystem.read.is_empty()
            );
        }
    }

    #[test]
    fn a_bad_or_built_in_name_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Bad Name", "../escape", "directory", ""] {
            assert!(init(dir.path(), name, Template::Rust).is_err(), "{name:?}");
        }
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn an_existing_non_empty_directory_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("demo")).unwrap();
        std::fs::write(dir.path().join("demo/keep.txt"), "mine").unwrap();
        assert!(matches!(
            init(dir.path(), "demo", Template::Rust),
            Err(Error::InvalidInput { .. })
        ));
    }

    #[test]
    fn the_generated_case_parses_as_a_conformance_case() {
        let parsed: super::super::conformance::Case = serde_json::from_str(&case_json()).unwrap();
        assert_eq!(parsed.documents.len(), 3);
    }

    #[test]
    fn a_scaffold_starts_on_its_own_release_line_and_the_requirement_is_one_cargo_would_accept() {
        let before = semver::Version::new(0, 7, 3);
        let after = semver::Version::new(2, 1, 0);
        assert_eq!(requirement_for(&before), ">=0.7.0, <0.8.0");
        assert_eq!(requirement_for(&after), ">=2.0.0, <3.0.0");
        for (version, inside, outside) in [(before, "0.7.9", "0.8.0"), (after, "2.9.0", "3.0.0")] {
            let requirement = semver::VersionReq::parse(&requirement_for(&version)).unwrap();
            assert!(requirement.matches(&semver::Version::parse(inside).unwrap()));
            assert!(!requirement.matches(&semver::Version::parse(outside).unwrap()));
        }
    }
}
