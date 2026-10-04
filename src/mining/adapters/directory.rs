//! The `directory` source: the files under a directory on disk, one document per file.
//!
//! What mining did before the unified source model, as an adapter: the same walk (name-ordered, symlinks
//! skipped, noisy directories skipped, unreadable or non-UTF-8 files skipped quietly), now incremental (a
//! modification-time watermark) and chunked (a large file becomes several drawers instead of being skipped).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::domain::{
    Candidate, CanonicalDocument, Cursor, NameKind, RawDocument, Segment, SourceCapabilities,
    SourceKind, SourceRef, validate_name,
};
use crate::error::{Error, Result};

use super::super::adapter::{Discovery, SourceAdapter};
use super::watermark::{Entry, Watermark, mtime_ns, page};

/// The adapter's name.
pub const PROVIDER: &str = "directory";

/// Directories never worth mining — build output, VCS metadata, dependency trees. Skipped by name at any depth,
/// the same cheap denylist mempalace-rs's miner uses before anything fancier (gitignore-awareness) is worth the
/// added dependency.
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

/// Reads the files under a directory.
pub struct DirectoryAdapter {
    /// Files larger than this are skipped rather than truncated silently into a misleading drawer.
    max_file_bytes: u64,
}

impl DirectoryAdapter {
    /// An adapter that skips files over `max_file_bytes`.
    #[must_use]
    pub fn new(max_file_bytes: u64) -> Self {
        Self { max_file_bytes }
    }

    /// The absolute path of `key` under the source's root.
    fn path_of(source: &SourceRef, key: &str) -> PathBuf {
        key.split('/')
            .fold(PathBuf::from(&source.locator), |path, part| path.join(part))
    }
}

impl SourceAdapter for DirectoryAdapter {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn description(&self) -> &'static str {
        "the text files under a directory, one document per file"
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            incremental: true,
            // The file is itself durable and local: keeping a second copy would only double the palace.
            retains_raw: false,
            needs_credentials: false,
        }
    }

    fn identify(&self, locator: Option<&str>) -> Result<SourceRef> {
        let Some(locator) = locator else {
            return Err(Error::invalid_input(
                "path",
                "the directory source needs a directory to mine",
            ));
        };
        let canonical = std::fs::canonicalize(locator)
            .map_err(|source| Error::io(locator.to_string(), source))?;
        if !canonical.is_dir() {
            return Err(Error::invalid_input(
                "path",
                format!("{} is not a directory", canonical.display()),
            ));
        }
        Ok(SourceRef {
            provider: PROVIDER.to_string(),
            account: None,
            locator: canonical.display().to_string(),
        })
    }

    fn default_wing(&self, source: &SourceRef) -> String {
        Path::new(&source.locator).file_name().map_or_else(
            || "unnamed".to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    fn default_room(&self) -> &'static str {
        "files"
    }

    async fn discover(
        &self,
        source: &SourceRef,
        cursor: &Cursor,
        limit: usize,
    ) -> Result<Discovery> {
        let after = Watermark::parse(PROVIDER, cursor)?;
        let mut entries = Vec::new();
        collect(
            Path::new(&source.locator),
            "",
            self.max_file_bytes,
            &mut entries,
        );
        let (candidates, exhausted) = page(entries, after.as_ref(), limit);
        Ok(Discovery {
            candidates,
            exhausted,
        })
    }

    async fn read(&self, source: &SourceRef, candidate: &Candidate) -> Result<Option<RawDocument>> {
        let path = Self::path_of(source, &candidate.handle);
        // Any failure is a skip, not an error: the file may have gone, shrunk or grown since discovery, and a mixed
        // tree of text and binary files is the normal case.
        let Ok(metadata) = std::fs::metadata(&path) else {
            return Ok(None);
        };
        if metadata.len() == 0 || metadata.len() > self.max_file_bytes {
            return Ok(None);
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(None);
        };
        let Ok(body) = String::from_utf8(bytes) else {
            return Ok(None);
        };
        let occurred_at = metadata.modified().ok().map(DateTime::<Utc>::from);
        Ok(Some(RawDocument {
            external_id: candidate.external_id.clone(),
            revision: RawDocument::revision_of(&body),
            body,
            metadata: json!({ "path": path.display().to_string() }),
            occurred_at,
        }))
    }

    fn normalize(&self, raw: &RawDocument) -> Result<CanonicalDocument> {
        Ok(CanonicalDocument {
            title: Some(raw.external_id.clone()),
            room: None,
            // Named by its path under the mined root, so `wing/files/src/lib.rs` addresses it; a path that is not a
            // valid drawer name (a UUID-looking file name, say) is simply left unnamed.
            name: validate_name(NameKind::Drawer, &raw.external_id)
                .is_ok()
                .then(|| raw.external_id.clone()),
            kind: SourceKind::File,
            uri: raw
                .metadata
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            tags: vec![],
            segments: vec![Segment {
                text: raw.body.clone(),
            }],
        })
    }
}

