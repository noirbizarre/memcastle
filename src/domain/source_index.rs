//! The registry index: the published list of source packages a registry offers (docs/adr/033, docs/adr/039).
//!
//! A registry is one JSON file, `memcastle-index.json`, that anyone can host (a git repository's raw file, static
//! pages, a directory on a share). It names each source, and for each version where its archive is, the SHA-256 of
//! that archive and optionally a signature over it. A source may instead name a GitHub repository, and its versions
//! are then the releases of that repository, read when the index is loaded (`crate::distribution`). This module is only the shape and the rules about it: reading the
//! file, downloading an archive and verifying a signature are `crate::distribution`'s, so the choice of version is a
//! pure function a test can enumerate.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::source_package::{contract_version, is_valid_source_name, version_compatibility};

/// The index format this MemCastle reads. A higher number is refused rather than half-understood.
pub const INDEX_FORMAT: u32 = 1;

/// A registry's index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIndex {
    /// The index format ([`INDEX_FORMAT`]).
    pub format: u32,
    /// What the registry calls itself, for people choosing between several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The sources on offer.
    #[serde(default)]
    pub sources: Vec<IndexedSource>,
}

/// One source in an index, with every version published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedSource {
    /// The source's name, as `memcastle source install <name>` takes it.
    pub name: String,
    /// One line saying what it reads.
    pub description: String,
    /// A page about the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// The SPDX identifier of its licence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// The GitHub repository (`owner/name`) whose releases publish this source, instead of `versions`: the versions
    /// are filled in from them when the index is loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Every published version, in any order.
    #[serde(default)]
    pub versions: Vec<IndexedVersion>,
}

/// One published version of a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedVersion {
    /// The package's semantic version.
    pub version: String,
    /// The contract it was built against (`MAJOR.MINOR`), copied from its manifest so an incompatible version is
    /// skipped without downloading it. Absent when the publisher did not record it (a GitHub release): the manifest
    /// inside the archive is then what decides, once it is downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    /// The MemCastle versions it runs on, copied from its manifest; absent as [`Self::contract`] is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memcastle: Option<String>,
    /// Where the archive is: absolute, or relative to the index's own location.
    pub url: String,
    /// SHA-256 of the archive, in lowercase hex. Always checked before anything is read from the archive.
    pub sha256: String,
    /// The archive's size in bytes, when the publisher recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// A signature over the archive's bytes, when the publisher signs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<IndexSignature>,
    /// Withdrawn by the publisher: never chosen as the latest, but still installable by asking for it explicitly.
    #[serde(default, skip_serializing_if = "is_false")]
    pub yanked: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` hands over a reference.
fn is_false(value: &bool) -> bool {
    !*value
}

/// An ed25519 signature over a package archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexSignature {
    /// The id of the signing key: the first 16 hex characters of the SHA-256 of its public key.
    pub key: String,
    /// The 64-byte signature, base64.
    pub value: String,
}

/// Whether `text` names a GitHub repository the way `owner/name` does: two non-empty segments of the characters GitHub
/// allows, so it can be put in a URL path without escaping.
#[must_use]
pub fn is_repository(text: &str) -> bool {
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    text.split_once('/')
        .is_some_and(|(owner, name)| valid(owner) && valid(name))
}

impl SourceIndex {
    /// An empty index, for a publisher starting one.
    #[must_use]
    pub fn new(name: Option<String>) -> Self {
        Self {
            format: INDEX_FORMAT,
            name,
            sources: Vec::new(),
        }
    }

    /// Parse and validate an index.
    ///
    /// # Errors
    ///
    /// A sentence saying what is wrong, for the error that reports it.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let index: Self =
            serde_json::from_str(text).map_err(|e| format!("it is not an index: {e}"))?;
        index.validate()?;
        Ok(index)
    }

    /// Check every rule a parse cannot.
    ///
    /// # Errors
    ///
    /// A sentence naming the first thing wrong.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.format == 0 || self.format > INDEX_FORMAT {
            return Err(format!(
                "it is index format {}, and this MemCastle reads format {INDEX_FORMAT}; a newer index needs a newer MemCastle",
                self.format
            ));
        }
        let mut names = HashSet::new();
        for source in &self.sources {
            if !is_valid_source_name(&source.name) {
                return Err(format!(
                    "source name `{}` is not a valid source name",
                    source.name
                ));
            }
            if !names.insert(source.name.as_str()) {
                return Err(format!("source `{}` is listed twice", source.name));
            }
            if let Some(repository) = &source.repository {
                if !is_repository(repository) {
                    return Err(format!(
                        "source `{}`: repository `{repository}` is not `owner/name`",
                        source.name
                    ));
                }
                if !source.versions.is_empty() {
                    return Err(format!(
                        "source `{}` names a repository and lists versions; its versions are the repository's releases",
                        source.name
                    ));
                }
            }
            let mut versions = HashSet::new();
            for entry in &source.versions {
                let at = format!("{} {}", source.name, entry.version);
                let version = semver::Version::parse(&entry.version)
                    .map_err(|_| format!("`{at}`: the version is not a semantic version"))?;
                if !versions.insert(version) {
                    return Err(format!("`{at}` is listed twice"));
                }
                if let Some(contract) = &entry.contract {
                    contract_version(contract).map_err(|reason| format!("`{at}`: {reason}"))?;
                }
                if let Some(memcastle) = &entry.memcastle {
                    semver::VersionReq::parse(memcastle).map_err(|_| {
                        format!("`{at}`: memcastle `{memcastle}` is not a version requirement")
                    })?;
                }
                if entry.url.trim().is_empty() {
                    return Err(format!("`{at}` has no url"));
                }
                let hex = entry.sha256.len() == 64
                    && entry
                        .sha256
                        .chars()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
                if !hex {
                    return Err(format!(
                        "`{at}`: sha256 must be 64 lowercase hex characters"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The source called `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&IndexedSource> {
        self.sources.iter().find(|source| source.name == name)
    }

    /// The sources whose name or description contains `query`, ignoring case; every source for an empty query.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<&IndexedSource> {
        let query = query.trim().to_lowercase();
        self.sources
            .iter()
            .filter(|source| {
                query.is_empty()
                    || source.name.to_lowercase().contains(&query)
                    || source.description.to_lowercase().contains(&query)
            })
            .collect()
    }
}

