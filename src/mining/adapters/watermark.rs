//! The cursor shared by the adapters that read local files: a modification-time watermark.
//!
//! The cursor `{"mtime_ns": N, "key": K}` means "everything ordered at or before (N, K) is done", where files are
//! ordered by modification time and then by their path relative to the source root. The path breaks ties, so many
//! files saved in the same instant are neither skipped nor read twice, and an unchanged file keeps its place.
//!
//! What it cannot see: a file that appears with an old modification time (restored from a backup, copied with
//! its times preserved) sorts before the watermark and is missed until a `--full` run. A deleted file is not
//! noticed at all. Both are limits of a local watermark, not of the model: a source with real change feeds
//! supplies a cursor that does see them.

use serde_json::json;

use crate::domain::{Candidate, Cursor};
use crate::error::{Error, Result};

/// A file found by a walk: enough to order it and to build its [`Candidate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The path relative to the source root, with `/` separators. The document's identity.
    pub key: String,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
}

/// A parsed cursor: the last entry that is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    mtime_ns: i64,
    key: String,
}

impl Watermark {
    /// Read `cursor` (`null` is the beginning).
    ///
    /// # Errors
    ///
    /// [`Error::SourceCursorInvalid`] naming `source` when `cursor` is neither `null` nor a watermark.
    pub fn parse(source: &str, cursor: &Cursor) -> Result<Option<Self>> {
        if cursor.is_null() {
            return Ok(None);
        }
        let invalid = |message: &str| Error::SourceCursorInvalid {
            adapter: source.to_string(),
            message: message.to_string(),
        };
        let mtime_ns = cursor
            .get("mtime_ns")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| invalid("`mtime_ns` is missing or not an integer"))?;
        let key = cursor
            .get("key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| invalid("`key` is missing or not a string"))?;
        Ok(Some(Self {
            mtime_ns,
            key: key.to_string(),
        }))
    }
}

impl Entry {
    /// The cursor that says this entry (and everything before it) is done.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        json!({ "mtime_ns": self.mtime_ns, "key": self.key })
    }
}

/// The entries strictly after `after`, oldest first, at most `limit` of them as [`Candidate`]s, and whether
/// nothing is left beyond them.
#[must_use]
pub fn page(
    mut entries: Vec<Entry>,
    after: Option<&Watermark>,
    limit: usize,
) -> (Vec<Candidate>, bool) {
    entries.sort_by(|a, b| (a.mtime_ns, &a.key).cmp(&(b.mtime_ns, &b.key)));
    entries.retain(|entry| {
        after.is_none_or(|mark| {
            (entry.mtime_ns, entry.key.as_str()) > (mark.mtime_ns, mark.key.as_str())
        })
    });
    let exhausted = entries.len() <= limit;
    entries.truncate(limit);
    let candidates = entries
        .into_iter()
        .map(|entry| Candidate {
            cursor_after: entry.cursor(),
            handle: entry.key.clone(),
            external_id: entry.key,
        })
        .collect();
    (candidates, exhausted)
}

/// Modification time as nanoseconds since the epoch; zero for a time before it or a platform that cannot say.
#[must_use]
pub fn mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, mtime_ns: i64) -> Entry {
        Entry {
            key: key.into(),
            mtime_ns,
        }
    }

    fn keys(candidates: &[Candidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.external_id.as_str()).collect()
    }

    #[test]
    fn entries_come_oldest_first_with_the_path_breaking_ties() {
        let (page, exhausted) = page(vec![entry("b", 2), entry("z", 1), entry("a", 1)], None, 10);
        assert_eq!(keys(&page), ["a", "z", "b"]);
        assert!(exhausted);
    }

    #[test]
    fn a_page_stops_at_the_limit_and_says_more_remain() {
        let (page, exhausted) = page(vec![entry("a", 1), entry("b", 2), entry("c", 3)], None, 2);
        assert_eq!(keys(&page), ["a", "b"]);
        assert!(
            !exhausted,
            "a third entry is left, so the run must say it is truncated"
        );
    }

    #[test]
    fn a_page_exactly_at_the_limit_is_exhausted() {
        let (_, exhausted) = page(vec![entry("a", 1), entry("b", 2)], None, 2);
        assert!(
            exhausted,
            "nothing is left, so 'at the limit' must not read as 'over it'"
        );
    }

    #[test]
    fn a_cursor_excludes_everything_at_or_before_it() {
        let mark = Watermark::parse("p", &entry("b", 2).cursor()).unwrap();
        let (page, _) = page(
            vec![entry("a", 1), entry("b", 2), entry("c", 2), entry("d", 3)],
            mark.as_ref(),
            10,
        );
        assert_eq!(keys(&page), ["c", "d"]);
    }

    #[test]
    fn a_file_saved_in_the_same_instant_as_the_cursor_is_not_skipped() {
        // Same mtime as the watermark but a later path: the tiebreak is what keeps it from being lost.
        let mark = Watermark::parse("p", &entry("a", 5).cursor()).unwrap();
        let (page, _) = page(vec![entry("a", 5), entry("b", 5)], mark.as_ref(), 10);
        assert_eq!(keys(&page), ["b"]);
    }

    #[test]
    fn a_modified_file_comes_back_after_the_cursor() {
        let mark = Watermark::parse("p", &entry("a", 1).cursor()).unwrap();
        let (page, _) = page(vec![entry("a", 9)], mark.as_ref(), 10);
        assert_eq!(keys(&page), ["a"]);
    }

    #[test]
    fn each_candidate_carries_the_cursor_that_follows_it() {
        let (page, _) = page(vec![entry("a", 1), entry("b", 2)], None, 10);
        assert_eq!(
            page[0].cursor_after,
            serde_json::json!({"mtime_ns": 1, "key": "a"})
        );
        assert_eq!(
            page[1].cursor_after,
            serde_json::json!({"mtime_ns": 2, "key": "b"})
        );
    }

    #[test]
    fn null_is_the_beginning_and_a_foreign_cursor_is_refused_by_name() {
        assert!(Watermark::parse("p", &Cursor::Null).unwrap().is_none());
        let err = Watermark::parse("directory", &serde_json::json!({"offset": 3})).unwrap_err();
        assert!(
            matches!(err, Error::SourceCursorInvalid { ref adapter, .. } if adapter == "directory")
        );
    }
}