/// Recursively collect the files under `dir` (whose path relative to the root is `prefix`), skipping noisy
/// subtrees, symlinks (a symlink escaping the root is a real hazard for a project miner; not following them is
/// simpler than guarding them), empty files and files over `max_bytes`, and names that are not valid UTF-8.
fn collect(dir: &Path, prefix: &str, max_bytes: u64, out: &mut Vec<Entry>) {
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
            collect(&entry.path(), &key, max_bytes, out);
        } else if file_type.is_file()
            && let Ok(metadata) = entry.metadata()
            && metadata.len() > 0
            && metadata.len() <= max_bytes
        {
            out.push(Entry {
                key,
                mtime_ns: mtime_ns(&metadata),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> DirectoryAdapter {
        DirectoryAdapter::new(1024)
    }

    fn source_of(dir: &Path) -> SourceRef {
        adapter().identify(dir.to_str()).unwrap()
    }

    async fn all(dir: &Path) -> Vec<String> {
        let source = source_of(dir);
        adapter()
            .discover(&source, &Cursor::Null, 1000)
            .await
            .unwrap()
            .candidates
            .into_iter()
            .map(|c| c.external_id)
            .collect()
    }

    #[tokio::test]
    async fn files_are_found_by_their_path_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").unwrap();
        let mut found = all(dir.path()).await;
        found.sort();
        assert_eq!(found, ["README.md", "src/lib.rs"]);
    }

    #[tokio::test]
    async fn noisy_directories_hidden_directories_and_empty_files_are_not_discovered() {
        let dir = tempfile::tempdir().unwrap();
        for noisy in [".git", "target", "node_modules", ".hidden"] {
            std::fs::create_dir(dir.path().join(noisy)).unwrap();
            std::fs::write(dir.path().join(noisy).join("x.txt"), "x").unwrap();
        }
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        std::fs::write(dir.path().join("keep.txt"), "keep").unwrap();
        assert_eq!(all(dir.path()).await, ["keep.txt"]);
    }

    #[tokio::test]
    async fn a_file_over_the_size_limit_is_not_discovered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), "x".repeat(2000)).unwrap();
        std::fs::write(dir.path().join("ok.txt"), "x").unwrap();
        assert_eq!(all(dir.path()).await, ["ok.txt"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("file-link"),
        )
        .unwrap();
        assert!(all(dir.path()).await.is_empty());
    }

    #[tokio::test]
    async fn a_binary_file_is_skipped_at_read_time_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let source = source_of(dir.path());
        let found = adapter()
            .discover(&source, &Cursor::Null, 10)
            .await
            .unwrap();
        let read = adapter().read(&source, &found.candidates[0]).await.unwrap();
        assert!(read.is_none());
    }

    #[tokio::test]
    async fn a_file_that_vanished_since_discovery_is_skipped_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let source = source_of(dir.path());
        let found = adapter()
            .discover(&source, &Cursor::Null, 10)
            .await
            .unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        assert!(
            adapter()
                .read(&source, &found.candidates[0])
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_revision_changes_exactly_when_the_content_does() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "one").unwrap();
        let source = source_of(dir.path());
        let read = |source: SourceRef| async move {
            let found = adapter()
                .discover(&source, &Cursor::Null, 10)
                .await
                .unwrap();
            adapter()
                .read(&source, &found.candidates[0])
                .await
                .unwrap()
                .unwrap()
        };
        let first = read(source.clone()).await;
        let same = read(source.clone()).await;
        std::fs::write(&file, "two").unwrap();
        let changed = read(source).await;
        assert_eq!(first.revision, same.revision);
        assert_ne!(first.revision, changed.revision);
    }

    #[test]
    fn identifying_a_relative_or_trailing_slash_spelling_gives_the_same_source() {
        let dir = tempfile::tempdir().unwrap();
        let plain = adapter().identify(dir.path().to_str()).unwrap();
        let slashed = adapter()
            .identify(Some(&format!("{}/", dir.path().display())))
            .unwrap();
        assert_eq!(
            plain.id(),
            slashed.id(),
            "two spellings of one directory must share a cursor"
        );
    }

    #[test]
    fn identifying_a_missing_directory_or_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            adapter()
                .identify(dir.path().join("nope").to_str())
                .is_err()
        );
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            adapter().identify(file.to_str()),
            Err(Error::InvalidInput { .. })
        ));
        assert!(matches!(
            adapter().identify(None),
            Err(Error::InvalidInput { .. })
        ));
    }

    #[test]
    fn a_document_is_named_by_its_path_unless_that_is_not_a_valid_name() {
        let raw = |id: &str| RawDocument {
            external_id: id.into(),
            revision: "r".into(),
            body: "text".into(),
            metadata: json!({"path": "/x"}),
            occurred_at: None,
        };
        let named = adapter().normalize(&raw("src/lib.rs")).unwrap();
        assert_eq!(named.name.as_deref(), Some("src/lib.rs"));
        let uuid_like = adapter()
            .normalize(&raw("67e55044-10b1-426f-9247-bb680e5fe0c8"))
            .unwrap();
        assert!(
            uuid_like.name.is_none(),
            "a UUID-looking name is reserved for addressing by id"
        );
        assert_eq!(named.kind, SourceKind::File);
        assert_eq!(
            named.segments.len(),
            1,
            "a file is one segment: the chunker decides whether to cut it"
        );
    }
}
