//! __NAME__: a MemCastle source that wraps a command-line program, written as a WebAssembly component.
//!
//! The component does not read anything itself: it asks the host to run the programs `memcastle-source.toml` lists
//! under `[permissions] process`, by exact name and without a shell, and turns their output into documents. Here the
//! programs are `ls` and `cat`, to show the shape on something every machine has; replace them with the tool you are
//! wrapping and change the manifest to match.
//!
//! Build it with `memcastle source build`, check it with `memcastle source test`, and ship it with
//! `memcastle source package`.

use serde_json::{Value, json};

wit_bindgen::generate!({
    path: "wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::host::run_process;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind, SourceRef,
};

/// This source's source name; it must match `source.name` in `memcastle-source.toml`.
const NAME: &str = "__NAME__";

struct Source;

/// Run a granted program and return its standard output, or `None` when it exits unsuccessfully.
fn output(program: &str, args: &[String]) -> Result<Option<Vec<u8>>, SourceError> {
    // A program that is not in the manifest is refused by the host, and the message says so.
    let result = run_process(program, args, None).map_err(SourceError::Failed)?;
    Ok((result.status == 0).then_some(result.stdout))
}

/// The cursor is `{"after": "<name>"}`: everything up to and including that name is done. `null` is the beginning.
fn parse_cursor(cursor: &str) -> Result<Option<String>, SourceError> {
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
            return Err(SourceError::InvalidInput("give the directory to list".to_string()));
        };
        Ok(SourceRef { source: NAME.to_string(), account: None, locator, options: Vec::new() })
    }

    fn default_wing(source: SourceRef) -> String {
        source
            .locator
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("unnamed")
            .to_string()
    }

    fn default_room() -> String {
        "notes".to_string()
    }

    fn discover(source: SourceRef, cursor: String, limit: u32) -> Result<Discovery, SourceError> {
        let after = parse_cursor(&cursor)?;
        let Some(listing) = output("ls", &["-1".to_string(), source.locator.clone()])? else {
            return Err(SourceError::Failed(format!("`ls` could not list {}", source.locator)));
        };
        let mut names: Vec<String> = String::from_utf8_lossy(&listing)
            .lines()
            .filter(|name| name.ends_with(".txt"))
            .map(str::to_string)
            .collect();
        names.sort();
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
        let path = format!("{}/{}", source.locator.trim_end_matches('/'), candidate.handle);
        let Some(bytes) = output("cat", &[path.clone()])? else {
            return Ok(None);
        };
        let Ok(body) = String::from_utf8(bytes) else {
            return Ok(None);
        };
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: revision_of(&body),
            body,
            metadata: json!({ "path": path }).to_string(),
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
