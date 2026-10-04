//! The `pi-sessions` source: the conversation history of the Pi coding agent, read from its session files.
//!
//! Pi writes one JSONL file per session under `<root>/<working directory, slugged>/`, appending one entry per line
//! as the session goes on. This adapter reads those files directly: no Pi process, no model, no tokens, and it
//! works on history from sessions that ended long ago, which is the point of source-driven mining.
//!
//! What it files, per session: a header (session id and working directory) and each user and assistant message as
//! text, with the time it was written. What it leaves out on purpose: model reasoning (large and not what was
//! said), tool results (large, and usually the contents of files that can be mined as files), and everything that
//! is not a conversation message. Tool *calls* are kept as one-line markers so the shape of the work stays
//! visible. A line that is not valid JSON, or an entry type this adapter does not know, is skipped rather than
//! failing the session: Pi's format is Pi's to evolve, and a reader that stopped at the first surprise would stop
//! mining all history.
//!
//! The format here is Pi's session format version 3. The adapter never opens anything else under Pi's directory;
//! in particular not its credentials file.
//!
//! Sessions are appended to, so a modification-time watermark finds the ones that grew, and the pipeline's
//! revision check plus chunk hashes make re-ingesting a grown session cost only its new tail.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::domain::{
    Candidate, CanonicalDocument, Cursor, RawDocument, Segment, SourceCapabilities, SourceKind,
    SourceRef,
};
use crate::error::{Error, Result};

use super::super::adapter::{Discovery, SourceAdapter};
use super::watermark::{Entry, Watermark, mtime_ns, page};

/// The adapter's name.
pub const PROVIDER: &str = "pi-sessions";

/// The longest title taken from a session's first prompt, in characters.
const TITLE_CHARS: usize = 80;

/// Reads Pi session files.
pub struct PiSessionsAdapter {
    /// Where sessions live when the caller names no locator.
    default_root: PathBuf,
}

impl PiSessionsAdapter {
    /// An adapter that looks in `configured` (`mining.pi_sessions_dir`) or, with none, in `~/.pi/agent/sessions`.
    #[must_use]
    pub fn new(configured: Option<&Path>) -> Self {
        let default_root = configured.map_or_else(
            || {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".pi")
                    .join("agent")
                    .join("sessions")
            },
            Path::to_path_buf,
        );
        Self { default_root }
    }
}

impl SourceAdapter for PiSessionsAdapter {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn description(&self) -> &'static str {
        "Pi coding-agent session history (JSONL files under ~/.pi/agent/sessions)"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            incremental: true,
            // Session files are rotated and deleted by the user; the transcript is worth keeping once read.
            retains_raw: true,
            needs_credentials: false,
        }
    }

    fn identify(&self, locator: Option<&str>) -> Result<SourceRef> {
        let root = locator.map_or_else(|| self.default_root.clone(), PathBuf::from);
        let canonical = std::fs::canonicalize(&root)
            .map_err(|source| Error::io(root.display().to_string(), source))?;
        if !canonical.is_dir() {
            return Err(Error::invalid_input(
                "locator",
                format!("{} is not a directory of Pi sessions", canonical.display()),
            ));
        }
        Ok(SourceRef {
            provider: PROVIDER.to_string(),
            account: None,
            locator: canonical.display().to_string(),
        })
    }

    fn default_wing(&self, _source: &SourceRef) -> String {
        "pi".to_string()
    }

    fn default_room(&self) -> &'static str {
        "sessions"
    }

    async fn discover(
        &self,
        source: &SourceRef,
        cursor: &Cursor,
        limit: usize,
    ) -> Result<Discovery> {
        let after = Watermark::parse(PROVIDER, cursor)?;
        let mut entries = Vec::new();
        collect(Path::new(&source.locator), &mut entries);
        let (candidates, exhausted) = page(entries, after.as_ref(), limit);
        Ok(Discovery {
            candidates,
            exhausted,
        })
    }

    async fn read(&self, source: &SourceRef, candidate: &Candidate) -> Result<Option<RawDocument>> {
        let path = candidate
            .handle
            .split('/')
            .fold(PathBuf::from(&source.locator), |path, part| path.join(part));
        let Ok(metadata) = std::fs::metadata(&path) else {
            return Ok(None);
        };
        // A session being written right now can end mid-line or mid-character; the unfinished tail is picked up
        // whole on the next run, once the file has moved on.
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        if body.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(RawDocument {
            external_id: candidate.external_id.clone(),
            revision: RawDocument::revision_of(&body),
            body,
            metadata: json!({ "path": path.display().to_string() }),
            occurred_at: metadata.modified().ok().map(DateTime::<Utc>::from),
        }))
    }

    fn normalize(&self, raw: &RawDocument) -> Result<CanonicalDocument> {
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

        // A session with no messages files nothing (the pipeline still remembers its revision).
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

        Ok(CanonicalDocument {
            title,
            room,
            name,
            kind: SourceKind::Transcript,
            uri: raw
                .metadata
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string),
            tags: vec!["transcript".to_string(), "pi".to_string()],
            segments,
        })
    }
}

/// The text of one Pi message worth filing, or `None` when it has none (reasoning only, a tool result, an unknown
/// role).
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

