//! Reading and rewriting the `[[miners]]` section of the configuration file, and nothing else in it.
//!
//! The daemon is the only writer (`docs/adr/037-persistent-miner-configuration.md`), and it edits the file in place
//! with `toml_edit` so everything it does not own survives: comments, the order of the other tables, the formatting
//! of an entry that did not change. Environment and command-line overrides are never part of this file, which is why
//! the daemon edits it and does not serialise its resolved [`Config`](super::Config).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item};

use crate::domain::{MinerDefinition, validate_miners};
use crate::error::{Error, Result};

/// What a file looked like when it was read: cheap to compare, so the daemon can tell "someone edited it" without
/// reading it on every request, and a write can refuse to overwrite an edit it has not seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// The miners in a file, and the file's stamp at that moment (`None` when the file does not exist yet).
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Every entry, in file order, already validated.
    pub miners: Vec<MinerDefinition>,
    /// The stamp to hand back to [`write`].
    pub stamp: Option<FileStamp>,
}

/// The one key of the configuration file this module reads.
#[derive(Deserialize)]
struct MinersOnly {
    #[serde(default)]
    miners: Vec<MinerDefinition>,
}

/// Borrowed counterpart of [`MinersOnly`], to render a single entry exactly as the serialiser would write it.
#[derive(Serialize)]
struct OneMiner<'a> {
    miners: [&'a MinerDefinition; 1],
}

fn file_error(path: &Path, reason: impl std::fmt::Display) -> Error {
    Error::MinerConfigFile {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

/// The stamp of `path`, or `None` when it does not exist.
///
/// # Errors
///
/// Returns [`Error::MinerConfigFile`] when the file exists but cannot be inspected.
pub fn stamp(path: &Path) -> Result<Option<FileStamp>> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(FileStamp {
            modified: meta.modified().ok(),
            len: meta.len(),
        })),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(file_error(path, source)),
    }
}

/// Parse the miners out of configuration text, ignoring every other section.
fn parse(path: &Path, text: &str) -> Result<Vec<MinerDefinition>> {
    let MinersOnly { miners } = toml::from_str(text).map_err(|e| {
        file_error(
            path,
            format!("the `[[miners]]` section does not parse: {e}"),
        )
    })?;
    validate_miners(&miners).map_err(|reason| file_error(path, reason))?;
    Ok(miners)
}

/// Read the miners from `path`. A file that does not exist has none.
///
/// # Errors
///
/// Returns [`Error::MinerConfigFile`] when the file cannot be read, or its `[[miners]]` section does not parse or
/// does not validate.
pub fn read(path: &Path) -> Result<Loaded> {
    // Stamped *before* reading: an edit between the two makes the stamp stale, so the next refresh reads again,
    // where the other order could record a stamp for text that was never read.
    let before = stamp(path)?;
    if before.is_none() {
        return Ok(Loaded {
            miners: Vec::new(),
            stamp: None,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|source| file_error(path, source))?;
    Ok(Loaded {
        miners: parse(path, &text)?,
        stamp: before,
    })
}

/// Render one entry the way the serialiser writes it, as a table ready to sit in `[[miners]]`.
fn render(path: &Path, miner: &MinerDefinition) -> Result<toml_edit::Table> {
    let text = toml::to_string(&OneMiner { miners: [miner] }).map_err(|e| file_error(path, e))?;
    let doc: DocumentMut = text.parse().map_err(|e| file_error(path, e))?;
    let mut table = doc
        .get("miners")
        .and_then(Item::as_array_of_tables)
        .and_then(|tables| tables.get(0))
        .cloned()
        .ok_or_else(|| file_error(path, "an entry could not be rendered"))?;
    forget_positions(&mut table);
    Ok(table)
}

/// Drop the positions a table was given in the throwaway document it was rendered in, so it takes its place from
/// where it is inserted (right after what precedes it) instead of from a document it was never part of.
fn forget_positions(table: &mut toml_edit::Table) {
    table.set_position(None);
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(inner) => forget_positions(inner),
            Item::ArrayOfTables(inner) => inner.iter_mut().for_each(forget_positions),
            _ => {}
        }
    }
}

