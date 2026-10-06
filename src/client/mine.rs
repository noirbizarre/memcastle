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
    use std::path::Component;

    let path = absolute(value)?;
    let mut literal = PathBuf::new();
    let mut rest: Vec<String> = Vec::new();
    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        // Once a segment holds a star, everything after it belongs to the pattern, literal or not.
        if !rest.is_empty() || text.contains('*') {
            rest.push(text.into_owned());
        } else {
            literal.push(component);
        }
    }
    let resolved = std::fs::canonicalize(&literal).unwrap_or_else(|_| {
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
    let mut out = resolved.to_string_lossy().into_owned();
    for part in rest {
        if !out.ends_with('/') {
            out.push('/');
        }
        out.push_str(&part);
    }
    Ok(out)
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

    #[test]
    fn a_path_option_is_resolved_the_way_a_programs_working_directory_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(real.join("app/src")).unwrap();
        std::os::unix::fs::symlink(real.join("app"), real.join("link")).unwrap();
        let app = real.join("app").display().to_string();

        for spelling in [
            format!("{}/", app),
            format!("{}/./src/..", app),
            format!("{}/link/../app", real.display()),
            format!("{}//app", real.display()),
            format!("{}/link", real.display()),
        ] {
            assert_eq!(resolve_pattern(&spelling).unwrap(), app, "{spelling}");
        }
    }

    #[test]
    fn a_pattern_keeps_what_follows_its_first_star_and_resolves_what_precedes_it() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(real.join("work")).unwrap();
        std::os::unix::fs::symlink(real.join("work"), real.join("ws")).unwrap();
        let work = real.join("work").display().to_string();

        assert_eq!(
            resolve_pattern(&format!("{}/ws/*", real.display())).unwrap(),
            format!("{work}/*")
        );
        assert_eq!(
            resolve_pattern(&format!("{}/ws/*/src/../x*", real.display())).unwrap(),
            format!("{work}/*/src/../x*"),
            "after a star nothing is known, so nothing is resolved"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_is_only_cleaned_up_lexically() {
        assert_eq!(
            resolve_pattern("/no/such/./place/../dir/").unwrap(),
            "/no/such/dir"
        );
        assert_eq!(resolve_pattern("/no/such/p*").unwrap(), "/no/such/p*");
    }

    #[test]
    fn a_relative_pattern_is_made_absolute_against_the_shells_directory() {
        let resolved = resolve_pattern("workspaces/*").unwrap();
        assert!(
            Path::new(&resolved).is_absolute() && resolved.ends_with("/workspaces/*"),
            "{resolved}"
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
