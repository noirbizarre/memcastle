//! Claude Code history, acquired conservatively from its per-session JSONL transcripts.
//!
//! Only `~/.claude/projects/<project>/<session>.jsonl` is read. Claude Code owns the format, so malformed and unknown
//! records are ignored rather than making one new record shape stop all history mining.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

wit_bindgen::generate!({ path: "../../wit", world: "source" });

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind,
    SourceRef,
};

const NAME: &str = "claude";
const TITLE_CHARS: usize = 80;

struct Claude;

struct Entry {
    key: String,
    mtime_ns: i64,
}

fn default_root() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude").join("projects"))
}

/// Claude Code puts sessions one project directory below its projects root; this scope excludes settings and diagnostics.
fn collect(root: &Path, out: &mut Vec<Entry>) {
    let Ok(projects) = std::fs::read_dir(root) else {
        return;
    };
    for project in projects.flatten() {
        let Ok(kind) = project.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let Some(project_name) = project.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(sessions) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let Ok(kind) = session.file_type() else {
                continue;
            };
            let Some(name) = session.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !kind.is_file()
                || Path::new(&name)
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            if let Ok(metadata) = session.metadata() {
                out.push(Entry {
                    key: format!("{project_name}/{name}"),
                    mtime_ns: mtime_ns(&metadata),
                });
            }
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

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
        + (153 * ((month + 9) % 12) + 2) / 5
        + day
        - 1
        - 719_468
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Use the full instant as the discovery floor; dropping the time would include earlier sessions from the same day.
fn parse_since(value: &str) -> Result<i64, String> {
    let value = value.trim();
    let bad =
        || format!("`{value}` is not a date; use `2026-09`, `2026-09-14` or an RFC 3339 time");
    let number = |text: &str, digits: usize| -> Option<i64> {
        if text.len() == digits && text.bytes().all(|byte| byte.is_ascii_digit()) {
            text.parse().ok()
        } else {
            None
        }
    };
    let (date, time) = match value.find(['T', 't']) {
        Some(at) => (&value[..at], Some(&value[at + 1..])),
        None => (value, None),
    };
    let parts: Vec<_> = date.split('-').collect();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|part| !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(bad());
    }
    let year: i64 = parts[0].parse().map_err(|_| bad())?;
    let month: i64 = parts[1].parse().map_err(|_| bad())?;
    let day: i64 = if parts.len() == 3 {
        parts[2].parse().map_err(|_| bad())?
    } else {
        1
    };
    if parts[0].len() != 4
        || parts[1].len() != 2
        || (parts.len() == 3 && parts[2].len() != 2)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return Err(bad());
    }
    let mut seconds = days_from_civil(year, month, day) * 86_400;
    let mut fraction_ms = 0;
    if let Some(time) = time {
        // A missing zone would make the same cutoff mean different instants on different machines.
        let (clock, offset) = if let Some(clock) = time.strip_suffix(['Z', 'z']) {
            (clock, 0)
        } else {
            let at = time.rfind(['+', '-']).ok_or_else(bad)?;
            let (clock, zone) = time.split_at(at);
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let (hours, minutes) = zone[1..].split_once(':').ok_or_else(bad)?;
            let hours = number(hours, 2).filter(|hour| *hour < 24).ok_or_else(bad)?;
            let minutes = number(minutes, 2)
                .filter(|minute| *minute < 60)
                .ok_or_else(bad)?;
            (clock, sign * (hours * 3600 + minutes * 60))
        };
        let (whole, fraction) = match clock.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (clock, None),
        };
        let mut fields = whole.split(':');
        let mut next = |limit: i64| {
            fields
                .next()
                .and_then(|part| number(part, 2))
                .filter(|field| *field < limit)
        };
        let (hour, minute, second) = (
            next(24).ok_or_else(bad)?,
            next(60).ok_or_else(bad)?,
            next(60).ok_or_else(bad)?,
        );
        if fields.next().is_some() {
            return Err(bad());
        }
        seconds += hour * 3600 + minute * 60 + second - offset;
        if let Some(fraction) = fraction {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(bad());
            }
            let padded: String = fraction.chars().chain("000".chars()).take(3).collect();
            fraction_ms = padded.parse().map_err(|_| bad())?;
        }
    }
    Ok(seconds * 1000 + fraction_ms)
}

