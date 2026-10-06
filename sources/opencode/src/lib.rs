//! The OpenCode history source: the conversation history of the OpenCode coding agent, acquired through its own CLI.
//!
//! OpenCode (1.2 and later) keeps every session in one SQLite database that it writes while it runs. A component with
//! read-only file access cannot open a live, write-ahead-logged database safely, and linking a SQLite engine into a
//! source would only copy OpenCode's storage format into MemCastle. So this source asks OpenCode instead, through the
//! one program its manifest grants (`opencode`): `opencode db` lists which sessions changed since the cursor, and
//! `opencode export <session>` returns one session as JSON. No model is called and no session is replayed, and it works
//! on history from sessions that ended long ago.
//!
//! What it files, per session: a header (session id and working directory) and each user and assistant message as text,
//! with the time it was written. Tool calls are kept as one-line markers so the shape of the work stays visible.
//! What it leaves out on purpose: model reasoning, tool outputs, patches and snapshots (large, and usually file contents
//! that can be mined as files), text OpenCode injected itself (`synthetic` or `ignored` parts), and the summaries it
//! writes when compacting a long session (they restate messages that are filed already). A part type this source does
//! not know is skipped, not an error: the export format is OpenCode's to evolve, and a reader that stopped at the first
//! surprise would stop mining all history.
//!
//! The one thing coupled to OpenCode's internals is the discovery query over its `session` table (`id`, `time_updated`):
//! `opencode session list` has no way to ask only for what changed. A schema change shows up as a failed query with
//! OpenCode's own message, never as silently missing sessions.
//!
//! Acquisition is all this does: MemCastle chunks, deduplicates, files and remembers what it returns.

use serde_json::{Value, json};

wit_bindgen::generate!({
    path: "../../wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::host::run_process;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind,
    SourceRef,
};

/// This source's name; it must match `source.name` in the manifest.
const NAME: &str = "opencode";

/// The one program this source runs; it must be listed under `permissions.process` in the manifest.
const PROGRAM: &str = "opencode";

/// The longest title or tool label taken from a text, in characters.
const LABEL_CHARS: usize = 80;

/// How much of a failed command's standard error is quoted back to the user, in characters.
const STDERR_CHARS: usize = 300;

struct OpenCode;

/// What a finished `opencode` run produced, when it succeeded.
struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

/// Run `opencode` with `args`. A refusal by the host (the program is not granted, it is not installed, it ran too long)
/// is a failure that says so; a non-zero exit is the caller's to interpret.
fn opencode(args: &[&str]) -> Result<Run, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let output = run_process(PROGRAM, &args, None)?;
    Ok(Run {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// A failed run as an error that says what ran and what OpenCode answered.
fn failure(what: &str, run: &Run) -> SourceError {
    let stderr: String = run.stderr.trim().chars().take(STDERR_CHARS).collect();
    SourceError::Failed(format!(
        "`opencode {what}` exited with status {}{}; check that `opencode` on the daemon's PATH is a current installation (1.2 or later) and that `opencode {what}` works in a terminal",
        run.status,
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    ))
}

/// The JSON value in `text`, which starts at its first `open` character: OpenCode may print a line of its own (a
/// migration notice) before the document, and that line is not ours to choke on.
fn json_from(text: &str, open: char) -> Option<Value> {
    let start = text.find(open)?;
    serde_json::from_str(text[start..].trim()).ok()
}

/// A number of milliseconds as JSON gives it: an integer, or a float for a timestamp OpenCode wrote with a fraction.
fn millis(value: &Value) -> Option<i64> {
    value
        .as_i64()
        // Truncating a fraction of a millisecond is the intent.
        .or_else(|| value.as_f64().map(|float| float as i64))
}

/// Whether `id` can be written into a query as it is: OpenCode's ids are `ses_` and base-62 characters. The cursor is
/// the only place one comes from that this source did not just read from OpenCode, and a query cannot bind parameters.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// The cursor `{"updated", "id"}` means "every session ordered at or before this is done"; `null` is the beginning.
fn parse_cursor(cursor: &str) -> Result<Option<(i64, String)>, SourceError> {
    let invalid = |message: &str| SourceError::CursorInvalid(message.to_string());
    let value: Value =
        serde_json::from_str(cursor).map_err(|_| invalid("the cursor is not JSON"))?;
    if value.is_null() {
        return Ok(None);
    }
    let updated = value
        .get("updated")
        .and_then(Value::as_i64)
        .filter(|updated| *updated >= 0)
        .ok_or_else(|| invalid("`updated` is missing or not a non-negative integer"))?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| is_plain_id(id))
        .ok_or_else(|| invalid("`id` is missing or not a session id"))?;
    Ok(Some((updated, id.to_string())))
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

/// Epoch milliseconds as an RFC 3339 UTC timestamp, or `None` outside years 0000 to 9999.
///
/// OpenCode stores integers, and MemCastle fails a whole job on an `occurred-at` it cannot parse, so the conversion is
/// exact (days to a civil date, after Howard Hinnant) and a value out of range is left out rather than guessed at.
fn rfc3339(epoch_ms: i64) -> Option<String> {
    let seconds = epoch_ms.div_euclid(1000);
    let fraction = epoch_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    if !(0..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction:03}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    ))
}

/// The first line of `text`, cut to [`LABEL_CHARS`] characters.
fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(LABEL_CHARS)
        .collect()
}

