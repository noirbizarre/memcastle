//! The Pi history source: the conversation history of the Pi coding agent, read from its session files.
//!
//! Pi writes one JSONL file per session under `<root>/<working directory, slugged>/`, appending one entry per line as
//! the session goes on. This source reads those files directly: no Pi process, no model, no tokens, and it works on
//! history from sessions that ended long ago.
//!
//! What it files, per session: a header (session id and working directory) and each user and assistant message as text,
//! with the time it was written. What it leaves out on purpose: model reasoning (large, and not what was said), tool
//! results (large, and usually the contents of files that can be mined as files), and everything that is not a
//! conversation message. Tool *calls* are kept as one-line markers so the shape of the work stays visible.
//! A line that is not valid JSON, or an entry type this source does not know, is skipped rather than failing the
//! session: Pi's format is Pi's to evolve, and a reader that stopped at the first surprise would stop mining all history.
//!
//! The format is Pi's session format version 3. The source never opens anything but `*.jsonl` files, and in particular
//! not Pi's credentials file. Sessions are appended to, so a modification-time watermark finds the ones that grew, and
//! MemCastle's revision check plus chunk hashes make re-ingesting a grown session cost only its new tail.
//!
//! Acquisition is all this does: MemCastle chunks, deduplicates, files and remembers what it returns.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

wit_bindgen::generate!({
    path: "../../wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind,
    SourceRef,
};

/// This source's name; it must match `source.name` in the manifest.
const NAME: &str = "pi";

/// The longest title taken from a session's first prompt, in characters.
const TITLE_CHARS: usize = 80;

struct Pi;

/// A session file found by the walk: its path under the root (the document's identity) and its modification time.
struct Entry {
    key: String,
    mtime_ns: i64,
}

/// Where Pi keeps its sessions, when the caller names no locator. `HOME` is the one variable the manifest lists.
fn default_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".pi")
            .join("agent")
            .join("sessions"),
    )
}

/// Collect the session files under `root`: `*.jsonl` directly in it and in each directory one level down (Pi's layout
/// is one directory per working directory). Symlinks are not followed, and nothing but `.jsonl` is ever listed.
fn collect(root: &Path, out: &mut Vec<Entry>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if file_type.is_file() {
            push_if_session(&entry, name, out);
        } else if file_type.is_dir() {
            let Ok(inner) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for session in inner.flatten() {
                let Ok(inner_type) = session.file_type() else {
                    continue;
                };
                let Some(inner_name) = session.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if inner_type.is_file() {
                    push_if_session(&session, format!("{name}/{inner_name}"), out);
                }
            }
        }
    }
}

/// Add `entry` as `key` if it is a `.jsonl` file.
fn push_if_session(entry: &std::fs::DirEntry, key: String, out: &mut Vec<Entry>) {
    if !Path::new(&key)
        .extension()
        .is_some_and(|extension| extension == "jsonl")
    {
        return;
    }
    if let Ok(metadata) = entry.metadata() {
        out.push(Entry {
            key,
            mtime_ns: mtime_ns(&metadata),
        });
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
    let value: Value =
        serde_json::from_str(cursor).map_err(|_| invalid("the cursor is not JSON"))?;
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

/// FNV-1a over the body, with its length: a revision must change when the content does, and nothing more is asked of it.
fn revision_of(body: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}-{}", body.len())
}

/// The session header entry (`{"type":"session", ...}`), which Pi writes as the first line; `None` when there is none.
fn header_of(body: &str) -> Option<Value> {
    body.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| entry.get("type").and_then(Value::as_str) == Some("session"))
}

/// Whether `text` has the shape of an RFC 3339 timestamp (`2026-07-14T14:27:12.546Z`, or with a `+HH:MM` offset).
///
/// MemCastle fails the whole job on an `occurred-at` it cannot parse, so a header Pi wrote in some other shape is left
/// out of the document rather than taking every session with it.
fn looks_like_rfc3339(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|b| b.iter().all(u8::is_ascii_digit))
    };
    let shape = digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10)
        && matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        && digits(11..13)
        && bytes.get(13) == Some(&b':')
        && digits(14..16)
        && bytes.get(16) == Some(&b':')
        && digits(17..19);
    if !shape {
        return false;
    }
    // Optional fraction, then `Z` or an offset.
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let end = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        if end == 0 {
            return false;
        }
        rest = &fraction[end..];
    }
    match rest.as_bytes() {
        [b'Z' | b'z'] => true,
        [b'+' | b'-', h1, h2, b':', m1, m2] => [h1, h2, m1, m2].iter().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// The text of one Pi message worth filing, or `None` when it has none (reasoning only, a tool result, an unknown role).
fn message_text(role: &str, message: &Value) -> Option<String> {
    match role {
        "user" | "assistant" => {
            let text = match message.get("content")? {
                Value::String(text) => text.trim().to_string(),
                Value::Array(blocks) => blocks
                    .iter()
                    .filter_map(block_text)
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => return None,
            };
            (!text.trim().is_empty()).then_some(text)
        }
        // The command the user ran, not its output.
        "bashExecution" => message
            .get("command")
            .and_then(Value::as_str)
            .map(|command| format!("[bash: {}]", first_line(command))),
        _ => None,
    }
}

/// One content block as text: prose as it is, a tool call as a marker, anything else (reasoning, images) not at all.
fn block_text(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => {
            let text = block.get("text").and_then(Value::as_str)?.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        "toolCall" => {
            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
            Some(format!("[tool call: {name}]"))
        }
        _ => None,
    }
}

/// The first line of `text`, cut to [`TITLE_CHARS`] characters.
fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(TITLE_CHARS)
        .collect()
}

