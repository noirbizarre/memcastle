//! The argument grammar of `memcastle mine`: what a person types, as the one request the daemon reads.
//!
//! ```text
//! memcastle mine <SOURCE> [TARGET] [KEY=VALUE]...
//! memcastle mine <PATH>   [KEY=VALUE]...          (shorthand for `directory <PATH>`)
//! ```
//!
//! Pure: it reads no daemon and no palace. The one thing it needs from the outside, which sources exist and what they
//! accept, is handed in, so the grammar is testable without either (docs/adr/041). It lives with the client because
//! it is the CLI's own convenience: the daemon, MCP and REST all take the resolved request and no grammar.

use std::path::{Path, PathBuf};

use crate::domain::{MiningSource, OptionKind, Options, is_option_key, is_valid_source_name};
use crate::error::{Error, Result};
use crate::mining::AdapterInfo;

/// The name that stands for the built-in directory source.
const DIRECTORY: &str = "directory";

/// What was typed after `memcastle mine`, split into its three kinds of word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The first word: a source's name, or a directory.
    pub target: String,
    /// The one bare word after it, when there is one: where within the source to read.
    pub positional: Option<String>,
    /// The `key=value` words.
    pub options: Options,
}

/// Split the words after `memcastle mine`.
///
/// A word is an option when it is `key=value` with a key that [`is_option_key`], so `./a=b` and `/data/x=1` are still
/// paths. At most one bare word follows the target, and a key may be given once.
///
/// # Errors
///
/// [`Error::InvalidInput`] naming the word, for a second bare word or a repeated key.
pub fn parse(target: String, rest: Vec<String>) -> Result<Parsed> {
    let mut positional: Option<String> = None;
    let mut options = Options::new();
    for word in rest {
        match word.split_once('=') {
            Some((key, value)) if is_option_key(key) => {
                if options.insert(key.to_string(), value.to_string()).is_some() {
                    return Err(Error::invalid_input(
                        "options",
                        format!("`{key}` is given twice; a run takes each option once"),
                    ));
                }
            }
            _ => {
                if let Some(first) = &positional {
                    return Err(Error::invalid_input(
                        "args",
                        format!(
                            "unexpected `{word}` after `{first}`; give one place to read, then `key=value` options"
                        ),
                    ));
                }
                positional = Some(word);
            }
        }
    }
    Ok(Parsed {
        target,
        positional,
        options,
    })
}

/// Whether `target` can only be a directory: it has a separator, or it is not shaped like a source's name (`.`,
/// `..`, `~/x`, `Notes`).
///
/// Source names are lowercase letters, digits and `-`, so everything else is unambiguous without asking anyone.
#[must_use]
pub fn is_path_like(target: &str) -> bool {
    target.contains(['/', '\\']) || !is_valid_source_name(target)
}

impl Parsed {
    /// Whether resolving this needs the daemon's list of sources: it names something that might be a source, and not
    /// the built-in `directory`, which needs no lookup.
    #[must_use]
    pub fn needs_sources(&self) -> bool {
        !is_path_like(&self.target) && self.target != DIRECTORY
    }
}

/// Make `path` absolute against this shell's working directory.
///
/// The daemon would otherwise resolve `./project` against its own, which is wherever it was started and usually not
/// where the user is standing.
fn absolute(path: &str) -> Result<PathBuf> {
    std::path::absolute(path).map_err(|source| Error::io(path.to_string(), source))
}