/// One part of a message as text, or `None` when it is not worth filing (reasoning, outputs, patches, anything unknown).
fn part_text(part: &Value) -> Option<String> {
    match part.get("type").and_then(Value::as_str)? {
        "text" => {
            // Text OpenCode added on the user's behalf (an expanded command, a file's contents) is not what was said.
            let flagged = |key: &str| part.get(key).and_then(Value::as_bool) == Some(true);
            if flagged("synthetic") || flagged("ignored") {
                return None;
            }
            let text = part.get("text").and_then(Value::as_str)?.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        "tool" => {
            let name = part.get("tool").and_then(Value::as_str).unwrap_or("tool");
            // The title is OpenCode's one-line description of the call ("Read src/lib.rs"), present once it ran.
            let title = part
                .pointer("/state/title")
                .and_then(Value::as_str)
                .map(first_line)
                .filter(|title| !title.is_empty());
            Some(match title {
                Some(title) => format!("[tool: {name}] {title}"),
                None => format!("[tool: {name}]"),
            })
        }
        // The name of an attached file, never its contents (`url` can be the whole file, encoded).
        "file" => part
            .get("filename")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| format!("[file: {name}]")),
        _ => None,
    }
}

/// A message as the segment it files, or `None` when nothing of it is kept. Also the time it was written.
fn message_segment(message: &Value) -> Option<(String, String)> {
    let info = message.get("info")?;
    let role = info.get("role").and_then(Value::as_str)?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    // The summary OpenCode writes when it compacts a long session restates messages that are filed already.
    if info.get("summary").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = message
        .get("parts")?
        .as_array()?
        .iter()
        .filter_map(part_text)
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return None;
    }
    let at = info
        .pointer("/time/created")
        .and_then(millis)
        .and_then(rfc3339)
        .unwrap_or_default();
    Some((role.to_string(), format!("### {role} · {at}\n{text}\n\n")))
}

impl Guest for OpenCode {
    fn identify(locator: Option<String>) -> Result<SourceRef, SourceError> {
        // Asked first, so a machine without OpenCode fails here with a message that says how to fix it, not on the
        // first query of a job.
        let run = opencode(&["db", "path"]).map_err(|message| {
            SourceError::Failed(format!(
                "{message}; this source needs the `opencode` command on the daemon's PATH (https://opencode.ai)"
            ))
        })?;
        if run.status != 0 {
            return Err(failure("db path", &run));
        }
        // A locator is only a name for the history being mined: OpenCode decides where its database is, and this
        // source reads nothing from the file system. Without one, the identity is the database itself, so a changed
        // `XDG_DATA_HOME` is a different source with its own cursor.
        let locator = match locator {
            Some(locator) => locator,
            None => {
                let path = run.stdout.trim();
                if path.is_empty() {
                    return Err(SourceError::Failed(
                        "`opencode db path` printed nothing; give a name for this history with --locator"
                            .to_string(),
                    ));
                }
                path.to_string()
            }
        };
        Ok(SourceRef {
            source: NAME.to_string(),
            account: None,
            locator,
        })
    }

    fn default_wing(_source: SourceRef) -> String {
        "opencode".to_string()
    }

    fn default_room() -> String {
        "sessions".to_string()
    }

    fn discover(_source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let (updated, id) = after.unwrap_or((-1, String::new()));
        // One more than asked for says whether there is a next page. Ordered by (time_updated, id) and strictly after the
        // cursor, so sessions saved in the same millisecond are neither skipped nor read twice. Both values were checked
        // above, which is what makes writing them into the query safe.
        let query = format!(
            "SELECT id, time_updated FROM session WHERE time_updated > {updated} OR (time_updated = {updated} AND id > '{id}') ORDER BY time_updated, id LIMIT {}",
            u64::from(limit) + 1
        );
        let run = opencode(&["db", "--format", "json", &query]).map_err(SourceError::Failed)?;
        if run.status != 0 {
            return Err(failure("db", &run));
        }
        let Some(Value::Array(rows)) = json_from(&run.stdout, '[') else {
            // What came back is quoted, so a cut-off answer is told apart from an old OpenCode (which has no `db`
            // command to answer with JSON at all) instead of both being blamed on the version.
            let seen: String = run.stdout.trim().chars().take(STDERR_CHARS).collect();
            return Err(SourceError::Failed(format!(
                "`opencode db --format json` did not answer with a complete JSON array ({} bytes, starting {seen:?}); \
                 either its output was cut off or this source needs OpenCode 1.2 or later",
                run.stdout.len()
            )));
        };
        let mut candidates = Vec::new();
        for row in &rows {
            let (Some(id), Some(updated)) = (
                row.get("id").and_then(Value::as_str),
                row.get("time_updated").and_then(millis),
            ) else {
                return Err(SourceError::Failed(
                    "OpenCode's `session` table no longer has `id` and `time_updated`; this source needs updating for this OpenCode version"
                        .to_string(),
                ));
            };
            // An id this source would refuse in a cursor cannot be resumed from, so it is not offered.
            if !is_plain_id(id) {
                continue;
            }
            candidates.push(Candidate {
                cursor_after: json!({ "updated": updated, "id": id }).to_string(),
                handle: id.to_string(),
                external_id: id.to_string(),
            });
        }
        let limit = limit as usize;
        let exhausted = rows.len() <= limit;
        candidates.truncate(limit);
        Ok(Discovery {
            candidates,
            exhausted,
        })
    }

