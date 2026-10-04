//! The project-local `.config/memcastle.toml`, as far as mining reads it: the wing a mined directory belongs to.
//!
//! The file declares which memory scope a project belongs to (see `docs/project-config.md`).
//! The daemon reads it for exactly one decision, the default wing of a mined directory, and only here, in the adapter
//! that reads that directory: the pipeline stays source-agnostic and the rest of the daemon knows no project file.
//! Everything else the file says (rooms, the environment overrides) is resolved by agent integrations on their own side
//! and passed to MemCastle as ordinary parameters.
//!
//! The TypeScript integrations carry their own reader of the same contract, held to this one by the shared fixtures in
//! `tests/fixtures/project-config/`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::domain::{NameKind, validate_name};

/// The file's location under a project root.
const PROJECT_FILE: &str = ".config/memcastle.toml";

/// A table nothing may be written in yet: accepting it now lets it grow later without a breaking change.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reserved {}

/// `[project]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectSection {
    /// The project's display name, and the wing when `[memcastle] wing` says none.
    name: Option<String>,
}

/// `[memcastle]`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeSection {
    wing: Option<String>,
    /// Scopes search in integrations; mining's room is the adapter's, so it is read only to be validated.
    room: Option<String>,
}

/// The whole file. Unknown keys are refused so that a typo, or a secret someone pasted in, is an error and never a
/// silently ignored line (the file is not a place for credentials and says so by rejecting them).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectFile {
    #[serde(default)]
    project: ProjectSection,
    #[serde(default)]
    memcastle: ScopeSection,
    #[serde(default)]
    #[allow(dead_code)] // Reserved: parsed so a stray key under it is refused, never read.
    mining: Reserved,
}

/// The project file that governs `start`, if any.
///
/// Walks from `start` toward the root and takes the nearest file, so a nested project uses its own file and inherits
/// nothing from a parent.
/// The walk never reads `$HOME/.config/memcastle.toml` (it would claim every directory under the home directory) and
/// never goes above the enclosing git root or `$HOME`, so a project cannot be given an unrelated parent's scope.
fn discover(start: &Path, home: Option<&Path>) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if home == Some(dir) {
            return None;
        }
        let candidate = dir.join(PROJECT_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        // `.git` is a directory in a checkout and a file in a worktree or submodule: either marks a project's edge.
        if dir.join(".git").exists() {
            return None;
        }
    }
    None
}

/// The wing declared for the project containing `start`: `Ok(None)` when no file governs it.
///
/// # Errors
///
/// A message naming the file and the problem when the file cannot be read, is not valid for the contract, or names a
/// wing or room MemCastle would refuse.
fn declared_wing(start: &Path, home: Option<&Path>) -> Result<Option<String>, String> {
    let Some(file) = discover(start, home) else {
        return Ok(None);
    };
    let at = file.display();
    let text = std::fs::read_to_string(&file).map_err(|e| format!("cannot read {at}: {e}"))?;
    let parsed: ProjectFile =
        toml::from_str(&text).map_err(|e| format!("{at} is not valid: {e}"))?;
    // A name the daemon would refuse must fail here, where the file is named, not later as an unexplained 400.
    for (field, value, kind) in [
        ("[project] name", &parsed.project.name, NameKind::Wing),
        ("[memcastle] wing", &parsed.memcastle.wing, NameKind::Wing),
        ("[memcastle] room", &parsed.memcastle.room, NameKind::Room),
    ] {
        if let Some(value) = value {
            validate_name(kind, value).map_err(|e| format!("{at}: {field} is not usable: {e}"))?;
        }
    }
    Ok(parsed.memcastle.wing.or(parsed.project.name))
}

/// The wing the project containing `start` declares, or `None` to use the adapter's own default.
///
/// `default_wing` cannot fail, so a broken file is logged and mining carries on with the default.
/// Failing the job instead would let one typo in a file nobody asked about block mining a directory that mined fine
/// before, and the explicit wing an operator can still pass is unaffected either way.
pub(super) fn project_wing(start: &Path) -> Option<String> {
    let home = dirs::home_dir().map(|h| h.canonicalize().unwrap_or(h));
    wing_with_home(start, home.as_deref())
}

