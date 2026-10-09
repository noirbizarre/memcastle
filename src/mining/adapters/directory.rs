//! The `directory` source: the files under a directory on disk, one document per file.
//!
//! What mining did before the unified source model, as an adapter: the same walk (name-ordered, symlinks
//! skipped, noisy directories skipped, unreadable or non-UTF-8 files skipped quietly), now incremental (a
//! modification-time watermark) and chunked (a large file becomes several drawers instead of being skipped).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::domain::{
    Candidate, CanonicalDocument, Cursor, NameKind, OptionKind, OptionSpec, Options, RawDocument,
    Segment, SourceCapabilities, SourceKind, SourceRef, TriggerMechanism, TriggerSpec, parse_since,
    supported_triggers, validate_name,
};
use crate::error::{Error, Result};

use super::super::adapter::{Discovery, SourceAdapter};
use super::watermark::{Entry, Watermark, mtime_ns, page};
use crate::project::{project_wing, wing_from_directory};

/// The adapter's name.
pub const NAME: &str = "directory";

/// The options a run of the `directory` source accepts.
#[must_use]
pub fn options() -> Vec<OptionSpec> {
    vec![OptionSpec::new(
        "since",
        "only files modified at or after this date (`2026-09`, `2026-09-14` or an RFC 3339 time)",
        OptionKind::Date,
    )
    .with_breadth(crate::domain::OptionBreadth::Since)]
}