    fn read(_source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        // The id goes to OpenCode as an argument, never a shell word, but one that begins with `-` could still be taken
        // for an option.
        if !is_plain_id(&candidate.handle) {
            return Ok(None);
        }
        let run = match opencode(&["export", &candidate.handle]) {
            Ok(run) => run,
            // A session too large to come back in one answer is skipped, not fatal: it would otherwise fail every job
            // at the same place, and everything after it would never be mined.
            Err(message) if message.contains("of output") => return Ok(None),
            Err(message) => return Err(SourceError::Failed(message)),
        };
        // Gone since discovery (deleted), or not exportable: the next run decides again.
        if run.status != 0 {
            return Ok(None);
        }
        let Some(session) = json_from(&run.stdout, '{') else {
            return Ok(None);
        };
        let messages = session.get("messages").and_then(Value::as_array);
        // A session nobody has written in has nothing to file; it is rediscovered when its `time_updated` moves.
        if messages.is_none_or(Vec::is_empty) {
            return Ok(None);
        }
        // The document as OpenCode printed it from its first brace, without anything it said before.
        let body = run.stdout[run.stdout.find('{').unwrap_or(0)..]
            .trim()
            .to_string();
        let info = session.get("info").unwrap_or(&Value::Null);
        let field = |pointer: &str| info.pointer(pointer).cloned().unwrap_or(Value::Null);
        let occurred_at = info
            .pointer("/time/created")
            .and_then(millis)
            .and_then(rfc3339);
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision_of(&body),
            body,
            metadata: json!({
                "session_id": field("/id"),
                "directory": field("/directory"),
                "project_id": field("/projectID"),
                "title": field("/title"),
                "version": field("/version"),
                "parent_id": field("/parentID"),
            })
            .to_string(),
            occurred_at,
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        // The body is what `read` took from OpenCode and checked; one that is not JSON files nothing rather than
        // failing the job.
        let session: Value = serde_json::from_str(&raw.body).unwrap_or(Value::Null);
        let info = session.get("info").unwrap_or(&Value::Null);

        let mut messages: Vec<&Value> = session
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| messages.iter().collect())
            .unwrap_or_default();
        // Creation time, then id (ids ascend with creation): imported messages do not always have ascending ids, and the
        // order must not depend on how OpenCode happened to list them.
        messages.sort_by_key(|message| {
            let info = message.get("info");
            (
                info.and_then(|i| i.pointer("/time/created"))
                    .and_then(millis)
                    .unwrap_or(0),
                info.and_then(|i| i.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        });

        let mut title = None;
        let mut filed = Vec::new();
        for message in messages {
            let Some((role, text)) = message_segment(message) else {
                continue;
            };
            if title.is_none() && role == "user" {
                // The body of the segment after its `### role · time` line.
                title = text.lines().nth(1).map(first_line);
            }
            filed.push(Segment { text });
        }

        let directory = info.get("directory").and_then(Value::as_str);
        let id = info
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&raw.external_id);
        // The header carries nothing that changes while a session goes on (OpenCode renames sessions as they start), so
        // the segments of a session that grew stay byte-identical up to its new tail.
        let mut segments = Vec::new();
        if !filed.is_empty() {
            segments.push(Segment {
                text: format!(
                    "# OpenCode session {id}\nworking directory: {}\n\n",
                    directory.unwrap_or("unknown")
                ),
            });
            segments.extend(filed);
        }

        let named = info
            .get("title")
            .and_then(Value::as_str)
            .map(first_line)
            .filter(|title| !title.is_empty());
        Ok(CanonicalDocument {
            title: named.or(title),
            room: directory
                .and_then(|directory| directory.trim_end_matches('/').rsplit('/').next())
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            name: Some(id.to_string()),
            kind: SourceKind::Transcript,
            uri: Some(format!("opencode://session/{id}")),
            tags: vec!["transcript".to_string(), "opencode".to_string()],
            segments,
        })
    }
}

export!(OpenCode);