impl Guest for Pi {
    fn identify(locator: Option<String>) -> Result<SourceRef, SourceError> {
        let root = match locator {
            Some(locator) => PathBuf::from(locator),
            None => default_root().ok_or_else(|| {
                SourceError::InvalidInput(
                    "cannot tell where Pi keeps its sessions without a home directory; give the sessions directory as the locator"
                        .to_string(),
                )
            })?,
        };
        // MemCastle hands over the canonical path of a directory it has opened for this source, so there is nothing
        // to canonicalise here; a locator that is not a readable directory is simply refused.
        if !root.is_dir() {
            return Err(SourceError::InvalidInput(format!(
                "{} is not a directory of Pi sessions this source can read",
                root.display()
            )));
        }
        Ok(SourceRef {
            source: NAME.to_string(),
            account: None,
            locator: root.display().to_string(),
        })
    }

    fn default_wing(_source: SourceRef) -> String {
        "pi".to_string()
    }

    fn default_room() -> String {
        "sessions".to_string()
    }

    fn discover(source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let mut entries = Vec::new();
        collect(Path::new(&source.locator), &mut entries);
        // Oldest first, ties broken by path, so sessions saved in the same instant are neither skipped nor read twice.
        entries.sort_by(|a, b| (a.mtime_ns, &a.key).cmp(&(b.mtime_ns, &b.key)));
        entries.retain(|entry| {
            after.as_ref().is_none_or(|(mtime, key)| {
                (entry.mtime_ns, entry.key.as_str()) > (*mtime, key.as_str())
            })
        });
        let limit = limit as usize;
        let exhausted = entries.len() <= limit;
        entries.truncate(limit);
        Ok(Discovery {
            candidates: entries
                .into_iter()
                .map(|entry| Candidate {
                    cursor_after: json!({ "mtime_ns": entry.mtime_ns, "key": entry.key })
                        .to_string(),
                    handle: entry.key.clone(),
                    external_id: entry.key,
                })
                .collect(),
            exhausted,
        })
    }

    fn read(source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        let path = path_of(&source, &candidate.handle);
        // A session being written right now can end mid-line or mid-character; the unfinished tail is picked up whole
        // on the next run, once the file has moved on. Any failure is a skip, not an error.
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        if body.trim().is_empty() {
            return Ok(None);
        }
        let header = header_of(&body);
        let field = |name: &str| {
            header
                .as_ref()
                .and_then(|h| h.get(name))
                .cloned()
                .unwrap_or(Value::Null)
        };
        // The session's own start time is its identity in time; it does not move when the file is copied or restored.
        let occurred_at = field("timestamp")
            .as_str()
            .filter(|at| looks_like_rfc3339(at))
            .map(str::to_string);
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision_of(&body),
            body,
            metadata: json!({
                "path": path.display().to_string(),
                "session_id": field("id"),
                "cwd": field("cwd"),
                "version": field("version"),
            })
            .to_string(),
            occurred_at,
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        let mut header: Option<(String, Option<String>)> = None;
        let mut messages: Vec<Segment> = Vec::new();
        let mut title: Option<String> = None;

        for line in raw.body.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match entry.get("type").and_then(Value::as_str) {
                Some("session") => {
                    let id = entry
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let cwd = entry.get("cwd").and_then(Value::as_str).map(str::to_string);
                    header = Some((id, cwd));
                }
                Some("message") => {
                    let Some(message) = entry.get("message") else {
                        continue;
                    };
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let Some(text) = message_text(role, message) else {
                        continue;
                    };
                    if title.is_none() && role == "user" {
                        title = Some(first_line(&text));
                    }
                    let at = entry
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    messages.push(Segment {
                        text: format!("### {role} · {at}\n{text}\n\n"),
                    });
                }
                // Model and thinking-level changes, labels, compactions and anything newer: not conversation.
                _ => {}
            }
        }

        // A session with no messages files nothing (MemCastle still remembers its revision).
        let mut segments = Vec::new();
        if !messages.is_empty() {
            let (id, cwd) = header.clone().unwrap_or_default();
            segments.push(Segment {
                text: format!(
                    "# Pi session {id}\nworking directory: {}\n\n",
                    cwd.as_deref().unwrap_or("unknown")
                ),
            });
            segments.extend(messages);
        }

        let room = header
            .as_ref()
            .and_then(|(_, cwd)| cwd.as_deref())
            .and_then(|cwd| Path::new(cwd).file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let name = Path::new(&raw.external_id)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned());
        let metadata: Value = serde_json::from_str(&raw.metadata).unwrap_or(Value::Null);

        Ok(CanonicalDocument {
            title,
            room,
            name,
            kind: SourceKind::Transcript,
            uri: metadata
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string),
            tags: vec!["transcript".to_string(), "pi".to_string()],
            segments,
        })
    }
}

export!(Pi);
