//! One conversation per document, from a ChatGPT export or the experimental private web interface.
mod export;
mod web;

use std::path::PathBuf;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

wit_bindgen::generate!({
    path: "../../wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind,
    SourceRef,
};

struct ChatGpt;

fn failed(message: impl Into<String>) -> SourceError {
    SourceError::Failed(message.into())
}

fn id_of(value: &Value) -> Option<&str> {
    value
        .get("id")
        .or_else(|| value.get("conversation_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn validate_conversation(value: &Value) -> Result<&str, String> {
    let id = id_of(value).ok_or("conversation has no id")?;
    if let Some(mapping) = value.get("mapping").and_then(Value::as_object) {
        for (node_id, node) in mapping {
            let parent = node.get("parent").unwrap_or(&Value::Null);
            if !node.is_object()
                || !(parent.is_null() || parent.is_string())
                || node
                    .get("message")
                    .is_some_and(|message| !message.is_null() && !message.is_object())
            {
                return Err(format!(
                    "mapping node `{node_id}` has an invalid parent or message"
                ));
            }
        }
    } else if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        if messages.iter().any(|message| !message.is_object()) {
            return Err("conversation messages must be objects".into());
        }
    } else {
        return Err("conversation has neither mapping nor messages".into());
    }
    Ok(id)
}

fn records(source: &SourceRef) -> Result<Vec<Value>, SourceError> {
    if source.locator == "chatgpt:web" {
        let selected = source
            .options
            .iter()
            .find(|(key, _)| key == "projects")
            .map(|(_, value)| {
                let available = web::projects()?;
                web::select_projects(value, &available)
            })
            .transpose()
            .map_err(failed)?;
        return Ok(web::list(selected.as_deref())
            .map_err(failed)?
            .into_iter()
            .map(|listing| {
                json!({
                    "id": listing.id,
                    "project_id": listing.project.as_ref().map(|project| project.id.as_str()),
                    "project_name": listing.project.as_ref().map(|project| project.name.as_str()),
                })
            })
            .collect());
    }
    export::ids(&PathBuf::from(&source.locator)).map_err(failed)
}

fn cursor(cursor: &str) -> Result<(Option<String>, bool), SourceError> {
    let value: Value = serde_json::from_str(cursor)
        .map_err(|_| SourceError::CursorInvalid("the cursor is not JSON".into()))?;
    if value.is_null() {
        return Ok((None, false));
    }
    let key = value
        .get("after")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| SourceError::CursorInvalid("the cursor has no conversation ID".into()))?;
    let done = value
        .get("done")
        .and_then(Value::as_bool)
        .ok_or_else(|| SourceError::CursorInvalid("the cursor has no sweep marker".into()))?;
    Ok((Some(key.to_string()), done))
}

fn revision(body: &str) -> String {
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

fn timestamp(value: &Value) -> Option<String> {
    let value = value.get("create_time")?;
    // Export times are Unix timestamps; web times may already be RFC 3339-like strings.
    if let Some(time) = value.as_str() {
        return Some(time.to_string());
    }
    let seconds = value.as_f64()?;
    let whole = seconds.floor() as i64;
    let nanos = ((seconds - whole as f64) * 1e9) as u32;
    // Avoid adding a date-time dependency just to format the source's own timestamp.
    let date = time::OffsetDateTime::from_unix_timestamp(whole)
        .ok()?
        .replace_nanosecond(nanos)
        .ok()?;
    date.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn content_text(content: &Value) -> String {
    let Some(parts) = content.get("parts").and_then(Value::as_array) else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            part.as_str()
                .or_else(|| part.get("text").and_then(Value::as_str))
        })
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn segments(value: &Value) -> Result<Vec<Segment>, SourceError> {
    let mut entries: Vec<(String, Option<String>, &Value)> = Vec::new();
    if let Some(mapping) = value.get("mapping").and_then(Value::as_object) {
        // Walk parent relationships rather than relying on JSON object order, which differs across exports.
        let mut keys: Vec<_> = mapping.keys().collect();
        keys.sort();
        for key in keys {
            let node = &mapping[key];
            if let Some(message) = node.get("message").filter(|m| !m.is_null()) {
                let parent = node
                    .get("parent")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                entries.push((key.to_string(), parent, message));
            }
        }
        // Ordering by graph depth and ID gives stable parent-before-child output, including divergent branches.
        entries.sort_by_key(|(id, _, _)| {
            let mut depth = 0usize;
            let mut next = id.as_str();
            while depth <= mapping.len() {
                let Some(parent) = mapping
                    .get(next)
                    .and_then(|node| node.get("parent"))
                    .and_then(Value::as_str)
                else {
                    break;
                };
                depth += 1;
                next = parent;
            }
            (depth, id.clone())
        });
    } else if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for (index, message) in messages.iter().enumerate() {
            let id = message
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| index.to_string());
            entries.push((
                id,
                message
                    .get("parent")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                message,
            ));
        }
    } else {
        return Err(failed("conversation has neither mapping nor messages"));
    }

    let mut out = Vec::new();
    for (id, parent, message) in entries {
        let author = message
            .get("author")
            .and_then(|a| a.get("role"))
            .and_then(Value::as_str)
            .or_else(|| message.get("role").and_then(Value::as_str))
            .unwrap_or("unknown");
        let channel = message.get("channel").and_then(Value::as_str).or_else(|| {
            message
                .get("metadata")
                .and_then(|m| m.get("channel"))
                .and_then(Value::as_str)
        });
        // Internal reasoning and tool output are not conversation turns; raw records still retain source metadata.
        if !matches!(author, "user" | "assistant") || channel.is_some_and(|c| c != "final") {
            continue;
        }
        let content = message.get("content").unwrap_or(&Value::Null);
        let text = content_text(content);
        if text.is_empty() {
            continue;
        }
        let model = message
            .get("metadata")
            .and_then(|m| m.get("model_slug"))
            .and_then(Value::as_str)
            .map(|m| format!(" model={m}"))
            .unwrap_or_default();
        let name = message
            .get("author")
            .and_then(|author| author.get("name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(|name| format!(" name={name}"))
            .unwrap_or_default();
        let time = timestamp(message)
            .map(|t| format!(" at {t}"))
            .unwrap_or_default();
        let parent = parent.map(|p| format!(" parent={p}")).unwrap_or_default();
        out.push(Segment {
            text: format!("[{author} id={id}{parent}{name}{model}{time}]\n{text}\n\n"),
        });
    }
    Ok(out)
}

impl Guest for ChatGpt {
    fn identify(
        locator: Option<String>,
        options: Vec<(String, String)>,
    ) -> Result<SourceRef, SourceError> {
        let mut account = None;
        let mut mode = None;
        let mut projects = None;
        for (key, value) in &options {
            if key == "mode" && mode.is_none() && value == "web" {
                mode = Some(value.clone());
            } else if key == "account"
                && account.is_none()
                && !value.is_empty()
                && value.len() <= 80
            {
                account = Some(value.clone());
            } else if key == "projects" && projects.is_none() {
                projects = Some(value.clone());
            } else {
                return Err(SourceError::InvalidInput("use mode=web account=LABEL projects=id:ID,name:NAME for web, or a file path without options for an export".into()));
            }
        }
        if mode.is_some() {
            if account.is_none() {
                return Err(SourceError::InvalidInput(
                    "web mining needs a non-secret account=LABEL to separate account histories"
                        .into(),
                ));
            }
            if locator.is_some() {
                return Err(SourceError::InvalidInput(
                    "mode=web does not take an export file path".into(),
                ));
            }
            // A web run must establish its authorization before the pipeline creates its source record or cursor.
            web::check_auth().map_err(failed)?;
            let mut account = account.unwrap_or_default();
            let mut options = options;
            if let Some(selection) = projects {
                let available = web::projects().map_err(failed)?;
                let selected = web::select_projects(&selection, &available)
                    .map_err(SourceError::InvalidInput)?;
                let ids: Vec<_> = selected.iter().map(|project| project.id.as_str()).collect();
                account = json!({"account": account, "projects": ids}).to_string();
                // Normalizing names into IDs pins this run's discovery to the exact slice it identified.
                if let Some((_, value)) = options.iter_mut().find(|(key, _)| key == "projects") {
                    *value = ids
                        .iter()
                        .map(|id| format!("id:{id}"))
                        .collect::<Vec<_>>()
                        .join(",");
                }
            }
            return Ok(SourceRef {
                source: "chatgpt".into(),
                account: Some(account),
                locator: "chatgpt:web".into(),
                options,
            });
        }
        if account.is_some() || projects.is_some() {
            return Err(SourceError::InvalidInput(
                "account=LABEL and projects=… require mode=web; export files cannot be filtered by project".into(),
            ));
        }
        let locator = locator.ok_or_else(|| {
            SourceError::InvalidInput(
                "give an export ZIP or conversations.json path, or mode=web account=LABEL".into(),
            )
        })?;
        // The host canonicalizes a granted locator before opening its parent; WASI cannot resolve ancestors
        // outside that preopen a second time.
        let path = PathBuf::from(&locator);
        if !path.is_file() {
            return Err(SourceError::InvalidInput(
                "the export locator must be a file".into(),
            ));
        }
        Ok(SourceRef {
            source: "chatgpt".into(),
            account: None,
            locator: path.to_string_lossy().into_owned(),
            options: vec![],
        })
    }

    fn default_wing(_source: SourceRef) -> String {
        "chatgpt".into()
    }
    fn default_room() -> String {
        "conversations".into()
    }

    fn discover(source: SourceRef, previous: String, limit: u32) -> Result<Discovery, SourceError> {
        let (after, done) = cursor(&previous)?;
        let mut items: Vec<_> = records(&source)?
            .into_iter()
            .map(|value| {
                id_of(&value)
                    .map(|id| (id.to_string(), value.clone()))
                    .ok_or_else(|| failed("a ChatGPT conversation has no id"))
            })
            .collect::<Result<_, _>>()?;
        items.sort_by(|a, b| a.0.cmp(&b.0));
        if items.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(failed("the ChatGPT listing has duplicate conversation IDs"));
        }
        let mut remaining: Vec<_> = items
            .iter()
            .filter(|(id, _)| done || after.as_ref().is_none_or(|prev| id > prev))
            .cloned()
            .collect();
        // The last seen conversation may have been deleted since the previous run. Restart the sweep rather than
        // leaving a cursor beyond every remaining ID forever.
        if remaining.is_empty() && after.is_some() && !done {
            remaining = items;
        }
        let count = remaining.len().min(limit as usize);
        let exhausted = count == remaining.len();
        let candidates = remaining
            .into_iter()
            .take(count)
            .enumerate()
            .map(|(index, (id, value))| Candidate {
                cursor_after: json!({"after": id, "done": exhausted && index + 1 == count})
                    .to_string(),
                handle: if source.locator == "chatgpt:web" {
                    value.to_string()
                } else {
                    id.clone()
                },
                external_id: id,
            })
            .collect();
        Ok(Discovery {
            candidates,
            exhausted,
        })
    }

    fn read(source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        let mut project = None;
        let value = if source.locator == "chatgpt:web" {
            let handle: Value = serde_json::from_str(&candidate.handle)
                .map_err(|_| failed("ChatGPT conversation handle is invalid"))?;
            if id_of(&handle) != Some(candidate.external_id.as_str()) {
                return Err(failed("ChatGPT conversation handle names another document"));
            }
            project = match (
                handle.get("project_id").and_then(Value::as_str),
                handle.get("project_name").and_then(Value::as_str),
            ) {
                (Some(id), Some(name)) => Some((id.to_string(), name.to_string())),
                (None, None) => None,
                _ => return Err(failed("ChatGPT project provenance is incomplete")),
            };
            web::conversation(&candidate.external_id).map_err(failed)?
        } else {
            let Some(value) =
                export::conversation(&PathBuf::from(&source.locator), &candidate.handle)
                    .map_err(failed)?
            else {
                return Ok(None);
            };
            value
        };
        if id_of(&value) != Some(candidate.external_id.as_str()) {
            return Err(failed("ChatGPT returned a different conversation ID"));
        }
        let body =
            serde_json::to_string(&value).map_err(|_| failed("cannot encode the conversation"))?;
        if body.len() > export::MAX_CONVERSATION {
            return Err(failed(
                "conversation exceeds 16 MiB; use an export split into smaller files",
            ));
        }
        let occurred_at = timestamp(&value);
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision(&body),
            body,
            metadata: if source.locator == "chatgpt:web" {
                json!({"backend": "web", "project_id": project.as_ref().map(|p| &p.0),
                       "project_name": project.as_ref().map(|p| &p.1)})
                .to_string()
            } else {
                json!({"backend": "export"}).to_string()
            },
            occurred_at,
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        let value: Value = serde_json::from_str(&raw.body)
            .map_err(|_| failed("malformed ChatGPT conversation"))?;
        validate_conversation(&value).map_err(failed)?;
        let id = id_of(&value).unwrap_or_default();
        Ok(CanonicalDocument {
            title: value
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
            room: None,
            name: None,
            kind: SourceKind::Transcript,
            uri: Some(format!("https://chatgpt.com/c/{id}")),
            tags: vec!["chatgpt".into(), "transcript".into()],
            segments: segments(&value)?,
        })
    }
}

export!(ChatGpt);
