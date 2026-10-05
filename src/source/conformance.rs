//! The conformance cases every source is held to, native or WebAssembly (docs/adr/026).
//!
//! A case is a directory holding `case.json` and the tree a source is asked to mine. The runner is generic over
//! [`SourceAdapter`], so the very same cases run against a built-in adapter and against a loaded component: if they
//! disagree on what a source must do, a case fails for one and not the other.
//!
//! What a case pins down is the contract, not an adapter's taste: how paging resumes, what a bad cursor does, that a
//! revision and a normalization are stable, and which documents come out with which canonical fields. What differs
//! legitimately between sources (the shape of a cursor, a drawer name, a revision's format) is not compared.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::domain::{Candidate, CanonicalDocument, Cursor, SourceRef};
use crate::error::{Error, Result};
use crate::mining::adapter::SourceAdapter;

/// The file that makes a directory a case.
pub const CASE_FILE: &str = "case.json";

/// The most pages a case may take to be discovered. A source that never says it is exhausted fails instead of
/// hanging the run.
const MAX_PAGES: usize = 1_000;

/// One conformance case, as `case.json` states it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// The case's name.
    pub name: String,
    /// What it checks, in a sentence.
    pub description: String,
    /// The tree to mine, relative to the case's directory.
    pub locator: String,
    /// The page size discovery is asked for. Smaller than the number of documents, so paging is exercised.
    pub limit: usize,
    /// The documents the source must produce, and what each must normalize to.
    pub documents: Vec<ExpectedDocument>,
    /// Things in the tree that must produce no document: found and then skipped, or never found.
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// A document a case expects.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedDocument {
    /// Its identity within the source.
    pub external_id: String,
    /// Its normalized title.
    pub title: Option<String>,
    /// Its normalized kind (`file`, `manual`, `transcript`, `other`).
    pub kind: String,
    /// Its normalized tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Its normalized room; `null` files it under the source's default room.
    #[serde(default)]
    pub room: Option<String>,
    /// The text of its segments, in order.
    pub segments: Vec<String>,
}

/// What running one case found.
#[derive(Debug, Clone)]
pub struct CaseReport {
    /// The case's name.
    pub name: String,
    /// What went wrong; empty when the case passed.
    pub failures: Vec<String>,
}

/// What running a directory of cases found.
#[derive(Debug, Clone)]
pub struct Report {
    /// One entry per case, by name.
    pub cases: Vec<CaseReport>,
}

impl Report {
    /// Whether every case passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.cases.iter().all(|case| case.failures.is_empty())
    }
}

/// Every case under `dir`, by name, with its tree resolved to an absolute path.
///
/// # Errors
///
/// [`Error::Io`] when a case cannot be read and [`Error::SourceManifestInvalid`] when a `case.json` does not parse.
pub fn load_cases(dir: &Path) -> Result<Vec<(Case, PathBuf)>> {
    let mut cases = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir.display().to_string(), e))?;
    for entry in entries.flatten() {
        let file = entry.path().join(CASE_FILE);
        if !file.is_file() {
            continue;
        }
        let text =
            std::fs::read_to_string(&file).map_err(|e| Error::io(file.display().to_string(), e))?;
        let case: Case = serde_json::from_str(&text).map_err(|e| Error::SourceManifestInvalid {
            message: format!("{}: {e}", file.display()),
        })?;
        let tree = entry.path().join(&case.locator);
        let tree =
            std::fs::canonicalize(&tree).map_err(|e| Error::io(tree.display().to_string(), e))?;
        cases.push((case, tree));
    }
    cases.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    Ok(cases)
}

/// Run every case under `dir` against `adapter`.
///
/// # Errors
///
/// As [`load_cases`]. A case that fails is in the report, not an error.
pub async fn run_all<A: SourceAdapter>(adapter: &A, dir: &Path) -> Result<Report> {
    let mut cases = Vec::new();
    for (case, tree) in load_cases(dir)? {
        let failures = run_case(adapter, &case, &tree).await;
        cases.push(CaseReport {
            name: case.name,
            failures,
        });
    }
    if cases.is_empty() {
        return Err(Error::SourceManifestInvalid {
            message: format!(
                "no conformance cases under {}: each is a directory holding `{CASE_FILE}`",
                dir.display()
            ),
        });
    }
    Ok(Report { cases })
}