/// Make the file's `[[miners]]` section equal `miners`, leaving the rest of the file as it was.
///
/// An entry that is unchanged is kept byte for byte (its comments too); a changed one is re-rendered under the
/// comments that sat above it; entries not in `miners` are removed; new ones are appended. The file is replaced
/// atomically (a temporary file in the same directory, then a rename), keeping its permissions, because it may hold
/// an authentication token and a half-written configuration would stop the next start.
///
/// `expected` is what the caller last saw ([`Loaded::stamp`]): if the file has changed since, nothing is written,
/// because overwriting would discard an edit the caller never read.
///
/// Returns the new stamp.
///
/// # Errors
///
/// Returns [`Error::MinerConfigFile`] when the file changed since `expected`, cannot be parsed or written, or holds
/// `miners` in a shape that is not a list of tables.
pub fn write(
    path: &Path,
    expected: Option<FileStamp>,
    miners: &[MinerDefinition],
) -> Result<Option<FileStamp>> {
    // `validate_miners` again: a write is the last place a bad definition can be stopped before it is persisted.
    validate_miners(miners).map_err(|reason| file_error(path, reason))?;
    if stamp(path)? != expected {
        return Err(file_error(
            path,
            "it changed while the daemon was editing it; run `memcastle miner reload` to read the change, then retry",
        ));
    }

    let text = if expected.is_some() {
        std::fs::read_to_string(path).map_err(|source| file_error(path, source))?
    } else {
        String::new()
    };
    // The same text, parsed the way the daemon reads it, so entries line up with the tables by index.
    let existing = parse(path, &text)?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| file_error(path, format!("it is not valid TOML: {e}")))?;

    if miners.is_empty() {
        doc.remove("miners");
    } else {
        // `miners = [{ ... }]` is valid TOML that `read` accepts; it becomes `[[miners]]` when rewritten.
        if let Some(item) = doc.remove("miners") {
            let tables = item.into_array_of_tables().map_err(|_| {
                file_error(
                    path,
                    "`miners` is not a list of tables; write it as `[[miners]]`",
                )
            })?;
            doc.insert("miners", Item::ArrayOfTables(tables));
        } else {
            doc.insert("miners", Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let tables = doc
            .get_mut("miners")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| file_error(path, "the `[[miners]]` section could not be edited"))?;

        // Edited in place, never rebuilt: `toml_edit` orders tables by the position they had in the file, and a
        // table that was rendered elsewhere carries a position of its own, which would throw it to the top.
        let mut current = existing;
        for index in (0..current.len()).rev() {
            if !miners.iter().any(|m| m.name == current[index].name) {
                tables.remove(index);
                current.remove(index);
            }
        }
        for miner in miners {
            match current.iter().position(|c| c.name == miner.name) {
                // Unchanged: left alone, so its comments and formatting are exactly as the user wrote them.
                Some(index) if current[index] == *miner => {}
                Some(index) => {
                    let mut table = render(path, miner)?;
                    if let Some(old) = tables.get_mut(index) {
                        // The comments above the old entry are the user's, and describe the miner, not its fields.
                        *table.decor_mut() = old.decor().clone();
                        *old = table;
                    }
                }
                None => {
                    let mut table = render(path, miner)?;
                    if !text.trim().is_empty() {
                        // Set apart from whatever precedes it, as a person writing the entry would.
                        table.decor_mut().set_prefix("\n");
                    }
                    tables.push(table);
                }
            }
        }
    }

    replace_atomically(path, &doc.to_string())?;
    stamp(path)
}

