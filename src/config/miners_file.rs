//! Reading and rewriting the `[[miners]]` and `[[triggers]]` sections of the configuration file, and nothing else in it.
//!
//! The daemon is the only writer (`docs/adr/037-persistent-miner-configuration.md`, `docs/adr/043-source-triggers.md`),
//! and it edits the file in place with `toml_edit` so everything it does not own survives: comments, the order of the
//! other tables, the formatting of an entry that did not change. Environment and command-line overrides are never part
//! of this file, which is why the daemon edits it and does not serialise its resolved [`Config`](super::Config).
//!
//! The two sections share one file, so they share one stamp (what the file looked like) and one write lock: a miner
//! write and a trigger write in the same moment are applied one after the other, never interleaved.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;
use toml_edit::{ArrayOfTables, DocumentMut, Item};

use crate::domain::{MinerDefinition, TriggerDefinition, validate_miners, validate_triggers};
use crate::error::{Error, Result};

/// Serialises the daemon's own writes to the file: two registries (miners, triggers) edit the same text, and the
/// second must read what the first wrote, not the text it started from.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

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

/// The triggers in a file, and the file's stamp at that moment (`None` when the file does not exist yet).
#[derive(Debug, Clone)]
pub struct LoadedTriggers {
    /// Every entry, in file order, already validated.
    pub triggers: Vec<TriggerDefinition>,
    /// The stamp to hand back to [`write_triggers`].
    pub stamp: Option<FileStamp>,
}

/// One kind of entry the file holds as an array of tables.
trait Section: Serialize + DeserializeOwned + PartialEq + Clone {
    /// The array's key in the file.
    const KEY: &'static str;
    /// The entry's name, which is its identity within the section.
    fn name(&self) -> &str;
    /// Check every entry and that their names are unique.
    fn validate_all(entries: &[Self]) -> std::result::Result<(), String>;
}

impl Section for MinerDefinition {
    const KEY: &'static str = "miners";
    fn name(&self) -> &str {
        &self.name
    }
    fn validate_all(entries: &[Self]) -> std::result::Result<(), String> {
        validate_miners(entries)
    }
}

impl Section for TriggerDefinition {
    const KEY: &'static str = "triggers";
    fn name(&self) -> &str {
        &self.name
    }
    fn validate_all(entries: &[Self]) -> std::result::Result<(), String> {
        validate_triggers(entries)
    }
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

/// Parse one section out of configuration text, ignoring every other section.
fn parse<T: Section>(path: &Path, text: &str) -> Result<Vec<T>> {
    let mut table: toml::Table = toml::from_str(text).map_err(|e| {
        file_error(
            path,
            format!("the configuration does not parse as TOML: {e}"),
        )
    })?;
    let entries: Vec<T> = match table.remove(T::KEY) {
        None => Vec::new(),
        Some(value) => value.try_into().map_err(|e| {
            file_error(
                path,
                format!("the `[[{}]]` section does not parse: {e}", T::KEY),
            )
        })?,
    };
    T::validate_all(&entries).map_err(|reason| file_error(path, reason))?;
    Ok(entries)
}

fn read_section<T: Section>(path: &Path) -> Result<(Vec<T>, Option<FileStamp>)> {
    // Stamped *before* reading: an edit between the two makes the stamp stale, so the next refresh reads again,
    // where the other order could record a stamp for text that was never read.
    let before = stamp(path)?;
    if before.is_none() {
        return Ok((Vec::new(), None));
    }
    let text = std::fs::read_to_string(path).map_err(|source| file_error(path, source))?;
    Ok((parse(path, &text)?, before))
}

/// Read the miners from `path`. A file that does not exist has none.
///
/// # Errors
///
/// Returns [`Error::MinerConfigFile`] when the file cannot be read, or its `[[miners]]` section does not parse or
/// does not validate.
pub fn read(path: &Path) -> Result<Loaded> {
    let (miners, stamp) = read_section(path)?;
    Ok(Loaded { miners, stamp })
}

/// Read the triggers from `path`. A file that does not exist has none.
///
/// # Errors
///
/// Returns [`Error::MinerConfigFile`] when the file cannot be read, or its `[[triggers]]` section does not parse or
/// does not validate.
pub fn read_triggers(path: &Path) -> Result<LoadedTriggers> {
    let (triggers, stamp) = read_section(path)?;
    Ok(LoadedTriggers { triggers, stamp })
}

/// Render one entry the way the serialiser writes it, as a table ready to sit in its array.
fn render<T: Section>(path: &Path, entry: &T) -> Result<toml_edit::Table> {
    let value = toml::Value::try_from(entry).map_err(|e| file_error(path, e))?;
    let mut wrapper = toml::Table::new();
    wrapper.insert(T::KEY.to_string(), toml::Value::Array(vec![value]));
    let text = toml::to_string(&wrapper).map_err(|e| file_error(path, e))?;
    let doc: DocumentMut = text.parse().map_err(|e| file_error(path, e))?;
    let mut table = doc
        .get(T::KEY)
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
    write_section(path, expected, miners)
}

/// Make the file's `[[triggers]]` section equal `triggers`, exactly as [`write`] does for the miners.
///
/// # Errors
///
/// Returns [`Error::MinerConfigFile`] when the file changed since `expected`, cannot be parsed or written, or holds
/// `triggers` in a shape that is not a list of tables.
pub fn write_triggers(
    path: &Path,
    expected: Option<FileStamp>,
    triggers: &[TriggerDefinition],
) -> Result<Option<FileStamp>> {
    write_section(path, expected, triggers)
}

fn write_section<T: Section>(
    path: &Path,
    expected: Option<FileStamp>,
    entries: &[T],
) -> Result<Option<FileStamp>> {
    // A poisoned lock only means another write panicked; the file is replaced atomically, so it is still whole.
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Validated again: a write is the last place a bad definition can be stopped before it is persisted.
    T::validate_all(entries).map_err(|reason| file_error(path, reason))?;
    if stamp(path)? != expected {
        return Err(file_error(
            path,
            "it changed while the daemon was editing it; run `memcastle miner reload` (and `memcastle trigger reload`) \
             to read the change, then retry",
        ));
    }

    let text = if expected.is_some() {
        std::fs::read_to_string(path).map_err(|source| file_error(path, source))?
    } else {
        String::new()
    };
    // The same text, parsed the way the daemon reads it, so entries line up with the tables by index.
    let existing: Vec<T> = parse(path, &text)?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| file_error(path, format!("it is not valid TOML: {e}")))?;