/// The failures of one case; none means it passed.
pub async fn run_case<A: SourceAdapter>(adapter: &A, case: &Case, tree: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    let mut fail = |message: String| failures.push(message);

    if adapter.name().is_empty() || adapter.description().trim().is_empty() {
        fail("the source has no name or description".to_string());
    }
    if adapter.description().contains('\n') {
        fail("the description is more than one line".to_string());
    }

    let locator = tree.display().to_string();
    let source = match adapter.identify(Some(&locator)) {
        Ok(source) => source,
        Err(error) => {
            fail(format!("identify({locator}) failed: {error}"));
            return failures;
        }
    };
    if source.source != adapter.name() {
        fail(format!(
            "identify returned source `{}`, not `{}`",
            source.source,
            adapter.name()
        ));
    }
    match adapter.identify(Some(&locator)) {
        Ok(again) if again == source => {}
        Ok(_) => fail("identify gave a different identity for the same locator".to_string()),
        Err(error) => fail(format!("identify failed the second time: {error}")),
    }

    // A cursor the source did not produce must be refused as such, so MemCastle can say how to recover.
    match adapter
        .discover(&source, &json!("not a cursor"), case.limit)
        .await
    {
        Err(Error::SourceCursorInvalid { .. }) => {}
        Ok(_) => fail("a cursor the source never produced was accepted".to_string()),
        Err(other) => fail(format!(
            "a cursor the source never produced gave `{other}`, not a cursor-invalid error"
        )),
    }

    let Some((candidates, cursor)) = discover_all(adapter, &source, case, &mut fail).await else {
        return failures;
    };

    // After the last cursor there must be nothing: that is what makes a source incremental.
    if adapter.capabilities().incremental {
        match adapter.discover(&source, &cursor, case.limit).await {
            Ok(tail) if tail.candidates.is_empty() && tail.exhausted => {}
            Ok(tail) => fail(format!(
                "discovery after the last cursor found {} more candidates (exhausted: {})",
                tail.candidates.len(),
                tail.exhausted
            )),
            Err(error) => fail(format!("discovery after the last cursor failed: {error}")),
        }
    }

    let mut produced = Vec::new();
    for candidate in &candidates {
        let first = match adapter.read(&source, candidate).await {
            Ok(Some(raw)) => raw,
            Ok(None) => continue,
            Err(error) => {
                fail(format!("read({}) failed: {error}", candidate.external_id));
                continue;
            }
        };
        if first.external_id != candidate.external_id {
            fail(format!(
                "read({}) returned a document called `{}`",
                candidate.external_id, first.external_id
            ));
        }
        match adapter.read(&source, candidate).await {
            Ok(Some(second)) if second.revision == first.revision => {}
            _ => fail(format!(
                "reading {} twice gave different revisions",
                candidate.external_id
            )),
        }
        let normalized = adapter.normalize(&first);
        match (&normalized, adapter.normalize(&first)) {
            (Ok(one), Ok(two)) if one == &two => {}
            _ => fail(format!(
                "normalizing {} twice did not give the same document",
                candidate.external_id
            )),
        }
        match normalized {
            Ok(canonical) => produced.push((first.external_id, canonical)),
            Err(error) => fail(format!(
                "normalize({}) failed: {error}",
                candidate.external_id
            )),
        }
    }

    let mut produced_ids: Vec<&str> = produced.iter().map(|(id, _)| id.as_str()).collect();
    produced_ids.sort_unstable();
    let mut expected_ids: Vec<&str> = case
        .documents
        .iter()
        .map(|d| d.external_id.as_str())
        .collect();
    expected_ids.sort_unstable();
    if produced_ids != expected_ids {
        fail(format!(
            "the documents produced are {produced_ids:?}, expected {expected_ids:?}"
        ));
    }
    for skipped in &case.skipped {
        if produced_ids.contains(&skipped.as_str()) {
            fail(format!(
                "`{skipped}` should have been skipped but produced a document"
            ));
        }
    }
    for expected in &case.documents {
        if let Some((_, canonical)) = produced.iter().find(|(id, _)| id == &expected.external_id) {
            compare(expected, canonical, &mut fail);
        }
    }
    failures
}

