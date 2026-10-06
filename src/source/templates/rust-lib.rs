//! __NAME__: a MemCastle source, written as a WebAssembly component.
//!
//! This one reads the `.txt` files of a flat directory, one document per file, and is deliberately small: it shows
//! the five functions a source implements and nothing else. MemCastle does the rest (chunking, deduplication, the
//! drawers, the cursor and the job), and the host opens only what `memcastle-source.toml` asks for.
//!
//! Build it with `memcastle source build`, check it with `memcastle source test`, and ship it with
//! `memcastle source package`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

wit_bindgen::generate!({
    path: "wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind, SourceRef,
};

/// This source's source name; it must match `source.name` in `memcastle-source.toml`.
const NAME: &str = "__NAME__";

struct Source;

/// The names of the `.txt` files in `dir`, in the order discovery lists them.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".txt"))
        .collect();
    names.sort();
    names
}

/// The cursor is `{"after": "<name>"}`: everything up to and including that name is done. `null` is the beginning.
fn parse_cursor(cursor: &str) -> Result<Option<String>, SourceError> {
    // A cursor this source did not produce is reported as such, so MemCastle can tell the user to mine with `--full`.
    let invalid = |message: &str| SourceError::CursorInvalid(message.to_string());
    let value: Value = serde_json::from_str(cursor).map_err(|_| invalid("the cursor is not JSON"))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .get("after")
        .and_then(Value::as_str)
        .map(|name| Some(name.to_string()))
        .ok_or_else(|| invalid("`after` is missing or not a string"))
}

/// A revision must change exactly when the content does. A real source would use the source's etag or a strong hash.
fn revision_of(body: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}-{}", body.len())
}

impl Guest for Source {
    fn identify(
        locator: Option<String>,
        options: Vec<(String, String)>,
    ) -> Result<SourceRef, SourceError> {
        // A source that accepts no options refuses one rather than ignoring it: a typo would otherwise mine everything.
        if let Some((key, _)) = options.first() {
            return Err(SourceError::InvalidInput(format!("this source has no option `{key}`")));
        }
        let Some(locator) = locator else {
            return Err(SourceError::InvalidInput("give the directory to mine".to_string()));
        };
        if !Path::new(&locator).is_dir() {
            return Err(SourceError::InvalidInput(format!("{locator} is not a directory this source can read")));
        }
        Ok(SourceRef { source: NAME.to_string(), account: None, locator, options: Vec::new() })
    }

    fn default_wing(source: SourceRef) -> String {
        Path::new(&source.locator)
            .file_name()
            .map_or_else(|| "unnamed".to_string(), |name| name.to_string_lossy().into_owned())
    }

    fn default_room() -> String {
        "notes".to_string()
    }

    fn discover(source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let mut names = names(Path::new(&source.locator));
        names.retain(|name| after.as_ref().is_none_or(|after| name > after));
        let limit = limit as usize;
        let exhausted = names.len() <= limit;
        names.truncate(limit);
        let candidates = names
            .into_iter()
            .map(|name| Candidate {
                cursor_after: json!({ "after": name }).to_string(),
                handle: name.clone(),
                external_id: name,
            })
            .collect();
        Ok(Discovery { candidates, exhausted })
    }

    fn read(source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        let path: PathBuf = Path::new(&source.locator).join(&candidate.handle);
        // A file that vanished or is not text is skipped, not an error: the tree changes between runs.
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision_of(&body),
            body,
            metadata: json!({ "path": path.display().to_string() }).to_string(),
            occurred_at: None,
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        let metadata: Value = serde_json::from_str(&raw.metadata).unwrap_or(Value::Null);
        Ok(CanonicalDocument {
            title: Some(raw.external_id),
            room: None,
            name: None,
            kind: SourceKind::File,
            uri: metadata.get("path").and_then(Value::as_str).map(str::to_string),
            tags: Vec::new(),
            segments: vec![Segment { text: raw.body }],
        })
    }
}

export!(Source);