/// What can trigger a run of the `directory` source: a change under the directory, and a call from whatever tells you
/// the directory changed (a sync tool's hook). Neither runs unless the user defines and enables a trigger for it.
#[must_use]
pub fn triggers() -> Vec<TriggerSpec> {
    supported_triggers(&[
        (
            TriggerMechanism::Watch,
            "a file under the directory is created, changed or removed".to_string(),
        ),
        (
            TriggerMechanism::Webhook,
            "an external tool reports that the directory changed".to_string(),
        ),
    ])
}

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
    fn name(&self) -> &'static str {
        NAME
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

    fn identify(&self, locator: Option<&str>, options: &Options) -> Result<SourceRef> {
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
        // `since` only narrows what is read, so it is not part of the identity: a run with an earlier `since` than the
        // last continues from the same cursor and `--full` is how to reach back. It is stored as an RFC 3339 instant so
        // that `discover` has one form to read, whatever spelling the user typed.
        let mut normalised = Options::new();
        for (key, value) in options {
            match key.as_str() {
                "since" => {
                    let since = parse_since(value)
                        .map_err(|message| Error::invalid_input("since", message))?;
                    normalised.insert(key.clone(), since.to_rfc3339());
                }
                other => {
                    return Err(Error::invalid_input(
                        "options",
                        format!("the directory source has no option `{other}`; it accepts `since`"),
                    ));
                }
            }
        }
        Ok(SourceRef::new(NAME, None, canonical.display().to_string()).with_options(normalised))
    }

    fn default_wing(&self, source: &SourceRef) -> String {
        let root = Path::new(&source.locator);
        // A project that declares its wing is mined into it, so mining, wake-up and checkpoints meet in one wing;
        // a directory outside any project takes its own name, made acceptable the way `note` makes it, so a
        // directory named like a UUID is filed under a wing a path can address instead of one it cannot.
        project_wing(root)
            .or_else(|| wing_from_directory(root))
            .unwrap_or_else(|| "unnamed".to_string())
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
        let after = Watermark::parse(NAME, cursor)?;
        let root = PathBuf::from(&source.locator);
        let since = source.options.get("since").cloned();
        let max_file_bytes = self.max_file_bytes;
        // A large tree can take seconds to enumerate and sort. Never pin an HTTP worker on that synchronous walk.
        tokio::task::spawn_blocking(move || {
            let mut entries = Vec::new();
            collect(&root, "", max_file_bytes, &mut entries);
            if let Some(since) = since {
                // `identify` wrote this value; a hand-built invalid one reads everything. Saturate at year 2262.
                if let Ok(instant) = DateTime::parse_from_rfc3339(&since) {
                    let floor = instant.timestamp().saturating_mul(1_000_000_000);
                    entries.retain(|entry| entry.mtime_ns >= floor);
                }
            }
            let (candidates, exhausted) = page(entries, after.as_ref(), limit);
            Discovery {
                candidates,
                exhausted,
            }
        })
        .await
        .map_err(|error| Error::server(format!("directory discovery worker failed: {error}")))
    }

    async fn read(&self, source: &SourceRef, candidate: &Candidate) -> Result<Option<RawDocument>> {
        let path = Self::path_of(source, &candidate.handle);
        let candidate = candidate.clone();
        let max_file_bytes = self.max_file_bytes;
        tokio::task::spawn_blocking(move || read_file(path, candidate, max_file_bytes))
            .await
            .map_err(|error| Error::server(format!("directory read worker failed: {error}")))
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

/// Files can change between discovery and reading; the old skip semantics also apply on the blocking pool.
fn read_file(path: PathBuf, candidate: Candidate, max_file_bytes: u64) -> Option<RawDocument> {
    // Any failure is a skip, not an error: the file may have gone, shrunk or grown since discovery, and a mixed
    // tree of text and binary files is the normal case.
    let Ok(metadata) = std::fs::metadata(&path) else {
        return None;
    };
    if metadata.len() == 0 || metadata.len() > max_file_bytes {
        return None;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return None;
    };
    let Ok(body) = String::from_utf8(bytes) else {
        return None;
    };
    let occurred_at = metadata.modified().ok().map(DateTime::<Utc>::from);
    Some(RawDocument {
        external_id: candidate.external_id,
        revision: RawDocument::revision_of(&body),
        body,
        metadata: json!({ "path": path.display().to_string() }),
        occurred_at,
    })
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
        adapter().identify(dir.to_str(), &Options::new()).unwrap()
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

    #[test]
    fn a_mined_directory_takes_the_wing_its_project_declares_else_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join(".config")).unwrap();
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(
            project.join(".config/memcastle.toml"),
            "[memcastle]\nwing = \"declared\"\n",
        )
        .unwrap();
        let wing = |d: &Path| adapter().default_wing(&source_of(d));
        assert_eq!(wing(&plain), "plain");
        assert_eq!(wing(&project), "declared");
        // A subdirectory of a project is part of it: its wing is the project's, not its own name.
        assert_eq!(wing(&project.join("docs")), "declared");
    }

    #[test]
    fn a_directory_named_like_a_uuid_is_mined_into_a_wing_a_path_can_address() {
        let dir = tempfile::tempdir().unwrap();
        let uuid_named = dir.path().join("0b8c1e8e-7a52-4a1c-9d0e-6f0a3b2c1d4e");
        std::fs::create_dir_all(&uuid_named).unwrap();
        let wing = adapter().default_wing(&source_of(&uuid_named));
        assert_eq!(wing, "project-0b8c1e8e-7a52-4a1c-9d0e-6f0a3b2c1d4e");
        assert!(validate_name(NameKind::Wing, &wing).is_ok());
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

    fn since(value: &str) -> Options {
        Options::from([("since".to_string(), value.to_string())])
    }

    #[tokio::test]
    async fn since_leaves_out_the_files_not_modified_since_that_date() {
        let dir = tempfile::tempdir().unwrap();
        for (name, date) in [
            ("old.txt", "2026-01-10T00:00:00Z"),
            ("new.txt", "2026-09-20T00:00:00Z"),
        ] {
            let file = std::fs::File::create(dir.path().join(name)).unwrap();
            std::io::Write::write_all(&mut &file, b"x").unwrap();
            let at = DateTime::parse_from_rfc3339(date).unwrap();
            file.set_modified(std::time::SystemTime::from(at)).unwrap();
        }
        let found = |value: &str| {
            let source = adapter()
                .identify(dir.path().to_str(), &since(value))
                .unwrap();
            async move {
                adapter()
                    .discover(&source, &Cursor::Null, 100)
                    .await
                    .unwrap()
                    .candidates
                    .into_iter()
                    .map(|c| c.external_id)
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(
            found("2026-09").await,
            ["new.txt"],
            "a month is its first day"
        );
        assert_eq!(
            found("2026-01-10").await,
            ["old.txt", "new.txt"],
            "the date itself is included"
        );
        assert!(found("2027-01").await.is_empty());
    }

    #[test]
    fn since_does_not_change_which_source_a_directory_is() {
        let dir = tempfile::tempdir().unwrap();
        let plain = source_of(dir.path());
        let narrowed = adapter()
            .identify(dir.path().to_str(), &since("2026-09"))
            .unwrap();
        assert_eq!(
            plain.id(),
            narrowed.id(),
            "a narrower run must continue the same cursor, not start one of its own"
        );
        assert_eq!(narrowed.options["since"], "2026-09-01T00:00:00+00:00");
    }

    #[test]
    fn an_option_the_directory_source_does_not_have_or_a_bad_date_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let unknown = Options::from([("dir".to_string(), "/x".to_string())]);
        let error = adapter()
            .identify(dir.path().to_str(), &unknown)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("`dir`") && error.contains("`since`"),
            "{error}"
        );
        let error = adapter()
            .identify(dir.path().to_str(), &since("last week"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`last week`"), "{error}");
    }

    #[test]
    fn identifying_a_relative_or_trailing_slash_spelling_gives_the_same_source() {
        let dir = tempfile::tempdir().unwrap();
        let plain = adapter()
            .identify(dir.path().to_str(), &Options::new())
            .unwrap();
        let slashed = adapter()
            .identify(Some(&format!("{}/", dir.path().display())), &Options::new())
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
                .identify(dir.path().join("nope").to_str(), &Options::new())
                .is_err()
        );
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            adapter().identify(file.to_str(), &Options::new()),
            Err(Error::InvalidInput { .. })
        ));
        assert!(matches!(
            adapter().identify(None, &Options::new()),
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
