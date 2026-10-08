//! Codex rollout history, read from the JSONL transcripts Codex writes under `~/.codex/sessions`.
//!
//! The reader keeps what the user and assistant said, plus compact tool-call markers.
//! It intentionally leaves out reasoning, tool output, command/file contents and injected context: those are not the
//! conversation and can carry secrets or a copy of material that belongs to another source.

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

const NAME: &str = "codex";
const TITLE_CHARS: usize = 80;

struct Codex;

struct Entry {
    key: String,
    mtime_ns: i64,
}

fn default_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join(".codex").join("sessions"))
}

fn mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|time| i64::try_from(time.as_nanos()).ok())
        .unwrap_or(0)
}

/// Rollouts are three date directories deep; limiting the walk prevents an unexpected user tree from widening access.
fn collect(dir: &Path, prefix: &str, depth: u8, out: &mut Vec<Entry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() && depth < 3 {
            collect(&entry.path(), &key, depth + 1, out);
        } else if kind.is_file()
            && depth == 3
            && name.starts_with("rollout-")
            && name.ends_with(".jsonl")
            && let Ok(metadata) = entry.metadata()
        {
            out.push(Entry {
                key,
                mtime_ns: mtime_ns(&metadata),
            });
        }
    }
}

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
        .filter(|key| !key.is_empty() && !key.contains(".."))
        .ok_or_else(|| invalid("`key` is missing or not a rollout path"))?;
    Ok(Some((mtime_ns, key.to_string())))
}

fn path_of(source: &SourceRef, key: &str) -> PathBuf {
    key.split('/')
        .fold(PathBuf::from(&source.locator), |path, part| path.join(part))
}

fn revision_of(body: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in body.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}-{}", body.len())
}

fn payload(entry: &Value) -> &Value {
    entry.get("payload").unwrap_or(entry)
}

fn text_from(content: &Value) -> String {
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                let kind = part.get("type").and_then(Value::as_str)?;
                // These are the only content blocks authored for the conversation; reasoning and tool output are excluded.
                matches!(kind, "input_text" | "output_text" | "text")
                    .then(|| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .trim()
                            .to_string()
                    })
                    .filter(|text| !text.is_empty())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(TITLE_CHARS)
        .collect()
}

impl Guest for Codex {
    fn identify(
        locator: Option<String>,
        options: Vec<(String, String)>,
    ) -> Result<SourceRef, SourceError> {
        if let Some((key, _)) = options.first() {
            return Err(SourceError::InvalidInput(format!(
                "the codex source has no option `{key}`"
            )));
        }
        let root = locator.map(PathBuf::from).or_else(default_root).ok_or_else(|| {
            SourceError::InvalidInput("cannot tell where Codex keeps its sessions without a home directory; give the sessions directory as the locator".to_string())
        })?;
        if !root.is_dir() {
            return Err(SourceError::InvalidInput(format!(
                "{} is not a directory of Codex sessions this source can read",
                root.display()
            )));
        }
        Ok(SourceRef {
            source: NAME.to_string(),
            account: None,
            locator: root.display().to_string(),
            options: Vec::new(),
        })
    }

    fn default_wing(_source: SourceRef) -> String {
        NAME.to_string()
    }

    fn default_room() -> String {
        "sessions".to_string()
    }

    fn discover(source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let mut entries = Vec::new();
        collect(Path::new(&source.locator), "", 0, &mut entries);
        entries.sort_by(|a, b| (a.mtime_ns, &a.key).cmp(&(b.mtime_ns, &b.key)));
        entries.retain(|entry| {
            after.as_ref().is_none_or(|(mtime, key)| {
                (entry.mtime_ns, entry.key.as_str()) > (*mtime, key.as_str())
            })
        });
        let exhausted = entries.len() <= limit as usize;
        entries.truncate(limit as usize);
        Ok(Discovery {
            candidates: entries
                .into_iter()
                .map(|entry| Candidate {
                    cursor_after: json!({"mtime_ns": entry.mtime_ns, "key": entry.key}).to_string(),
                    handle: entry.key.clone(),
                    external_id: entry.key,
                })
                .collect(),
            exhausted,
        })
    }

    fn read(source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        let path = path_of(&source, &candidate.handle);
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        if body.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision_of(&body),
            body,
            metadata: json!({"path": path.display().to_string()}).to_string(),
            occurred_at: None,
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        let mut session_id = String::new();
        let mut cwd = None;
        let mut title = None;
        let mut segments = Vec::new();
        for line in raw.body.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let value = payload(&entry);
            let kind = value
                .get("type")
                .or_else(|| entry.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(kind, "session_meta" | "session") {
                session_id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                cwd = value.get("cwd").and_then(Value::as_str).map(str::to_string);
                continue;
            }
            if matches!(kind, "function_call" | "tool_call") {
                let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
                segments.push(Segment {
                    text: format!("[tool call: {name}]\n\n"),
                });
                continue;
            }
            if kind != "message" {
                continue;
            }
            let role = value
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(role, "user" | "assistant") {
                continue;
            }
            let text = value.get("content").map(text_from).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            if title.is_none() && role == "user" {
                title = Some(first_line(&text));
            }
            segments.push(Segment {
                text: format!("### {role}\n{text}\n\n"),
            });
        }
        if !segments.is_empty() {
            segments.insert(
                0,
                Segment {
                    text: format!(
                        "# Codex session {session_id}\nworking directory: {}\n\n",
                        cwd.as_deref().unwrap_or("unknown")
                    ),
                },
            );
        }
        let metadata: Value = serde_json::from_str(&raw.metadata).unwrap_or(Value::Null);
        let room = cwd
            .as_deref()
            .and_then(|cwd| Path::new(cwd).file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let name = Path::new(&raw.external_id)
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned());
        Ok(CanonicalDocument {
            title,
            room,
            name,
            kind: SourceKind::Transcript,
            uri: metadata
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string),
            tags: vec!["transcript".to_string(), NAME.to_string()],
            segments,
        })
    }
}

export!(Codex);