/// A `path` option's value as the tools that record it store a directory: absolute, with no `.`, `..`, doubled or
/// trailing separator, and with links resolved, since a program's working directory is the physical one.
///
/// A `*` makes the value a pattern (`workspaces/*`): the part before the first segment holding one is resolved, as a
/// directory is, and the rest is kept as typed. A part that does not exist (a project deleted since its sessions were
/// recorded) is cleaned up lexically instead, which is the most that can be said of it.
fn resolve_pattern(value: &str) -> Result<String> {
    use std::ffi::OsString;
    use std::path::Component;

    let path = absolute_keeping_dots(value)?;
    let mut literal = PathBuf::new();
    let mut rest: Vec<OsString> = Vec::new();
    for component in path.components() {
        // Once a segment holds a star, everything after it belongs to the pattern, literal or not.
        if !rest.is_empty() || component.as_os_str().to_string_lossy().contains('*') {
            rest.push(component.as_os_str().to_os_string());
        } else {
            literal.push(component);
        }
    }
    let resolved = std::fs::canonicalize(&literal)
        .map(without_verbatim_prefix)
        .unwrap_or_else(|_| {
            // `absolute` leaves `..` in place, so it is resolved here against what precedes it.
            let mut clean = PathBuf::new();
            for component in literal.components() {
                match component {
                    Component::ParentDir => {
                        clean.pop();
                    }
                    other => clean.push(other),
                }
            }
            clean
        });
    // Joined as a path, so the separator is the platform's own: `\` on Windows, which is how a program there records
    // its working directory.
    let mut out = resolved;
    out.extend(collapse_dots(rest));
    Ok(out.to_string_lossy().into_owned())
}

/// [`absolute`], except that a `..` is left where it is.
///
/// `std::path::absolute` asks the operating system on Windows, which resolves every `..` itself, even one after a `*`
/// that it cannot know the extent of; elsewhere it leaves them. A relative or already absolute path is made absolute
/// here, by joining the shell's directory, so [`collapse_dots`] is the one place `..` is decided. What is neither
/// (`\work` or `C:work` on Windows) needs the system's own rules and keeps them.
fn absolute_keeping_dots(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if !path.has_root()
        && path
            .components()
            .next()
            .is_none_or(|c| !matches!(c, std::path::Component::Prefix(_)))
    {
        let here = std::env::current_dir().map_err(|source| Error::io(".".to_string(), source))?;
        return Ok(here.join(path));
    }
    absolute(value)
}

/// The segments of a pattern with each `..` applied to the literal segment before it, as the sources do to the value
/// they compare with.
///
/// `std::path::absolute` collapses `..` on Windows and keeps it elsewhere, so this is done here, once, for both: the
/// same words must give the same request on every platform. A `..` that follows a `*` is kept, since what the star
/// stands for is not known.
fn collapse_dots(parts: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for part in parts {
        let literal_before = out
            .last()
            .is_some_and(|last| last != ".." && !last.to_string_lossy().contains('*'));
        if part == ".." && literal_before {
            out.pop();
        } else {
            out.push(part);
        }
    }
    out
}