/// Walk discovery to its end, checking each page; the candidates in order and the final cursor, or `None` when
/// paging is too broken to go on.
async fn discover_all<A: SourceAdapter>(
    adapter: &A,
    source: &SourceRef,
    case: &Case,
    fail: &mut impl FnMut(String),
) -> Option<(Vec<Candidate>, Cursor)> {
    let mut cursor = Cursor::Null;
    let mut seen: Vec<Candidate> = Vec::new();
    for _ in 0..MAX_PAGES {
        let page = match adapter.discover(source, &cursor, case.limit).await {
            Ok(page) => page,
            Err(error) => {
                fail(format!("discover failed: {error}"));
                return None;
            }
        };
        if page.candidates.len() > case.limit {
            fail(format!(
                "a page had {} candidates, more than the limit {}",
                page.candidates.len(),
                case.limit
            ));
        }
        for candidate in &page.candidates {
            if seen.iter().any(|c| c.external_id == candidate.external_id) {
                fail(format!(
                    "`{}` was discovered twice: resuming from a cursor must start strictly after it",
                    candidate.external_id
                ));
            }
        }
        if let Some(last) = page.candidates.last() {
            cursor = last.cursor_after.clone();
        } else if !page.exhausted {
            fail("a page was empty but not exhausted, so paging cannot make progress".to_string());
            return None;
        }
        seen.extend(page.candidates);
        if page.exhausted {
            return Some((seen, cursor));
        }
    }
    fail(format!(
        "discovery was not exhausted after {MAX_PAGES} pages"
    ));
    None
}