/// Collect the session files under `root`: `*.jsonl` directly in it and in each directory one level down (Pi's
/// layout is one directory per working directory). Symlinks are not followed.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> PiSessionsAdapter {
        PiSessionsAdapter::new(None)
    }

    fn raw(body: &str) -> RawDocument {
        RawDocument {
            external_id: "--home-me-project--/2026-07-14T14-27-12-546Z_019f6106.jsonl".into(),
            revision: RawDocument::revision_of(body),
            body: body.to_string(),
            metadata: json!({"path": "/sessions/x.jsonl"}),
            occurred_at: None,
        }
    }

    const FIXTURE: &str = include_str!("../../../tests/fixtures/sources/pi/session.jsonl");

    #[test]
    fn a_session_becomes_a_header_and_its_user_and_assistant_messages_in_order() {
        let doc = adapter().normalize(&raw(FIXTURE)).unwrap();
        let text: String = doc.segments.iter().map(|s| s.text.as_str()).collect();
        assert!(text.starts_with("# Pi session 019f6106"), "{text}");
        assert!(text.contains("working directory: /home/me/project"));
        let user = text.find("### user").unwrap();
        let assistant = text.find("### assistant").unwrap();
        assert!(
            user < assistant,
            "messages must stay in the order they were written"
        );
        assert!(text.contains("How do I rotate the signing keys?"));
        assert!(text.contains("Run the rotation script"));
    }

    #[test]
    fn tool_calls_are_kept_as_markers_and_tool_results_and_reasoning_are_dropped() {
        let doc = adapter().normalize(&raw(FIXTURE)).unwrap();
        let text: String = doc.segments.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains("[tool call: read]"));
        assert!(
            !text.contains("SECRET-TOOL-OUTPUT"),
            "tool results are large and not what was said"
        );
        assert!(
            !text.contains("PRIVATE-REASONING"),
            "reasoning is not what was said"
        );
    }

    #[test]
    fn entries_that_are_not_conversation_and_lines_that_are_not_json_are_skipped() {
        let doc = adapter().normalize(&raw(FIXTURE)).unwrap();
        let text: String = doc.segments.iter().map(|s| s.text.as_str()).collect();
        assert!(!text.contains("thinkingLevel"));
        assert!(!text.contains("model_change"));
        assert!(
            text.contains("### user"),
            "a bad line must not stop the lines after it"
        );
    }

    #[test]
    fn a_session_is_filed_as_a_transcript_under_its_working_directory_and_named_by_its_file() {
        let doc = adapter().normalize(&raw(FIXTURE)).unwrap();
        assert_eq!(doc.kind, SourceKind::Transcript);
        assert_eq!(doc.room.as_deref(), Some("project"));
        assert_eq!(
            doc.name.as_deref(),
            Some("2026-07-14T14-27-12-546Z_019f6106")
        );
        assert_eq!(
            doc.title.as_deref(),
            Some("How do I rotate the signing keys?")
        );
        assert_eq!(doc.uri.as_deref(), Some("/sessions/x.jsonl"));
    }

    #[test]
    fn a_session_with_no_messages_files_nothing() {
        let body = r#"{"type":"session","version":3,"id":"x","cwd":"/a"}
{"type":"model_change","provider":"p","modelId":"m"}
{"type":"message","message":{"role":"assistant","content":[]}}
"#;
        let doc = adapter().normalize(&raw(body)).unwrap();
        assert!(doc.segments.is_empty());
    }

    #[test]
    fn appending_a_message_only_adds_a_segment_at_the_end() {
        let grown = format!(
            "{FIXTURE}{}\n",
            r#"{"type":"message","timestamp":"2026-07-14T15:00:00Z","message":{"role":"user","content":"and then?"}}"#
        );
        let before = adapter().normalize(&raw(FIXTURE)).unwrap().segments;
        let after = adapter().normalize(&raw(&grown)).unwrap().segments;
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(
            &after[..before.len()],
            &before[..],
            "earlier segments must be byte-identical so their chunks keep their hashes"
        );
    }

    #[tokio::test]
    async fn sessions_are_discovered_one_directory_down_and_only_jsonl_files() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("--home-me-project--");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("a.jsonl"), FIXTURE).unwrap();
        std::fs::write(project.join("notes.txt"), "not a session").unwrap();
        std::fs::write(root.path().join("auth.json"), "{\"token\":\"never read\"}").unwrap();
        let source = adapter().identify(root.path().to_str()).unwrap();
        let found = adapter()
            .discover(&source, &Cursor::Null, 10)
            .await
            .unwrap();
        let ids: Vec<_> = found
            .candidates
            .iter()
            .map(|c| c.external_id.as_str())
            .collect();
        assert_eq!(ids, ["--home-me-project--/a.jsonl"]);
    }

    #[tokio::test]
    async fn an_empty_session_file_is_skipped_at_read_time() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.jsonl"), "  \n").unwrap();
        let source = adapter().identify(root.path().to_str()).unwrap();
        let found = adapter()
            .discover(&source, &Cursor::Null, 10)
            .await
            .unwrap();
        assert!(
            adapter()
                .read(&source, &found.candidates[0])
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_configured_root_replaces_the_default_one() {
        let root = tempfile::tempdir().unwrap();
        let source = PiSessionsAdapter::new(Some(root.path()))
            .identify(None)
            .unwrap();
        assert_eq!(
            Path::new(&source.locator),
            std::fs::canonicalize(root.path()).unwrap()
        );
    }
}