    if entries.is_empty() {
        doc.remove(T::KEY);
    } else {
        // `miners = [{ ... }]` is valid TOML that `read` accepts; it becomes `[[miners]]` when rewritten.
        if let Some(item) = doc.remove(T::KEY) {
            let tables = item.into_array_of_tables().map_err(|_| {
                file_error(
                    path,
                    format!(
                        "`{key}` is not a list of tables; write it as `[[{key}]]`",
                        key = T::KEY
                    ),
                )
            })?;
            doc.insert(T::KEY, Item::ArrayOfTables(tables));
        } else {
            doc.insert(T::KEY, Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let tables = doc
            .get_mut(T::KEY)
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| {
                file_error(
                    path,
                    format!("the `[[{}]]` section could not be edited", T::KEY),
                )
            })?;

        // Edited in place, never rebuilt: `toml_edit` orders tables by the position they had in the file, and a
        // table that was rendered elsewhere carries a position of its own, which would throw it to the top.
        let mut current = existing;
        for index in (0..current.len()).rev() {
            if !entries.iter().any(|m| m.name() == current[index].name()) {
                tables.remove(index);
                current.remove(index);
            }
        }
        for entry in entries {
            match current.iter().position(|c| c.name() == entry.name()) {
                // Unchanged: left alone, so its comments and formatting are exactly as the user wrote them.
                Some(index) if current[index] == *entry => {}
                Some(index) => {
                    let mut table = render(path, entry)?;
                    if let Some(old) = tables.get_mut(index) {
                        // The comments above the old entry are the user's, and describe the entry, not its fields.
                        *table.decor_mut() = old.decor().clone();
                        *old = table;
                    }
                }
                None => {
                    let mut table = render(path, entry)?;
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

    fn trigger(name: &str) -> TriggerDefinition {
        toml::from_str(&format!(
            "name = \"{name}\"\nminer = \"docs\"\ntype = \"poll\"\nevery = \"5m\"\n"
        ))
        .expect("a trigger")
    }

    const WITH_BOTH: &str = "\
# my daemon
[server]
port = 4000

# the docs miner
[[miners]]
name = \"docs\"
source = \"directory\"
locator = \"/data/docs\"

# polls the docs
[[triggers]]
name = \"nightly\"
miner = \"docs\"
type = \"poll\"   # keep me
every = \"1h\"
";

    #[test]
    fn miners_and_triggers_are_read_from_the_same_file_independently() {
        let (_dir, path) = setup(WITH_BOTH);
        assert_eq!(read(&path).expect("miners").miners.len(), 1);
        let triggers = read_triggers(&path).expect("triggers").triggers;
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].name, "nightly");
        assert!(
            !triggers[0].enabled,
            "a trigger the file does not enable is off"
        );
    }

    #[test]
    fn adding_a_trigger_keeps_the_miners_the_comments_and_the_other_tables() {
        let (_dir, path) = setup(WITH_BOTH);
        let loaded = read_triggers(&path).expect("read");
        let mut triggers = loaded.triggers;
        triggers.push(trigger("hourly"));
        write_triggers(&path, loaded.stamp, &triggers).expect("write");

        let text = std::fs::read_to_string(&path).expect("read back");
        for kept in [
            "# my daemon",
            "port = 4000",
            "# the docs miner",
            "# polls the docs",
            "# keep me",
        ] {
            assert!(text.contains(kept), "`{kept}` was lost:\n{text}");
        }
        assert_eq!(read_triggers(&path).expect("reread").triggers, triggers);
        assert_eq!(
            read(&path).expect("miners").miners.len(),
            1,
            "the miners are untouched"
        );
    }

    #[test]
    fn writing_the_miners_leaves_the_triggers_alone_and_the_other_way_round() {
        let (_dir, path) = setup(WITH_BOTH);
        let miners = read(&path).expect("read");
        let mut all = miners.miners.clone();
        all.push(miner("notes"));
        let stamp = write(&path, miners.stamp, &all).expect("write miners");
        assert_eq!(read_triggers(&path).expect("triggers").triggers.len(), 1);
        write_triggers(&path, stamp, &[]).expect("remove all triggers");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(
            !text.contains("triggers") && text.contains("[[miners]]"),
            "{text}"
        );
    }

    #[test]
    fn a_trigger_that_enables_itself_in_the_file_is_read_as_enabled_and_one_that_does_not_never_is()
    {
        let (_dir, path) = setup(
            "[[triggers]]\nname = \"on\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\nenabled = true\n\
             [[triggers]]\nname = \"off\"\nminer = \"m\"\ntype = \"poll\"\nevery = \"1m\"\n",
        );
        let triggers = read_triggers(&path).expect("read").triggers;
        assert_eq!(
            triggers
                .iter()
                .map(|t| (t.name.as_str(), t.enabled))
                .collect::<Vec<_>>(),
            [("on", true), ("off", false)]
        );
    }

    #[test]
    fn a_bad_trigger_section_is_named_and_refused() {
        let (_dir, path) = setup("[[triggers]]\nname = \"t\"\nminer = \"m\"\ntype = \"poll\"\n");
        let error = read_triggers(&path).expect_err("no `every`").to_string();
        assert!(error.contains("needs `every`"), "{error}");
        let (_dir, path) = setup("[[triggers]]\nname = \"t\"\nminer = \"m\"\ntype = \"cron\"\n");
        assert!(read_triggers(&path).is_err(), "an unknown type is refused");
    }

    #[test]
    fn a_trigger_file_edited_since_it_was_read_is_never_overwritten() {
        let (_dir, path) = setup(WITH_BOTH);
        let loaded = read_triggers(&path).expect("read");
        std::fs::write(&path, format!("{WITH_BOTH}# edited by hand\n")).expect("edit");
        let error = write_triggers(&path, loaded.stamp, &[trigger("x")]).expect_err("stale");
        assert!(error.to_string().contains("changed while"), "{error}");
    }

    #[test]
    fn an_inline_triggers_array_is_rewritten_as_tables_and_a_scalar_is_refused() {
        let (_dir, path) = setup(
            "triggers = [{ name = \"a\", miner = \"docs\", type = \"poll\", every = \"1m\" }]\n",
        );
        let loaded = read_triggers(&path).expect("read");
        let mut triggers = loaded.triggers;
        triggers.push(trigger("b"));
        write_triggers(&path, loaded.stamp, &triggers).expect("write");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("[[triggers]]"), "{text}");
        assert_eq!(read_triggers(&path).expect("reread").triggers.len(), 2);

        let (_dir, path) = setup("triggers = 5\n");
        let error = read_triggers(&path).expect_err("not a list").to_string();
        assert!(error.contains("[[triggers]]"), "{error}");
    }

    #[test]
    fn a_missing_file_has_no_triggers_and_is_created_with_the_first_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("config.toml");
        let loaded = read_triggers(&path).expect("read");
        assert!(loaded.triggers.is_empty() && loaded.stamp.is_none());
        write_triggers(&path, None, &[trigger("a")]).expect("write");
        assert_eq!(
            read_triggers(&path).expect("reread").triggers,
            vec![trigger("a")]
        );
    }

    #[test]
    fn a_file_that_is_not_toml_is_named_and_nothing_is_written_over_it() {
        let (_dir, path) = setup("[[triggers\nname = ");
        let error = read_triggers(&path).expect_err("not TOML").to_string();
        assert!(error.contains("does not parse as TOML"), "{error}");
        let stamp = stamp(&path).expect("stamp");
        assert!(write_triggers(&path, stamp, &[trigger("a")]).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[[triggers\nname = "
        );
    }

    #[test]
    fn a_changed_trigger_keeps_the_comment_above_it_and_an_invalid_one_is_never_written() {
        let (_dir, path) = setup(WITH_BOTH);
        let loaded = read_triggers(&path).expect("read");
        let mut triggers = loaded.triggers;
        triggers[0].enabled = true;
        let stamp = write_triggers(&path, loaded.stamp, &triggers).expect("write");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(
            text.contains("# polls the docs") && text.contains("enabled = true"),
            "{text}"
        );

        let mut invalid = triggers.clone();
        invalid[0].settings.remove("every");
        let error = write_triggers(&path, stamp, &invalid)
            .expect_err("no `every`")
            .to_string();
        assert!(error.contains("needs `every`"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
    }
}