/// `path` without the `\\?\` prefix `canonicalize` gives a Windows path, which no program records as its working
/// directory (`\\?\C:\work` is `C:\work`, and `\\?\UNC\host\share` is `\\host\share`). A path that has none, and
/// every path off Windows, is returned as it is.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match text.strip_prefix(r"\\?\") {
        // Only a drive form: any other verbatim path (a device) is not something to rewrite.
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// The request `parsed` stands for, given the sources the daemon knows (`None` when [`Parsed::needs_sources`] is
/// false).
///
/// The order is the one docs/adr/041 fixes: a path-looking word is a directory; `directory` is the built-in source;
/// a known source's name is that source; an existing local directory is a directory; anything else is an error that
/// lists the sources.
///
/// # Errors
///
/// [`Error::InvalidInput`] when a directory is missing or given twice, or when the word is neither a source nor a
/// directory.
pub fn build(parsed: Parsed, adapters: Option<&[AdapterInfo]>) -> Result<(MiningSource, Options)> {
    let Parsed {
        target,
        positional,
        mut options,
    } = parsed;

    let directory = |path: &str, extra: Option<String>| -> Result<MiningSource> {
        if let Some(extra) = extra {
            return Err(Error::invalid_input(
                "args",
                format!("`{path}` is the directory to mine; `{extra}` is one place too many"),
            ));
        }
        Ok(MiningSource::Directory {
            path: absolute(path)?,
        })
    };

    if is_path_like(&target) {
        return Ok((directory(&target, positional)?, options));
    }
    if target == DIRECTORY {
        let Some(path) = positional else {
            return Err(Error::invalid_input(
                "path",
                "mining a directory needs a path: `memcastle mine directory <path>`, or just `memcastle mine <path>`",
            ));
        };
        return Ok((directory(&path, None)?, options));
    }

    let adapters = adapters.unwrap_or_default();
    let Some(adapter) = adapters.iter().find(|adapter| adapter.name == target) else {
        // A plain word that is not a source: a directory with an ordinary name, as `memcastle mine docs` always meant.
        if Path::new(&target).is_dir() {
            return Ok((directory(&target, positional)?, options));
        }
        let known = adapters
            .iter()
            .map(|adapter| adapter.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::invalid_input(
            "source",
            format!(
                "`{target}` is neither a source nor a directory; known sources: {known} \
                 (a directory with this name is `memcastle mine ./{target}`)"
            ),
        ));
    };

    // A value naming a place on this machine is made absolute and cleaned up here, for the reason `absolute` gives and
    // so that it reads the way the tools that recorded it wrote it down; which values those are is what the source
    // declared, not something the CLI knows by name.
    for spec in &adapter.options {
        if spec.kind == OptionKind::Path
            && let Some(value) = options.get_mut(&spec.name)
        {
            *value = resolve_pattern(value)?;
        }
    }
    let reads_locator = adapter
        .permissions
        .filesystem
        .read
        .iter()
        .any(|entry| entry == "locator");
    let locator = match positional {
        Some(place) if reads_locator => Some(absolute(&place)?.display().to_string()),
        other => other,
    };
    Ok((
        MiningSource::Named {
            source: target,
            locator,
        },
        options,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{OptionSpec, Permissions, SourceCapabilities, SourceOrigin, SourceState};

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    fn parsed(target: &str, rest: &[&str]) -> Parsed {
        parse(target.to_string(), words(rest)).unwrap()
    }

    fn adapter(name: &str, options: Vec<OptionSpec>, reads_locator: bool) -> AdapterInfo {
        let mut permissions = Permissions::default();
        if reads_locator {
            permissions.filesystem.read = vec!["locator".to_string()];
        }
        AdapterInfo {
            name: name.to_string(),
            description: "demo".to_string(),
            capabilities: SourceCapabilities::default(),
            origin: SourceOrigin::Package,
            version: None,
            state: SourceState::Enabled,
            unavailable_reason: None,
            permissions,
            registry: None,
            signed_by: None,
            auth: None,
            options,
        }
    }

    fn sources() -> Vec<AdapterInfo> {
        vec![
            adapter(
                "opencode",
                vec![
                    OptionSpec::new("since", "from", OptionKind::Date),
                    OptionSpec::new("dir", "where", OptionKind::Path),
                ],
                false,
            ),
            adapter("pi", vec![], true),
        ]
    }

    #[test]
    fn key_value_words_are_options_and_the_other_word_is_the_place_to_read() {
        let p = parsed("opencode", &["since=2026-09", "dir=/work/app"]);
        assert_eq!(p.positional, None);
        assert_eq!(p.options["since"], "2026-09");
        assert_eq!(p.options["dir"], "/work/app");
        let p = parsed("pi", &["/backups/sessions", "since=2026-09"]);
        assert_eq!(p.positional.as_deref(), Some("/backups/sessions"));
    }

    #[test]
    fn a_path_that_contains_an_equals_sign_is_still_a_path() {
        let p = parsed("pi", &["/data/a=b"]);
        assert_eq!(p.positional.as_deref(), Some("/data/a=b"));
        assert!(p.options.is_empty());
    }

    #[test]
    fn two_places_to_read_are_refused_naming_the_extra_word() {
        let error = parse("pi".into(), words(&["/a", "/b"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`/b`") && error.contains("`/a`"), "{error}");
    }

    #[test]
    fn an_option_given_twice_is_refused_rather_than_one_silently_winning() {
        let error = parse("pi".into(), words(&["since=2026", "since=2027"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("`since`") && error.contains("twice"),
            "{error}"
        );
    }

    #[test]
    fn only_a_word_shaped_like_a_source_name_can_be_a_source() {
        for path in [".", "..", "./pi", "~/x", "/data", "Notes", "a/b", "my_dir"] {
            assert!(is_path_like(path), "{path}");
        }
        for name in ["pi", "opencode", "directory", "my-source2"] {
            assert!(!is_path_like(name), "{name}");
        }
    }

    #[test]
    fn a_path_is_the_directory_shorthand() {
        let (source, options) = build(parsed("./project", &["since=2026-09"]), None).unwrap();
        let MiningSource::Directory { path } = source else {
            panic!("a path mines a directory");
        };
        assert!(
            path.is_absolute(),
            "the daemon must not resolve it against its own directory"
        );
        assert_eq!(options["since"], "2026-09");
    }

    #[test]
    fn the_directory_source_takes_its_path_as_the_one_place() {
        let (source, _) = build(parsed("directory", &["./project"]), None).unwrap();
        assert!(matches!(source, MiningSource::Directory { path } if path.is_absolute()));
        let error = build(parsed("directory", &[]), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs a path"), "{error}");
    }

    #[test]
    fn a_path_with_a_second_place_is_refused() {
        let error = build(parsed("./a", &["./b"]), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`./b`"), "{error}");
    }

    #[test]
    fn a_source_name_is_that_source_with_its_options() {
        let p = parsed("opencode", &["since=2026-09", "dir=."]);
        assert!(p.needs_sources());
        let (source, options) = build(p, Some(&sources())).unwrap();
        assert!(matches!(
            source,
            MiningSource::Named { ref source, locator: None } if source == "opencode"
        ));
        assert_eq!(options["since"], "2026-09", "a date is passed as typed");
        assert!(
            Path::new(&options["dir"]).is_absolute(),
            "a `path` option is made absolute against the shell's directory"
        );
    }

    /// A temporary directory's canonical path, which is where a program started inside it records its working directory.
    fn real_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let real = without_verbatim_prefix(std::fs::canonicalize(dir.path()).unwrap());
        (dir, real)
    }

    /// `parts` joined under `base` as a path, so every expectation uses the platform's own separator.
    fn under(base: &Path, parts: &[&str]) -> String {
        let mut path = base.to_path_buf();
        path.extend(parts);
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_path_option_is_cleaned_up_the_way_a_programs_working_directory_is_recorded() {
        let (_guard, real) = real_dir();
        std::fs::create_dir_all(real.join("app").join("src")).unwrap();
        let app = under(&real, &["app"]);

        for spelling in [
            format!("{app}{}", std::path::MAIN_SEPARATOR),
            under(&real, &["app", ".", "src", ".."]),
            under(&real, &["app", "src", "..", "..", "app"]),
        ] {
            assert_eq!(resolve_pattern(&spelling).unwrap(), app, "{spelling}");
        }
    }

    #[test]
    fn the_result_never_carries_the_verbatim_prefix_windows_canonicalization_adds() {
        let (_guard, real) = real_dir();
        let resolved = resolve_pattern(&real.to_string_lossy()).unwrap();
        assert!(!resolved.starts_with(r"\\?\"), "{resolved}");
        assert_eq!(resolved, real.to_string_lossy());
    }

    #[test]
    fn a_verbatim_prefix_is_removed_from_a_drive_path_and_a_share_and_nothing_else_is_touched() {
        let strip = |text: &str| {
            without_verbatim_prefix(PathBuf::from(text))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(strip(r"\\?\C:\work\app"), r"C:\work\app");
        assert_eq!(strip(r"\\?\UNC\host\share\app"), r"\\host\share\app");
        assert_eq!(strip(r"\\?\GLOBALROOT\device"), r"\\?\GLOBALROOT\device");
        assert_eq!(strip("/work/app"), "/work/app");
        assert_eq!(strip(r"C:\work"), r"C:\work");
    }

    // Links need a privilege on Windows, so only the creation of one is Unix-only: everything else here runs there too.
    #[cfg(unix)]
    #[test]
    fn a_link_in_the_part_that_exists_is_resolved() {
        let (_guard, real) = real_dir();
        std::fs::create_dir_all(real.join("app")).unwrap();
        std::os::unix::fs::symlink(real.join("app"), real.join("link")).unwrap();
        let app = under(&real, &["app"]);

        assert_eq!(resolve_pattern(&under(&real, &["link"])).unwrap(), app);
        assert_eq!(
            resolve_pattern(&under(&real, &["link", "..", "app"])).unwrap(),
            app
        );
    }

    #[test]
    fn a_pattern_keeps_what_follows_its_first_star_and_resolves_what_precedes_it() {
        let (_guard, real) = real_dir();
        std::fs::create_dir_all(real.join("work")).unwrap();

        assert_eq!(
            resolve_pattern(&under(&real, &["work", ".", "*"])).unwrap(),
            under(&real, &["work", "*"]),
            "what precedes the star is cleaned up"
        );
        assert_eq!(
            resolve_pattern(&under(&real, &["work", "*", "src", "..", "x*"])).unwrap(),
            under(&real, &["work", "*", "x*"]),
            "a `..` applies to the literal segment before it, on every platform"
        );
        assert_eq!(
            resolve_pattern(&under(&real, &["work", "*", "..", "x*"])).unwrap(),
            under(&real, &["work", "*", "..", "x*"]),
            "but not to a star, whose extent is not known"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_is_only_cleaned_up_lexically() {
        let (_guard, real) = real_dir();
        assert_eq!(
            resolve_pattern(&under(&real, &["no", "such", ".", "place", "..", "dir"])).unwrap(),
            under(&real, &["no", "such", "dir"])
        );
        assert_eq!(
            resolve_pattern(&under(&real, &["no", "such", "p*"])).unwrap(),
            under(&real, &["no", "such", "p*"])
        );
    }

    #[test]
    fn a_relative_pattern_is_made_absolute_against_the_shells_directory() {
        let resolved = PathBuf::from(
            resolve_pattern(&format!("workspaces{}*", std::path::MAIN_SEPARATOR)).unwrap(),
        );
        assert!(resolved.is_absolute(), "{resolved:?}");
        assert_eq!(resolved.file_name().unwrap(), "*");
        assert_eq!(
            resolved.parent().unwrap().file_name().unwrap(),
            "workspaces"
        );
    }

    #[test]
    fn a_source_that_reads_its_locator_as_a_directory_gets_it_absolute() {
        let (source, _) = build(parsed("pi", &["sessions"]), Some(&sources())).unwrap();
        let MiningSource::Named {
            locator: Some(locator),
            ..
        } = source
        else {
            panic!("a locator was given");
        };
        assert!(Path::new(&locator).is_absolute());
    }

    #[test]
    fn an_unknown_word_that_is_not_a_directory_lists_the_sources() {
        let error = build(parsed("no-such-thing", &[]), Some(&sources()))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("opencode, pi") && error.contains("./no-such-thing"),
            "{error}"
        );
    }

    #[test]
    fn a_plain_word_naming_an_existing_directory_is_still_a_directory() {
        // `src` exists in the directory tests run from, and is not a source: `memcastle mine docs` always meant a
        // directory, and still does.
        let (source, _) = build(parsed("src", &[]), Some(&sources())).unwrap();
        assert!(matches!(source, MiningSource::Directory { .. }));
    }

    #[test]
    fn a_known_source_wins_over_a_directory_of_the_same_name() {
        // `pi` is a source, so a stray `pi/` directory beside the user does not capture it.
        let (source, _) = build(parsed("pi", &[]), Some(&sources())).unwrap();
        assert!(matches!(source, MiningSource::Named { .. }));
    }
}