fn compare(expected: &ExpectedDocument, actual: &CanonicalDocument, fail: &mut impl FnMut(String)) {
    let id = &expected.external_id;
    if actual.title != expected.title {
        fail(format!(
            "{id}: title is {:?}, expected {:?}",
            actual.title, expected.title
        ));
    }
    let kind = serde_json::to_value(actual.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    if kind != expected.kind {
        fail(format!(
            "{id}: kind is `{kind}`, expected `{}`",
            expected.kind
        ));
    }
    if actual.tags != expected.tags {
        fail(format!(
            "{id}: tags are {:?}, expected {:?}",
            actual.tags, expected.tags
        ));
    }
    if actual.room != expected.room {
        fail(format!(
            "{id}: room is {:?}, expected {:?}",
            actual.room, expected.room
        ));
    }
    let segments: Vec<&str> = actual.segments.iter().map(|s| s.text.as_str()).collect();
    let wanted: Vec<&str> = expected.segments.iter().map(String::as_str).collect();
    if segments != wanted {
        fail(format!(
            "{id}: segments are {segments:?}, expected {wanted:?}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RawDocument;
    use crate::mining::adapters::directory::DirectoryAdapter;

    fn case(dir: &Path, expected_ids: &[&str]) -> Case {
        Case {
            name: "t".into(),
            description: "t".into(),
            locator: dir.display().to_string(),
            limit: 1,
            documents: expected_ids
                .iter()
                .map(|id| ExpectedDocument {
                    external_id: (*id).to_string(),
                    title: Some((*id).to_string()),
                    kind: "file".into(),
                    tags: vec![],
                    room: None,
                    segments: vec!["hello\n".into()],
                })
                .collect(),
            skipped: vec![],
        }
    }

    #[tokio::test]
    async fn a_source_that_produces_what_the_case_expects_passes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "hello\n").unwrap();
        std::fs::write(dir.path().join("b.md"), "hello\n").unwrap();
        let tree = dir.path().canonicalize().unwrap();

        let failures = run_case(
            &DirectoryAdapter::new(1024),
            &case(&tree, &["a.md", "b.md"]),
            &tree,
        )
        .await;
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[tokio::test]
    async fn a_missing_document_and_a_wrong_field_are_both_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "other\n").unwrap();
        let tree = dir.path().canonicalize().unwrap();

        let failures = run_case(
            &DirectoryAdapter::new(1024),
            &case(&tree, &["a.md", "gone.md"]),
            &tree,
        )
        .await;
        let text = failures.join("\n");
        assert!(text.contains("gone.md"), "{text}");
        assert!(text.contains("segments"), "{text}");
    }

    #[tokio::test]
    async fn an_empty_directory_of_cases_is_an_error_not_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let error = run_all(&DirectoryAdapter::new(1024), dir.path())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("no conformance cases"),
            "{error}"
        );
    }

    // A source that breaks exactly one rule of the contract, by wrapping the real `directory` adapter. The runner is
    // only worth trusting if each rule it claims to check can be seen failing.
    #[derive(Clone, Copy, PartialEq)]
    enum Fault {
        EmptyName,
        MultiLineDescription,
        WrongSource,
        UnstableIdentity,
        IdentifyFails,
        IdentifyFailsTheSecondTime,
        TailFails,
        AcceptsAForeignCursor,
        ForeignCursorIsAnotherError,
        RepeatsCandidates,
        EmptyPageNotExhausted,
        PageOverTheLimit,
        NeverExhausted,
        DiscoverFails,
        TailNotEmpty,
        ReadFails,
        ReadsAnotherDocument,
        UnstableRevision,
        UnstableNormalize,
        NormalizeFails,
    }

    struct Faulty {
        inner: DirectoryAdapter,
        fault: Fault,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Faulty {
        fn new(fault: Fault) -> Self {
            Self {
                inner: DirectoryAdapter::new(1024),
                fault,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn tick(&self) -> usize {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        }

        fn is(&self, fault: Fault) -> bool {
            self.fault == fault
        }
    }

    impl SourceAdapter for Faulty {
        fn name(&self) -> &str {
            if self.is(Fault::EmptyName) {
                ""
            } else {
                self.inner.name()
            }
        }

        fn description(&self) -> &str {
            if self.is(Fault::MultiLineDescription) {
                "one line\nand another"
            } else {
                self.inner.description()
            }
        }

        fn capabilities(&self) -> crate::domain::SourceCapabilities {
            self.inner.capabilities()
        }

        fn identify(&self, locator: Option<&str>) -> Result<SourceRef> {
            if self.is(Fault::IdentifyFails) {
                return Err(Error::invalid_input("locator", "refused"));
            }
            if self.is(Fault::IdentifyFailsTheSecondTime) && self.tick() >= 1 {
                return Err(Error::invalid_input("locator", "refused again"));
            }
            let mut source = self.inner.identify(locator)?;
            if self.is(Fault::WrongSource) {
                source.source = "someone-else".to_string();
            }
            if self.is(Fault::UnstableIdentity) {
                source.account = Some(self.tick().to_string());
            }
            Ok(source)
        }

        fn default_wing(&self, source: &SourceRef) -> String {
            self.inner.default_wing(source)
        }

        fn default_room(&self) -> &str {
            self.inner.default_room()
        }

        async fn discover(
            &self,
            source: &SourceRef,
            cursor: &Cursor,
            limit: usize,
        ) -> Result<crate::mining::adapter::Discovery> {
            let foreign = cursor.is_string();
            let none = crate::mining::adapter::Discovery {
                candidates: vec![],
                exhausted: true,
            };
            match self.fault {
                Fault::AcceptsAForeignCursor if foreign => Ok(none),
                Fault::ForeignCursorIsAnotherError if foreign => {
                    Err(Error::invalid_input("cursor", "no"))
                }
                Fault::DiscoverFails if !foreign => Err(Error::invalid_input("locator", "gone")),
                Fault::TailFails if !foreign && !cursor.is_null() => {
                    Err(Error::invalid_input("locator", "gone after the last page"))
                }
                Fault::EmptyPageNotExhausted if !foreign => Ok(crate::mining::adapter::Discovery {
                    candidates: vec![],
                    exhausted: false,
                }),
                Fault::PageOverTheLimit if !foreign => {
                    self.inner.discover(source, cursor, limit + 5).await
                }
                Fault::RepeatsCandidates if !foreign => {
                    // The same page again whatever the cursor, so a candidate is found twice.
                    let mut page = self.inner.discover(source, &Cursor::Null, limit).await?;
                    page.exhausted = !cursor.is_null();
                    Ok(page)
                }
                Fault::NeverExhausted if !foreign => Ok(crate::mining::adapter::Discovery {
                    candidates: vec![Candidate {
                        external_id: format!("doc-{}", self.tick()),
                        cursor_after: serde_json::json!({ "n": 1 }),
                        handle: String::new(),
                    }],
                    exhausted: false,
                }),
                Fault::TailNotEmpty if !foreign && !cursor.is_null() => {
                    Ok(crate::mining::adapter::Discovery {
                        candidates: vec![Candidate {
                            external_id: "extra".to_string(),
                            cursor_after: Cursor::Null,
                            handle: "extra".to_string(),
                        }],
                        exhausted: false,
                    })
                }
                _ => self.inner.discover(source, cursor, limit).await,
            }
        }

        async fn read(
            &self,
            source: &SourceRef,
            candidate: &Candidate,
        ) -> Result<Option<RawDocument>> {
            if self.is(Fault::ReadFails) {
                return Err(Error::invalid_input("read", "unreadable"));
            }
            let mut raw = self.inner.read(source, candidate).await?;
            if let Some(raw) = raw.as_mut() {
                if self.is(Fault::ReadsAnotherDocument) {
                    raw.external_id = "another".to_string();
                }
                if self.is(Fault::UnstableRevision) {
                    raw.revision = format!("{}-{}", raw.revision, self.tick());
                }
            }
            Ok(raw)
        }

        fn normalize(&self, raw: &RawDocument) -> Result<CanonicalDocument> {
            if self.is(Fault::NormalizeFails) {
                return Err(Error::invalid_input("body", "malformed"));
            }
            let mut canonical = self.inner.normalize(raw)?;
            if self.is(Fault::UnstableNormalize) {
                canonical.title = Some(self.tick().to_string());
            }
            Ok(canonical)
        }
    }

    /// The failures `fault` causes against a tree of `a.md` and `b.md`, paged `limit` at a time.
    async fn failures_of(fault: Fault, limit: usize) -> String {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "hello\n").unwrap();
        std::fs::write(dir.path().join("b.md"), "hello\n").unwrap();
        let tree = dir.path().canonicalize().unwrap();
        let mut case = case(&tree, &["a.md", "b.md"]);
        case.limit = limit;
        run_case(&Faulty::new(fault), &case, &tree).await.join("\n")
    }

    #[tokio::test]
    async fn every_rule_the_runner_checks_is_seen_failing_by_a_source_that_breaks_it() {
        let cases = [
            (Fault::EmptyName, 1, "no name or description"),
            (Fault::MultiLineDescription, 1, "more than one line"),
            (Fault::WrongSource, 1, "returned source `someone-else`"),
            (Fault::UnstableIdentity, 1, "different identity"),
            (Fault::IdentifyFails, 1, "identify("),
            (
                Fault::IdentifyFailsTheSecondTime,
                1,
                "failed the second time",
            ),
            (Fault::TailFails, 10, "after the last cursor failed"),
            (Fault::AcceptsAForeignCursor, 1, "was accepted"),
            (
                Fault::ForeignCursorIsAnotherError,
                1,
                "not a cursor-invalid error",
            ),
            (Fault::RepeatsCandidates, 1, "discovered twice"),
            (Fault::EmptyPageNotExhausted, 1, "cannot make progress"),
            (Fault::PageOverTheLimit, 1, "more than the limit"),
            (Fault::NeverExhausted, 1, "not exhausted after"),
            (Fault::DiscoverFails, 1, "discover failed"),
            (Fault::TailNotEmpty, 10, "after the last cursor found"),
            (Fault::ReadFails, 1, "read(a.md) failed"),
            (
                Fault::ReadsAnotherDocument,
                1,
                "returned a document called `another`",
            ),
            (Fault::UnstableRevision, 1, "twice gave different revisions"),
            (
                Fault::UnstableNormalize,
                1,
                "did not give the same document",
            ),
            (Fault::NormalizeFails, 1, "normalize(a.md) failed"),
        ];
        for (fault, limit, expected) in cases {
            let failures = failures_of(fault, limit).await;
            assert!(
                failures.contains(expected),
                "expected `{expected}` in:\n{failures}"
            );
        }
    }

    #[test]
    fn the_wrapper_delegates_what_it_does_not_break() {
        let faulty = Faulty::new(Fault::EmptyName);
        let source = SourceRef {
            source: "directory".into(),
            account: None,
            locator: "/data/notes".into(),
        };
        assert_eq!(faulty.default_wing(&source), "notes");
        assert_eq!(faulty.default_room(), "files");
    }

    #[tokio::test]
    async fn a_source_that_breaks_no_rule_has_no_failures() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "hello\n").unwrap();
        let tree = dir.path().canonicalize().unwrap();
        let mut case = case(&tree, &["a.md"]);
        case.limit = 10;
        // A tail check on a source that is not incremental is skipped, and one that is must find nothing.
        let failures = run_case(&DirectoryAdapter::new(1024), &case, &tree).await;
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[tokio::test]
    async fn a_document_the_case_says_to_skip_is_a_failure_when_it_is_produced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "hello\n").unwrap();
        let tree = dir.path().canonicalize().unwrap();
        let mut case = case(&tree, &["a.md"]);
        case.skipped = vec!["a.md".to_string()];

        let failures = run_case(&DirectoryAdapter::new(1024), &case, &tree).await;

        assert!(
            failures.join("\n").contains("should have been skipped"),
            "{failures:?}"
        );
    }

    #[tokio::test]
    async fn every_normalized_field_a_case_pins_is_compared() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "hello\n").unwrap();
        let tree = dir.path().canonicalize().unwrap();
        let mut case = case(&tree, &["a.md"]);
        case.limit = 10;
        case.documents[0].title = Some("other".to_string());
        case.documents[0].kind = "transcript".to_string();
        case.documents[0].tags = vec!["x".to_string()];
        case.documents[0].room = Some("elsewhere".to_string());

        let text = run_case(&DirectoryAdapter::new(1024), &case, &tree)
            .await
            .join("\n");

        for field in ["title is", "kind is", "tags are", "room is"] {
            assert!(text.contains(field), "{field} not reported:\n{text}");
        }
    }

    fn write_case(root: &Path, name: &str, json: &str) {
        std::fs::create_dir_all(root.join(name).join("tree")).unwrap();
        std::fs::write(root.join(name).join(CASE_FILE), json).unwrap();
    }

    const MINIMAL: &str =
        r#"{"name":"NAME","description":"d","locator":"tree","limit":1,"documents":[]}"#;

    #[test]
    fn cases_load_in_name_order_and_directories_without_a_case_file_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        write_case(root.path(), "b", &MINIMAL.replace("NAME", "second"));
        write_case(root.path(), "a", &MINIMAL.replace("NAME", "first"));
        std::fs::create_dir(root.path().join("not-a-case")).unwrap();

        let names: Vec<_> = load_cases(root.path())
            .unwrap()
            .into_iter()
            .map(|(case, _)| case.name)
            .collect();

        assert_eq!(names, ["first", "second"]);
    }

    #[test]
    fn a_malformed_case_or_one_whose_tree_is_missing_is_an_error_naming_it() {
        let root = tempfile::tempdir().unwrap();
        write_case(root.path(), "bad", r#"{"name": 1}"#);
        let error = load_cases(root.path()).unwrap_err().to_string();
        assert!(error.contains("case.json"), "{error}");

        let root = tempfile::tempdir().unwrap();
        write_case(root.path(), "gone", &MINIMAL.replace("NAME", "gone"));
        std::fs::remove_dir(root.path().join("gone/tree")).unwrap();
        assert!(load_cases(root.path()).is_err());
        assert!(load_cases(&root.path().join("missing")).is_err());
    }

    #[test]
    fn a_report_passes_only_when_every_case_does() {
        let case = |failures: Vec<String>| CaseReport {
            name: "c".into(),
            failures,
        };
        assert!(
            Report {
                cases: vec![case(vec![])]
            }
            .passed()
        );
        assert!(
            !Report {
                cases: vec![case(vec![]), case(vec!["x".into()])]
            }
            .passed()
        );
    }
}
