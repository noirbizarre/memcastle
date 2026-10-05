//! The reference WebAssembly source: the text files under a directory, one document per file.
//!
//! It follows the same rules as MemCastle's built-in `directory` adapter (hidden and build directories skipped,
//! symlinks not followed, empty, oversized and non-UTF-8 files skipped, a modification-time watermark as the cursor), so
//! the two pass the same conformance cases.
//!
//! Everything a source must do is in the five functions of `Guest` below. Everything it must *not* do is not possible:
//! it reads only the directory its manifest asked for, and MemCastle chunks, deduplicates and files what it returns.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

wit_bindgen::generate!({
    path: "../../wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind, SourceRef,
};

/// This source's name; it must match `source.name` in the manifest.
const NAME: &str = "directory-wasm";

/// Files larger than this are skipped rather than truncated into a misleading document.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Directories never worth reading, skipped at any depth.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "dist",
    "build",
    ".cache",
];

struct Directory;

/// A file found by the walk: the path under the root (the document's identity) and its modification time.
struct Entry {
    key: String,
    mtime_ns: i64,
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<Entry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let key = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if file_type.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name) {
                continue;
            }
            walk(&entry.path(), &key, out);
        } else if file_type.is_file()
            && let Ok(metadata) = entry.metadata()
            && metadata.len() > 0
            && metadata.len() <= MAX_FILE_BYTES
        {
            out.push(Entry {
                key,
                mtime_ns: mtime_ns(&metadata),
            });
        }
    }
}

fn mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(0)
}

/// The cursor `{"mtime_ns", "key"}` means "everything ordered at or before this is done"; `null` is the beginning.
fn parse_cursor(cursor: &str) -> Result<Option<(i64, String)>, SourceError> {
    let invalid = |message: &str| SourceError::CursorInvalid(message.to_string());
    let value: Value = serde_json::from_str(cursor).map_err(|_| invalid("the cursor is not JSON"))?;
    if value.is_null() {
        return Ok(None);
    }
    let mtime_ns = value
        .get("mtime_ns")
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid("`mtime_ns` is missing or not an integer"))?;
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`key` is missing or not a string"))?;
    Ok(Some((mtime_ns, key.to_string())))
}

fn path_of(source: &SourceRef, key: &str) -> PathBuf {
    key.split('/')
        .fold(PathBuf::from(&source.locator), |path, part| path.join(part))
}

/// FNV-1a over the body, with its length: a revision must change when the content does, and nothing more is asked of
/// it. A real source would use a stronger hash, or the source's own etag.
fn revision_of(body: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}-{}", body.len())
}

impl Guest for Directory {
    fn identify(locator: Option<String>) -> Result<SourceRef, SourceError> {
        let Some(locator) = locator else {
            return Err(SourceError::InvalidInput(
                "the directory source needs a directory to mine".to_string(),
            ));
        };
        // MemCastle hands over the canonical path of a directory it has opened for this source, so there is
        // nothing to canonicalise here; a locator that is not a readable directory is simply refused.
        if !Path::new(&locator).is_dir() {
            return Err(SourceError::InvalidInput(format!(
                "{locator} is not a directory this source can read"
            )));
        }
        Ok(SourceRef {
            source: NAME.to_string(),
            account: None,
            locator,
        })
    }

    fn default_wing(source: SourceRef) -> String {
        Path::new(&source.locator)
            .file_name()
            .map_or_else(|| "unnamed".to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn default_room() -> String {
        "files".to_string()
    }

    fn discover(source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let mut entries = Vec::new();
        walk(Path::new(&source.locator), "", &mut entries);
        // Oldest first, ties broken by path, so files saved in the same instant are neither skipped nor read twice.
        entries.sort_by(|a, b| (a.mtime_ns, &a.key).cmp(&(b.mtime_ns, &b.key)));
        entries.retain(|entry| {
            after
                .as_ref()
                .is_none_or(|(mtime, key)| (entry.mtime_ns, entry.key.as_str()) > (*mtime, key.as_str()))
        });
        let limit = limit as usize;
        let exhausted = entries.len() <= limit;
        entries.truncate(limit);
        Ok(Discovery {
            candidates: entries
                .into_iter()
                .map(|entry| Candidate {
                    cursor_after: json!({ "mtime_ns": entry.mtime_ns, "key": entry.key }).to_string(),
                    handle: entry.key.clone(),
                    external_id: entry.key,
                })
                .collect(),
            exhausted,
        })
    }

    fn read(source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        let path = path_of(&source, &candidate.handle);
        // Any failure is a skip, not an error: the file may have gone, shrunk or grown since discovery.
        let Ok(metadata) = std::fs::metadata(&path) else {
            return Ok(None);
        };
        if metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
            return Ok(None);
        }
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

export!(Directory);