/// Write `text` to `path` by renaming a finished temporary file over it.
fn replace_atomically(path: &Path, text: &str) -> Result<()> {
    // A symlinked configuration (a dotfiles manager's) is edited where it points: renaming over the link would
    // replace the link with a regular file and detach it from its repository.
    let target: PathBuf = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| file_error(path, "it has no parent directory"))?;
    std::fs::create_dir_all(dir).map_err(|source| file_error(path, source))?;

    let mut temp =
        tempfile::NamedTempFile::new_in(dir).map_err(|source| file_error(path, source))?;
    std::io::Write::write_all(&mut temp, text.as_bytes())
        .map_err(|source| file_error(path, source))?;
    if let Ok(meta) = std::fs::metadata(&target) {
        // Without this the rewritten file would get the temporary file's private default, or lose a deliberately
        // restrictive mode, and either is a surprise on a file that may hold a token.
        std::fs::set_permissions(temp.path(), meta.permissions())
            .map_err(|source| file_error(path, source))?;
    }
    temp.persist(&target)
        .map_err(|e| file_error(path, e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn miner(name: &str) -> MinerDefinition {
        toml::from_str(&format!(
            "name = \"{name}\"\nsource = \"directory\"\nlocator = \"/data/{name}\"\n"
        ))
        .expect("a miner")
    }

    const SAMPLE: &str = "\
# The palace lives here.
[palace]
path = \"/srv/palace\"   # keep this comment

# first miner, hand written
[[miners]]
name = \"docs\"
source = \"directory\"
locator = \"/data/docs\"   # trailing note

[server]
port = 4000
";

    fn setup(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write");
        (dir, path)
    }

    #[test]
    fn a_missing_file_has_no_miners_and_no_stamp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let loaded = read(&dir.path().join("absent.toml")).expect("read");
        assert!(loaded.miners.is_empty() && loaded.stamp.is_none());
    }

    #[test]
    fn other_sections_are_ignored_when_reading_miners() {
        let (_dir, path) = setup(SAMPLE);
        let loaded = read(&path).expect("read");
        assert_eq!(loaded.miners.len(), 1);
        assert_eq!(loaded.miners[0].name, "docs");
    }

    #[test]
    fn a_bad_miner_section_is_named_and_refused() {
        let (_dir, path) = setup("[[miners]]\nname = \"A\"\nsource = \"x\"\n");
        let error = read(&path).expect_err("an invalid name").to_string();
        assert!(error.contains("not a valid miner name"), "{error}");
        let (_dir, path) = setup("[[miners]]\nname = \"a\"\nsource = \"x\"\nenable = true\n");
        assert!(read(&path).is_err(), "an unknown key is refused");
    }

    #[test]
    fn adding_a_miner_keeps_every_comment_and_the_other_tables() {
        let (_dir, path) = setup(SAMPLE);
        let loaded = read(&path).expect("read");
        let mut miners = loaded.miners;
        miners.push(miner("notes"));
        write(&path, loaded.stamp, &miners).expect("write");

        let text = std::fs::read_to_string(&path).expect("read back");
        for kept in [
            "# The palace lives here.",
            "# keep this comment",
            "# first miner, hand written",
            "# trailing note",
            "port = 4000",
        ] {
            assert!(text.contains(kept), "`{kept}` was lost:\n{text}");
        }
        assert_eq!(read(&path).expect("reread").miners, miners);
    }

    #[test]
    fn a_changed_miner_keeps_the_comments_above_it() {
        let (_dir, path) = setup(SAMPLE);
        let loaded = read(&path).expect("read");
        let mut miners = loaded.miners;
        miners[0].enabled = false;
        write(&path, loaded.stamp, &miners).expect("write");

        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("# first miner, hand written"), "{text}");
        assert!(
            text.contains("# keep this comment") && text.contains("port = 4000"),
            "{text}"
        );
        assert!(!read(&path).expect("reread").miners[0].enabled);
    }

    #[test]
    fn nested_tables_are_written_as_tables_and_read_back_equal() {
        let (_dir, path) = setup(SAMPLE);
        let loaded = read(&path).expect("read");
        let mut rich = miner("signal");
        rich.scope = json!({"contacts": ["+336"], "groups": ["MemCastle"]})
            .as_object()
            .cloned()
            .expect("map");
        rich.config = json!({"window": {"days": 7}})
            .as_object()
            .cloned()
            .expect("map");
        rich.trigger = toml::from_str("type = \"event\"\nhook = \"x\"\n").expect("trigger");
        rich.credential = Some(crate::domain::CredentialRef::Env {
            name: "SIGNAL_TOKEN".to_string(),
        });
        let mut miners = loaded.miners;
        miners.push(rich);
        write(&path, loaded.stamp, &miners).expect("write");

        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(
            text.contains("[miners.scope]") || text.contains("scope"),
            "{text}"
        );
        assert_eq!(read(&path).expect("reread").miners, miners, "{text}");
        // The unrelated tables still come out in their own place.
        assert!(
            text.find("[server]").is_some() && text.find("[palace]").is_some(),
            "{text}"
        );
    }

    #[test]
    fn removing_the_last_miner_removes_the_section() {
        let (_dir, path) = setup(SAMPLE);
        let loaded = read(&path).expect("read");
        write(&path, loaded.stamp, &[]).expect("write");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(
            !text.contains("miners") && text.contains("[server]"),
            "{text}"
        );
    }

    #[test]
    fn a_missing_file_is_created_with_the_first_miner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.toml");
        write(&path, None, &[miner("docs")]).expect("write");
        assert_eq!(read(&path).expect("read").miners, vec![miner("docs")]);
    }

    #[test]
    fn a_file_edited_since_it_was_read_is_never_overwritten() {
        let (_dir, path) = setup(SAMPLE);
        let loaded = read(&path).expect("read");
        std::fs::write(&path, format!("{SAMPLE}# edited by hand\n")).expect("edit");
        let error = write(&path, loaded.stamp, &[miner("notes")]).expect_err("a stale stamp");
        assert!(error.to_string().contains("changed while"), "{error}");
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("# edited by hand")
        );
    }

    #[test]
    fn an_inline_miners_array_is_rewritten_as_tables() {
        let (_dir, path) = setup("miners = [{ name = \"a\", source = \"directory\" }]\n");
        let loaded = read(&path).expect("read");
        let mut miners = loaded.miners;
        miners.push(miner("b"));
        write(&path, loaded.stamp, &miners).expect("write");
        assert_eq!(read(&path).expect("reread").miners.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_restrictive_file_mode_survives_a_rewrite() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = setup(SAMPLE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let loaded = read(&path).expect("read");
        write(&path, loaded.stamp, &[miner("a")]).expect("write");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file may hold a token");
    }
}