/// [`project_wing`] with the home directory given, which is the seam the tests use instead of the real `$HOME`.
fn wing_with_home(start: &Path, home: Option<&Path>) -> Option<String> {
    match declared_wing(start, home) {
        Ok(wing) => wing,
        Err(message) => {
            tracing::warn!(
                directory = %start.display(),
                "ignoring the project file, mining under the directory's own name instead: {message}"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp tree with `files` written under it (parents created), returning its canonical root.
    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    fn wing(dir: &Path, start: &str, home: Option<&Path>) -> Result<Option<String>, String> {
        declared_wing(&dir.join(start), home)
    }

    #[test]
    fn the_shared_fixtures_resolve_to_the_same_wing_as_the_integrations_expect() {
        // The integrations resolve the same files in TypeScript; this is what keeps the two readers one contract.
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/project-config/cases.json"
        ))
        .unwrap();
        let mut replayed = 0;
        for case in fixtures["cases"].as_array().unwrap() {
            // The daemon never reads the environment: those cases belong to the integrations.
            if case.get("env").is_some() {
                continue;
            }
            let name = case["name"].as_str().unwrap();
            let files: Vec<(&str, &str)> = case["files"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str().unwrap()))
                .collect();
            let dir = tree(&files);
            let home = case
                .get("home")
                .map(|h| dir.path().join(h.as_str().unwrap()));
            let got = wing(dir.path(), case["start"].as_str().unwrap(), home.as_deref());
            if let Some(substring) = case.get("error") {
                let error = got.expect_err(name);
                assert!(
                    error.contains(substring.as_str().unwrap()),
                    "{name}: {error}"
                );
            } else {
                // `expect: null` is no project, and a project with no wing is the same answer for mining.
                let expected = case["expect"]["wing"].as_str().map(str::to_string);
                assert_eq!(got, Ok(expected), "{name}");
            }
            replayed += 1;
        }
        assert!(replayed > 15, "the fixtures were not replayed ({replayed})");
    }

    #[test]
    fn a_directory_without_a_project_file_declares_no_wing() {
        let dir = tree(&[("src/lib.rs", "")]);
        assert_eq!(wing(dir.path(), "src", None), Ok(None));
    }

    #[test]
    fn the_wing_key_is_found_from_a_nested_directory() {
        let dir = tree(&[
            (".config/memcastle.toml", "[memcastle]\nwing = \"castle\"\n"),
            ("a/b/x", ""),
        ]);
        assert_eq!(wing(dir.path(), "a/b", None), Ok(Some("castle".into())));
    }

    #[test]
    fn the_project_name_is_the_wing_only_when_no_wing_is_set() {
        let named = tree(&[(".config/memcastle.toml", "[project]\nname = \"named\"\n")]);
        assert_eq!(wing(named.path(), "", None), Ok(Some("named".into())));
        let both = tree(&[(
            ".config/memcastle.toml",
            "[project]\nname = \"named\"\n[memcastle]\nwing = \"explicit\"\n",
        )]);
        assert_eq!(wing(both.path(), "", None), Ok(Some("explicit".into())));
    }

    #[test]
    fn the_nearest_project_file_wins_and_a_parent_is_not_inherited() {
        let dir = tree(&[
            (".config/memcastle.toml", "[memcastle]\nwing = \"outer\"\n"),
            (
                "inner/.config/memcastle.toml",
                "[memcastle]\nroom = \"only-a-room\"\n",
            ),
        ]);
        // The inner file declares no wing, and the outer one must not fill the gap.
        assert_eq!(wing(dir.path(), "inner", None), Ok(None));
    }

    #[test]
    fn the_walk_stops_at_the_git_root() {
        let dir = tree(&[
            (".config/memcastle.toml", "[memcastle]\nwing = \"outer\"\n"),
            ("repo/.git/HEAD", ""),
        ]);
        assert_eq!(wing(dir.path(), "repo", None), Ok(None));
    }

    #[test]
    fn a_file_beside_a_git_marker_still_governs_its_own_root() {
        let dir = tree(&[
            (".config/memcastle.toml", "[memcastle]\nwing = \"mine\"\n"),
            (".git", "gitdir: elsewhere"),
        ]);
        assert_eq!(wing(dir.path(), "", None), Ok(Some("mine".into())));
    }

    #[test]
    fn the_home_directory_own_file_is_never_a_project_file() {
        let dir = tree(&[
            (".config/memcastle.toml", "[memcastle]\nwing = \"home\"\n"),
            ("work/x", ""),
        ]);
        assert_eq!(wing(dir.path(), "work", Some(dir.path())), Ok(None));
    }

    #[test]
    fn an_unknown_key_is_refused_so_a_secret_or_a_typo_is_not_ignored() {
        for body in [
            "[memcastle]\nwng = \"x\"\n",
            "[memcastle]\ntoken = \"hunter2\"\n",
            "palace = \"p\"\n",
        ] {
            let dir = tree(&[(".config/memcastle.toml", body)]);
            let error = wing(dir.path(), "", None).unwrap_err();
            assert!(error.contains("memcastle.toml"), "{error}");
            assert!(
                !error.contains("hunter2") || body.contains("token"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_wing_or_room_the_daemon_would_refuse_is_an_error_naming_the_field() {
        for (body, field) in [
            ("[memcastle]\nwing = \"a/b\"\n", "[memcastle] wing"),
            ("[memcastle]\nroom = \"\"\n", "[memcastle] room"),
            (
                "[project]\nname = \"11111111-1111-1111-1111-111111111111\"\n",
                "[project] name",
            ),
        ] {
            let dir = tree(&[(".config/memcastle.toml", body)]);
            let error = wing(dir.path(), "", None).unwrap_err();
            assert!(error.contains(field), "{error}");
        }
    }

    #[test]
    fn a_broken_file_falls_back_to_the_adapters_default_instead_of_failing() {
        let dir = tree(&[(".config/memcastle.toml", "this is = = not toml")]);
        assert_eq!(wing_with_home(dir.path(), None), None);
    }

    #[test]
    fn the_reserved_mining_table_is_accepted_empty_and_refused_with_keys() {
        let ok = tree(&[(
            ".config/memcastle.toml",
            "[mining]\n[memcastle]\nwing = \"w\"\n",
        )]);
        assert_eq!(wing(ok.path(), "", None), Ok(Some("w".into())));
        let bad = tree(&[(".config/memcastle.toml", "[mining]\nchunk_chars = 5\n")]);
        assert!(wing(bad.path(), "", None).is_err());
    }
}
