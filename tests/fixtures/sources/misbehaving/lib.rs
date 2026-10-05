//! A source that does what its file names say, to drive every way a component can answer the host.
//!
//! It is compiled by `tests/wasm_projects.rs` in place of the `rust` template's `src/lib.rs`, never as part of
//! MemCastle. `discover` lists every file in the directory; what `read` and `normalize` do depends on the name.

use std::path::Path;

use serde_json::json;

wit_bindgen::generate!({
    path: "wit",
    world: "source",
});

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind, SourceRef,
};

struct Source;

fn raw(candidate: &Candidate, metadata: &str, occurred_at: Option<&str>) -> RawDocument {
    RawDocument {
        external_id: candidate.external_id.clone(),
        revision: "r1".to_string(),
        body: "body".to_string(),
        metadata: metadata.to_string(),
        occurred_at: occurred_at.map(str::to_string),
    }
}

impl Guest for Source {
    fn identify(locator: Option<String>) -> Result<SourceRef, SourceError> {
        let locator = locator.ok_or_else(|| SourceError::InvalidInput("a directory, please".to_string()))?;
        Ok(SourceRef { source: "misbehaving".to_string(), account: None, locator })
    }

    fn default_wing(source: SourceRef) -> String {
        source.locator
    }

    fn default_room() -> String {
        "misc".to_string()
    }

    fn discover(source: SourceRef, _cursor: String, _limit: u32) -> Result<Discovery, SourceError> {
        // A directory called `badcursor` yields a candidate whose cursor is not JSON.
        if source.locator.ends_with("badcursor") {
            return Ok(Discovery {
                candidates: vec![Candidate {
                    external_id: "x".to_string(),
                    cursor_after: "this is not json".to_string(),
                    handle: "x".to_string(),
                }],
                exhausted: true,
            });
        }
        let mut names: Vec<String> = std::fs::read_dir(Path::new(&source.locator))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        let candidates = names
            .into_iter()
            .map(|name| Candidate {
                cursor_after: json!({ "after": name }).to_string(),
                handle: name.clone(),
                external_id: name,
            })
            .collect();
        Ok(Discovery { candidates, exhausted: true })
    }

    fn read(_source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        match candidate.handle.as_str() {
            "badmeta" => Ok(Some(raw(&candidate, "not json", None))),
            "baddate" => Ok(Some(raw(&candidate, "{}", Some("yesterday")))),
            "dated" => Ok(Some(raw(&candidate, "{\"k\":1}", Some("2026-01-02T03:04:05Z")))),
            "invalid" => Err(SourceError::InvalidInput("not mine".to_string())),
            "failed" => Err(SourceError::Failed("boom".to_string())),
            "cursor" => Err(SourceError::CursorInvalid("stale".to_string())),
            "trap" => panic!("guest panic"),
            "skip" => Ok(None),
            _ => Ok(Some(raw(&candidate, "{}", None))),
        }
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        let kind = match raw.external_id.as_str() {
            "manual" => SourceKind::Manual,
            "transcript" => SourceKind::Transcript,
            "other" => SourceKind::Other,
            _ => SourceKind::File,
        };
        Ok(CanonicalDocument {
            title: Some(raw.external_id),
            room: Some("room".to_string()),
            name: Some("name".to_string()),
            kind,
            uri: Some("uri".to_string()),
            tags: vec!["tag".to_string()],
            segments: vec![Segment { text: raw.body }],
        })
    }
}

export!(Source);