#[cfg(test)]
mod tests {
    use super::parse_since;

    #[test]
    fn a_timestamp_cutoff_honors_the_clock_and_offset() {
        let cutoff = parse_since("2026-09-14T18:00:00.250+02:00").unwrap();
        assert_eq!(cutoff, parse_since("2026-09-14T16:00:00.250Z").unwrap());
        assert!(cutoff > parse_since("2026-09-14").unwrap());
        assert!(cutoff < parse_since("2026-09-15").unwrap());
        assert_eq!(
            parse_since("2026-09").unwrap(),
            parse_since("2026-09-01").unwrap()
        );
        for invalid in [
            "2026-09-14T25:00:00Z",
            "2026-09-14T18:00:00",
            "2026-09-14T18:00:00+02:99",
        ] {
            assert!(parse_since(invalid).is_err(), "{invalid} should be refused");
        }
    }
}

fn normalize_dir(value: &str) -> String {
    let absolute = value.starts_with('/');
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    if absolute {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let (pattern, text): (Vec<_>, Vec<_>) = (pattern.chars().collect(), text.chars().collect());
    let (mut p, mut t, mut star) = (0, 0, None);
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((at, next)) = star {
            p = at + 1;
            t = next + 1;
            star = Some((at, next + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn parse_cursor(cursor: &str) -> Result<Option<(i64, String)>, SourceError> {
    let value: Value = serde_json::from_str(cursor)
        .map_err(|_| SourceError::CursorInvalid("the cursor is not JSON".into()))?;
    if value.is_null() {
        return Ok(None);
    }
    let mtime_ns = value
        .get("mtime_ns")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            SourceError::CursorInvalid("`mtime_ns` is missing or not an integer".into())
        })?;
    let key = value
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::CursorInvalid("`key` is missing or not a string".into()))?;
    Ok(Some((mtime_ns, key.to_owned())))
}

fn path_of(source: &SourceRef, key: &str) -> PathBuf {
    key.split('/')
        .fold(PathBuf::from(&source.locator), |path, part| path.join(part))
}

fn revision_of(body: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in body.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}-{}", body.len())
}

fn record_cwd(body: &str) -> Option<String> {
    body.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|record| record.get("cwd").and_then(Value::as_str).map(str::to_owned))
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => (!text.trim().is_empty()).then(|| text.trim().to_owned()),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => block
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned),
                    Some("tool_use") => Some(format!(
                        "[tool: {}]",
                        block.get("name").and_then(Value::as_str).unwrap_or("tool")
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

/// Whether this transcript has at least one message the normalizer can retain, so metadata-only sessions never become
/// empty documents in the palace.
fn has_retained_message(body: &str) -> bool {
    body.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).any(|record| {
        if record.get("isMeta").and_then(Value::as_bool) == Some(true)
            || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        {
            return false;
        }
        let message = record.get("message").unwrap_or(&record);
        matches!(message.get("role").and_then(Value::as_str), Some("user" | "assistant"))
            && content_text(message.get("content").unwrap_or(&Value::Null)).is_some()
    })
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

impl Guest for Claude {
    fn identify(
        locator: Option<String>,
        options: Vec<(String, String)>,
    ) -> Result<SourceRef, SourceError> {
        let mut normalized = Vec::new();
        let mut account = None;
        for (key, value) in options {
            match key.as_str() {
                "since" => {
                    parse_since(&value).map_err(SourceError::InvalidInput)?;
                    normalized.push((key, value.trim().to_owned()));
                }
                "dir" => {
                    let dir = normalize_dir(value.trim());
                    if dir.is_empty() || (!dir.starts_with('/') && !dir.starts_with('*')) {
                        return Err(SourceError::InvalidInput(
                            "`dir` must be an absolute path or a pattern starting with `*`".into(),
                        ));
                    }
                    account = Some(format!("dir={dir}"));
                    normalized.push((key, dir));
                }
                other => {
                    return Err(SourceError::InvalidInput(format!(
                        "the claude source has no option `{other}`; it accepts `since` and `dir`"
                    )));
                }
            }
        }
        let root = locator.map(PathBuf::from).or_else(default_root).ok_or_else(|| SourceError::InvalidInput("cannot find ~/.claude/projects without HOME; give the projects directory as the locator".into()))?;
        if !root.is_dir() {
            return Err(SourceError::InvalidInput(format!(
                "{} is not a directory of Claude Code transcripts this source can read",
                root.display()
            )));
        }
        Ok(SourceRef {
            source: NAME.into(),
            account,
            locator: root.display().to_string(),
            options: normalized,
        })
    }

    fn default_wing(_source: SourceRef) -> String {
        NAME.into()
    }
    fn default_room() -> String {
        "sessions".into()
    }

    fn discover(source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let option = |name| {
            source
                .options
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let mut entries = Vec::new();
        collect(Path::new(&source.locator), &mut entries);
        if let Some(since) = option("since") {
            let floor = parse_since(since)
                .map_err(SourceError::InvalidInput)?
                .saturating_mul(1_000_000);
            entries.retain(|entry| entry.mtime_ns >= floor);
        }
        if let Some(dir) = option("dir") {
            entries.retain(|entry| {
                std::fs::read_to_string(path_of(&source, &entry.key))
                    .ok()
                    .and_then(|body| record_cwd(&body))
                    .is_some_and(|cwd| glob_match(dir, &normalize_dir(&cwd)))
            });
        }
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
        if !has_retained_message(&body) {
            return Ok(None);
        }
        let first = body
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .next();
        let metadata = json!({ "path": path.display().to_string(), "session_id": first.as_ref().and_then(|record| record.get("sessionId")).cloned().unwrap_or(Value::Null), "cwd": record_cwd(&body) });
        let occurred_at = first
            .as_ref()
            .and_then(|record| record.get("timestamp"))
            .and_then(Value::as_str)
            .filter(|time| {
                time.contains('T')
                    && (time.ends_with('Z') || time.rfind(['+', '-']).is_some_and(|at| at > 10))
            })
            .map(str::to_owned);
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision_of(&body),
            body,
            metadata: metadata.to_string(),
            occurred_at,
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        let mut segments = Vec::new();
        let mut title = None;
        let mut cwd = None;
        let mut session_id = None;
        for line in raw.body.lines() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if record.get("isMeta").and_then(Value::as_bool) == Some(true)
                || record.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
            {
                continue;
            }
            cwd = cwd.or_else(|| record.get("cwd").and_then(Value::as_str).map(str::to_owned));
            session_id = session_id.or_else(|| {
                record
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let message = record.get("message").unwrap_or(&record);
            let role = message
                .get("role")
                .or_else(|| record.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(role, "user" | "assistant") {
                continue;
            }
            let Some(text) = content_text(message.get("content").unwrap_or(&Value::Null)) else {
                continue;
            };
            if title.is_none() && role == "user" {
                title = Some(first_line(&text));
            }
            let timestamp = record
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            segments.push(Segment {
                text: format!("### {role} · {timestamp}\n{text}\n\n"),
            });
        }
        if !segments.is_empty() {
            let id = session_id.unwrap_or_else(|| {
                raw.external_id
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(".jsonl")
                    .to_owned()
            });
            segments.insert(
                0,
                Segment {
                    text: format!(
                        "# Claude Code session {id}\nworking directory: {}\n\n",
                        cwd.as_deref().unwrap_or("unknown")
                    ),
                },
            );
        }
        let metadata: Value = serde_json::from_str(&raw.metadata).unwrap_or(Value::Null);
        Ok(CanonicalDocument {
            title,
            room: cwd
                .as_deref()
                .and_then(|cwd| Path::new(cwd).file_name())
                .map(|name| name.to_string_lossy().into_owned()),
            name: Path::new(&raw.external_id)
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned()),
            kind: SourceKind::Transcript,
            uri: metadata
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned),
            tags: vec!["transcript".into(), NAME.into()],
            segments,
        })
    }
}

export!(Claude);