impl IndexedVersion {
    /// Whether this version runs on the MemCastle `running`, and if not, why.
    ///
    /// A version that records neither requirement is assumed to, and the manifest inside the downloaded archive is
    /// checked by the install, which refuses one that cannot run with `SourceIncompatible`.
    ///
    /// # Errors
    ///
    /// A sentence saying why not.
    pub fn compatible_with(&self, running: &semver::Version) -> std::result::Result<(), String> {
        match (&self.contract, &self.memcastle) {
            (Some(contract), Some(memcastle)) => {
                version_compatibility(contract, memcastle, running)
            }
            _ => Ok(()),
        }
    }

    fn semver(&self) -> Option<semver::Version> {
        semver::Version::parse(&self.version).ok()
    }
}

impl IndexedSource {
    /// The version to install: `wanted` when given, else the highest version that is neither yanked nor
    /// incompatible with the MemCastle `running`.
    ///
    /// Choosing here, from the index alone, is what lets an install skip a version that could not run without
    /// downloading it, and say which versions exist when none can.
    ///
    /// # Errors
    ///
    /// A sentence saying why no version qualifies.
    pub fn pick(
        &self,
        wanted: Option<&str>,
        running: &semver::Version,
    ) -> std::result::Result<&IndexedVersion, String> {
        let mut sorted: Vec<&IndexedVersion> = self.versions.iter().collect();
        sorted.sort_by_key(|entry| std::cmp::Reverse(entry.semver()));
        let available = || {
            sorted
                .iter()
                .map(|entry| entry.version.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };

        if let Some(wanted) = wanted {
            let wanted_version = semver::Version::parse(wanted)
                .map_err(|_| format!("`{wanted}` is not a semantic version like `1.2.0`"))?;
            let entry = sorted
                .iter()
                .find(|entry| entry.semver().as_ref() == Some(&wanted_version))
                .ok_or_else(|| {
                    format!(
                        "it has no version {wanted}; available: {}",
                        if sorted.is_empty() {
                            "none".to_string()
                        } else {
                            available()
                        }
                    )
                })?;
            entry
                .compatible_with(running)
                .map_err(|reason| format!("version {wanted}: {reason}"))?;
            return Ok(entry);
        }

        let mut newest_refusal = None;
        for entry in sorted.iter().filter(|entry| !entry.yanked) {
            match entry.compatible_with(running) {
                Ok(()) => return Ok(entry),
                Err(reason) => {
                    newest_refusal.get_or_insert_with(|| format!("{}: {reason}", entry.version));
                }
            }
        }
        Err(match newest_refusal {
            Some(refusal) => format!(
                "no version runs on this MemCastle (the newest, {refusal}); available: {}",
                available()
            ),
            None if sorted.is_empty() => "it lists no versions".to_string(),
            None => format!("every version is yanked; available: {}", available()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(number: &str, memcastle: &str) -> IndexedVersion {
        IndexedVersion {
            version: number.to_string(),
            contract: Some(crate::domain::CONTRACT_VERSION.to_string()),
            memcastle: Some(memcastle.to_string()),
            url: format!("demo-{number}.tar.gz"),
            sha256: "a".repeat(64),
            size: None,
            signature: None,
            yanked: false,
        }
    }

    fn source(versions: Vec<IndexedVersion>) -> IndexedSource {
        IndexedSource {
            name: "demo".into(),
            description: "Reads demo documents".into(),
            homepage: None,
            license: None,
            repository: None,
            versions,
        }
    }

    fn running() -> semver::Version {
        semver::Version::new(0, 3, 1)
    }

    #[test]
    fn the_latest_version_is_the_highest_semantic_one_not_the_last_listed() {
        let source = source(vec![
            version("1.10.0", ">=0.1"),
            version("1.9.0", ">=0.1"),
            version("1.2.0", ">=0.1"),
        ]);
        assert_eq!(source.pick(None, &running()).unwrap().version, "1.10.0");
    }

    #[test]
    fn a_yanked_version_is_never_the_latest_but_can_be_asked_for() {
        let mut newest = version("2.0.0", ">=0.1");
        newest.yanked = true;
        let source = source(vec![newest, version("1.0.0", ">=0.1")]);
        assert_eq!(source.pick(None, &running()).unwrap().version, "1.0.0");
        assert_eq!(
            source.pick(Some("2.0.0"), &running()).unwrap().version,
            "2.0.0"
        );
    }

    #[test]
    fn an_incompatible_newest_version_is_skipped_for_the_newest_that_runs() {
        let source = source(vec![version("2.0.0", ">=9"), version("1.0.0", ">=0.1")]);
        assert_eq!(source.pick(None, &running()).unwrap().version, "1.0.0");
    }

    #[test]
    fn when_no_version_runs_the_refusal_names_why_and_what_exists() {
        let source = source(vec![version("2.0.0", ">=9"), version("1.0.0", ">=8")]);
        let reason = source.pick(None, &running()).unwrap_err();
        assert!(
            reason.contains("2.0.0") && reason.contains(">=9") && reason.contains("1.0.0"),
            "{reason}"
        );
    }

    #[test]
    fn asking_for_an_incompatible_or_unknown_version_says_so() {
        let source = source(vec![version("2.0.0", ">=9")]);
        let incompatible = source.pick(Some("2.0.0"), &running()).unwrap_err();
        assert!(incompatible.contains(">=9"), "{incompatible}");
        let unknown = source.pick(Some("3.0.0"), &running()).unwrap_err();
        assert!(unknown.contains("available: 2.0.0"), "{unknown}");
        assert!(source.pick(Some("latest"), &running()).is_err());
    }

    #[test]
    fn a_source_whose_every_version_is_yanked_or_absent_has_nothing_to_install() {
        let mut only = version("1.0.0", ">=0.1");
        only.yanked = true;
        assert!(
            source(vec![only])
                .pick(None, &running())
                .unwrap_err()
                .contains("yanked")
        );
        assert!(
            source(vec![])
                .pick(None, &running())
                .unwrap_err()
                .contains("no versions")
        );
    }

    fn index_json(edit: impl Fn(&mut serde_json::Value)) -> String {
        let mut value = serde_json::json!({
            "format": 1,
            "name": "test",
            "sources": [{
                "name": "demo",
                "description": "Reads demo documents",
                "versions": [{
                    "version": "1.0.0",
                    "contract": crate::domain::CONTRACT_VERSION,
                    "memcastle": ">=0.1",
                    "url": "demo-1.0.0.tar.gz",
                    "sha256": "a".repeat(64),
                }],
            }],
        });
        edit(&mut value);
        value.to_string()
    }

    #[test]
    fn a_well_formed_index_parses_and_is_searchable_by_name_or_description() {
        let index = SourceIndex::parse(&index_json(|_| {})).unwrap();
        assert!(index.find("demo").is_some() && index.find("other").is_none());
        assert_eq!(index.search("DEMO").len(), 1);
        assert_eq!(index.search("documents").len(), 1);
        assert_eq!(index.search("").len(), 1);
        assert!(index.search("nothing").is_empty());
    }

    #[test]
    fn a_source_naming_a_repository_lists_no_versions_of_its_own_and_the_name_must_be_owner_slash_name()
     {
        let repository = |value: &str| {
            index_json(|v| {
                v["sources"][0]["versions"] = serde_json::json!([]);
                v["sources"][0]["repository"] = value.into();
            })
        };
        assert!(SourceIndex::parse(&repository("noirbizarre/memcastle")).is_ok());
        for bad in [
            "memcastle",
            "a/b/c",
            "/b",
            "a/",
            "../x",
            "a/..",
            "a b/c",
            "a/b?x=1",
        ] {
            let error = SourceIndex::parse(&repository(bad)).unwrap_err();
            assert!(error.contains("owner/name"), "{bad}: {error}");
        }
        let both = index_json(|v| v["sources"][0]["repository"] = "o/r".into());
        let error = SourceIndex::parse(&both).unwrap_err();
        assert!(error.contains("repository's releases"), "{error}");
    }

    #[test]
    fn a_version_that_records_no_compatibility_is_taken_to_run_and_the_install_decides() {
        let mut unknown = version("1.0.0", ">=0.1");
        unknown.contract = None;
        unknown.memcastle = None;
        let source = source(vec![unknown]);
        assert_eq!(source.pick(None, &running()).unwrap().version, "1.0.0");
        // A requirement the index does record is still held to.
        let incompatible = IndexedSource {
            versions: vec![version("1.0.0", ">=9")],
            ..source
        };
        assert!(incompatible.pick(None, &running()).is_err());
    }

    #[test]
    fn every_rule_about_an_index_names_what_it_refuses() {
        type Edit = Box<dyn Fn(&mut serde_json::Value)>;
        let cases: Vec<(Edit, &str)> = vec![
            (Box::new(|v| v["format"] = 2.into()), "format 2"),
            (Box::new(|v| v["format"] = 0.into()), "format 0"),
            (
                Box::new(|v| v["sources"][0]["name"] = "../x".into()),
                "valid source name",
            ),
            (
                Box::new(|v| v["sources"][0]["versions"][0]["version"] = "one".into()),
                "semantic version",
            ),
            (
                Box::new(|v| v["sources"][0]["versions"][0]["sha256"] = "ABC".into()),
                "sha256",
            ),
            (
                Box::new(|v| v["sources"][0]["versions"][0]["memcastle"] = "lots".into()),
                "requirement",
            ),
            (
                Box::new(|v| v["sources"][0]["versions"][0]["contract"] = "x".into()),
                "contract",
            ),
            (
                Box::new(|v| v["sources"][0]["versions"][0]["url"] = " ".into()),
                "no url",
            ),
            (Box::new(|v| v["extra"] = 1.into()), "not an index"),
            (
                Box::new(|v| {
                    let again = v["sources"][0].clone();
                    v["sources"].as_array_mut().unwrap().push(again);
                }),
                "listed twice",
            ),
            (
                Box::new(|v| {
                    let again = v["sources"][0]["versions"][0].clone();
                    v["sources"][0]["versions"]
                        .as_array_mut()
                        .unwrap()
                        .push(again);
                }),
                "listed twice",
            ),
        ];
        for (edit, expected) in cases {
            let error = SourceIndex::parse(&index_json(edit)).unwrap_err();
            assert!(
                error.contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }
    }
}
