//! Finding, fetching and verifying source packages from registries (docs/adr/033, docs/adr/039).
//!
//! `crate::source` is what a package *is* and how to make one; `crate::mining::wasm` is how one *runs*; this module is
//! how one *arrives*: reading an index, choosing a version, downloading the archive, and proving it is the archive the
//! index named and that the user's trust policy allows. It returns bytes and says where they came from, and decides
//! nothing about installing them: that is `crate::app`, which is also the only caller, so nothing here touches the
//! store or the jobs (AGENTS.md, invariants 9 and 10).
//!
//! Only a source that is neither built in nor bundled ever arrives: a built-in one is compiled in, and a bundled one is
//! installed from the start, unpacked beside the binary (`crate::mining::bundled`), so neither is fetched from anywhere
//! and neither is held to the trust policy. What comes from here is a package from an index the user configured, and
//! every such package is held to the same checks.
//!
//! An index entry may name a GitHub repository instead of listing versions (`github`): the official registry is such
//! an index, a static file that only says which repositories publish sources, so adding one is a pull request and not
//! a release. Reading a repository's releases is the only request this module makes besides an index and an archive,
//! and goes through the same `Location`, so it is held to the same rules about schemes and redirects.

mod github;
mod location;
mod trust;

use crate::config::MiningConfig;
use crate::domain::{IndexedSource, IndexedVersion, SourceIndex, sha256_hex};
use crate::error::{Error, Result};
use crate::source::package::SourcePackage;

pub use location::Location;
pub use trust::TrustPolicy;

/// The index's file name, in a registry's directory.
pub const INDEX_FILE: &str = "memcastle-index.json";

/// The largest index read: a list of names and digests, not a payload.
const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// The most a repository's release listing may be: a hundred releases with their assets, owners and reactions.
const MAX_RELEASES_BYTES: usize = 8 * 1024 * 1024;

/// The largest archive downloaded: the same ceiling as an upload to the daemon, so what can be fetched can be
/// installed.
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// An archive that was fetched and verified.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The archive's bytes.
    pub archive: Vec<u8>,
    /// Its SHA-256, which equals the one the index published.
    pub archive_digest: String,
    /// The id of the trusted key that signed it, when one did.
    pub signed_by: Option<String>,
}

/// One index and where it came from.
#[derive(Debug, Clone)]
pub struct Registry {
    /// The location as configured, which is what a record of an install keeps to find it again.
    pub label: String,
    /// The index itself, with the versions of the sources that name a repository filled in.
    pub index: SourceIndex,
    /// One line for each repository whose releases could not be read, or whose files were passed over: the rest of the
    /// index still answers.
    pub warnings: Vec<String>,
    base: Location,
}

fn unavailable(location: &str, message: impl Into<String>) -> Error {
    Error::SourceRegistryUnavailable {
        location: location.to_string(),
        message: message.into(),
    }
}

impl Registry {
    /// Read the index at `label`, and the releases of every repository it names through the GitHub API at `github_api`.
    ///
    /// A repository that cannot be read is a warning and its sources are left out, so one repository being down, or the
    /// API's rate limit being spent, does not hide the sources that came from elsewhere.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when it cannot be read or is not an index this MemCastle understands.
    pub async fn load(label: &str, github_api: &str) -> Result<Self> {
        let base = Location::parse(label)
            .map_err(|reason| unavailable(label, reason))?
            .as_index(INDEX_FILE);
        let bytes = base
            .read(MAX_INDEX_BYTES)
            .await
            .map_err(|reason| unavailable(label, reason))?;
        let text =
            String::from_utf8(bytes).map_err(|_| unavailable(label, "it is not UTF-8 text"))?;
        let mut index = SourceIndex::parse(&text).map_err(|reason| unavailable(label, reason))?;
        let warnings = resolve_repositories(&mut index, github_api).await;
        Ok(Self {
            label: label.to_string(),
            index,
            warnings,
            base,
        })
    }

    /// Download the archive of `entry` (a version of `name`) and verify it.
    ///
    /// The SHA-256 is always checked, and so is the signature, as `policy` says.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when the archive cannot be fetched, [`Error::SourceIntegrity`] when it is
    /// not the archive the index named, and [`Error::SourceUntrusted`] when the policy refuses it.
    pub async fn fetch(
        &self,
        name: &str,
        entry: &IndexedVersion,
        policy: &TrustPolicy,
    ) -> Result<Fetched> {
        let source = self
            .base
            .join(&entry.url)
            .map_err(|reason| unavailable(&self.label, reason))?;
        let archive = source
            .read(MAX_ARCHIVE_BYTES)
            .await
            .map_err(|reason| unavailable(&source.to_string(), reason))?;
        let archive_digest = sha256_hex(&archive);
        if archive_digest != entry.sha256 {
            return Err(Error::SourceIntegrity {
                name: name.to_string(),
                message: format!(
                    "{source} has SHA-256 {archive_digest}, and the index published {}",
                    entry.sha256
                ),
            });
        }
        let signed_by = policy.check(name, &archive, entry.signature.as_ref())?;
        Ok(Fetched {
            archive,
            archive_digest,
            signed_by,
        })
    }
}

/// Fill in the versions of every source that names a repository, from that repository's releases.
///
/// One request for each distinct repository, however many sources it publishes. A source whose repository cannot be
/// read is removed from the index, since listing it with no versions would only say "it lists no versions".
async fn resolve_repositories(index: &mut SourceIndex, github_api: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut releases: std::collections::HashMap<String, Option<Vec<github::Release>>> =
        std::collections::HashMap::new();
    for repository in index
        .sources
        .iter()
        .filter_map(|source| source.repository.clone())
    {
        if releases.contains_key(&repository) {
            continue;
        }
        let listing = read_releases(github_api, &repository).await;
        releases.insert(
            repository.clone(),
            match listing {
                Ok(list) => Some(list),
                Err(reason) => {
                    warnings.push(format!("{repository}: {reason}"));
                    None
                }
            },
        );
    }
    index.sources.retain_mut(|source| {
        let Some(repository) = source.repository.as_deref() else {
            return true;
        };
        let Some(Some(list)) = releases.get(repository) else {
            return false;
        };
        let (versions, skipped) = github::versions_from_releases(&source.name, list);
        warnings.extend(
            skipped
                .into_iter()
                .map(|line| format!("{repository}: {}: {line}", source.name)),
        );
        source.versions = versions;
        true
    });
    warnings
}

async fn read_releases(
    github_api: &str,
    repository: &str,
) -> std::result::Result<Vec<github::Release>, String> {
    let url = github::releases_url(github_api, repository);
    let location = Location::parse(&url)?;
    let bytes = location
        .read(MAX_RELEASES_BYTES)
        .await
        .map_err(|reason| format!("its releases cannot be read: {reason}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("{url} did not answer with a list of releases: {e}"))
}

/// The package inside an archive must be the one the index said it was, or an index could publish a harmless
/// package's digest under a name that installs something else.
///
/// # Errors
///
/// [`Error::SourceIntegrity`] naming what differs.
pub fn verify_identity(package: &SourcePackage, name: &str, entry: &IndexedVersion) -> Result<()> {
    let manifest = &package.manifest.source;
    if manifest.name != name || manifest.version != entry.version {
        return Err(Error::SourceIntegrity {
            name: name.to_string(),
            message: format!(
                "the index lists it as {name} {}, and the package says it is {} {}",
                entry.version, manifest.name, manifest.version
            ),
        });
    }
    Ok(())
}

/// Every index the daemon consults, in the order they are configured.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    /// The indexes that could be read.
    pub registries: Vec<Registry>,
    /// One line for each index that could not be read: a registry that is down must not hide the others.
    pub warnings: Vec<String>,
}

impl Catalog {
    /// Read the configured registries, or only `only` when it is given.
    ///
    /// `only` is an explicit choice (`--registry`), so it replaces the lot rather than adding to it, and a failure to
    /// read it is an error and not a warning.
    ///
    /// # Errors
    ///
    /// [`Error::SourceRegistryUnavailable`] when `only` cannot be read.
    pub async fn open(mining: &MiningConfig, only: Option<&str>) -> Result<Self> {
        if let Some(location) = only {
            // The one place a location arrives from a request rather than a configuration file: it is checked
            // against the same scheme rules, and what it serves is held to the same trust policy.
            let registry = Registry::load(location, &mining.github_api_url).await?;
            return Ok(Self {
                warnings: registry.warnings.clone(),
                registries: vec![registry],
            });
        }
        let mut catalog = Self::default();
        for location in &mining.registries {
            match Registry::load(location, &mining.github_api_url).await {
                Ok(registry) => catalog.registries.push(registry),
                Err(error) => catalog.warnings.push(error.to_string()),
            }
        }
        // A repository that could not be read is as much a thing the user should hear about as a registry that is down.
        let from_repositories: Vec<String> = catalog
            .registries
            .iter()
            .flat_map(|registry| registry.warnings.iter().cloned())
            .collect();
        catalog.warnings.extend(from_repositories);
        Ok(catalog)
    }

    /// The first registry that offers `name`, which is where an install of it comes from.
    #[must_use]
    pub fn locate(&self, name: &str) -> Option<(&Registry, &IndexedSource)> {
        self.registries
            .iter()
            .find_map(|registry| registry.index.find(name).map(|source| (registry, source)))
    }

    /// The registry installed from `label`, for an update to ask again.
    #[must_use]
    pub fn by_label(&self, label: &str) -> Option<&Registry> {
        self.registries
            .iter()
            .find(|registry| registry.label == label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CONTRACT_VERSION, IndexedSource};

    fn entry(archive: &[u8]) -> IndexedVersion {
        IndexedVersion {
            version: "1.0.0".into(),
            contract: Some(CONTRACT_VERSION.into()),
            memcastle: Some(">=0.1".into()),
            url: "demo-1.0.0.tar.gz".into(),
            sha256: sha256_hex(archive),
            size: None,
            signature: None,
            yanked: false,
        }
    }

    fn write_registry(directory: &std::path::Path, archive: &[u8], published: &IndexedVersion) {
        std::fs::write(directory.join("demo-1.0.0.tar.gz"), archive).unwrap();
        let mut index = SourceIndex::new(Some("test".into()));
        index.sources.push(IndexedSource {
            name: "demo".into(),
            description: "demo".into(),
            homepage: None,
            license: None,
            repository: None,
            versions: vec![published.clone()],
        });
        std::fs::write(
            directory.join(INDEX_FILE),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_archive_matching_the_index_is_fetched_from_a_directory_registry() {
        let directory = tempfile::tempdir().unwrap();
        let published = entry(b"archive");
        write_registry(directory.path(), b"archive", &published);

        let registry = Registry::load(directory.path().to_str().unwrap(), "https://api.github.com")
            .await
            .unwrap();
        let (found, source) = (
            registry.index.find("demo").is_some(),
            registry.index.find("demo").unwrap(),
        );
        assert!(found);
        let policy = TrustPolicy::from_config(&MiningConfig::default()).unwrap();
        let fetched = registry
            .fetch("demo", &source.versions[0], &policy)
            .await
            .unwrap();
        assert_eq!(fetched.archive, b"archive");
        assert_eq!(fetched.archive_digest, published.sha256);
        assert_eq!(fetched.signed_by, None);
    }

    #[tokio::test]
    async fn an_archive_that_is_not_the_one_the_index_named_is_an_integrity_failure() {
        let directory = tempfile::tempdir().unwrap();
        let published = entry(b"archive");
        // The registry serves different bytes than it published.
        write_registry(directory.path(), b"substituted", &published);

        let registry = Registry::load(directory.path().to_str().unwrap(), "https://api.github.com")
            .await
            .unwrap();
        let policy = TrustPolicy::from_config(&MiningConfig::default()).unwrap();
        let error = registry
            .fetch("demo", &published, &policy)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::SourceIntegrity { .. }), "{error}");
    }

    #[tokio::test]
    async fn an_unreadable_or_malformed_index_names_its_location() {
        let directory = tempfile::tempdir().unwrap();
        let missing = Registry::load(directory.path().to_str().unwrap(), "https://api.github.com")
            .await
            .unwrap_err();
        assert!(
            matches!(missing, Error::SourceRegistryUnavailable { .. }),
            "{missing}"
        );

        std::fs::write(directory.path().join(INDEX_FILE), "{ not json").unwrap();
        let malformed =
            Registry::load(directory.path().to_str().unwrap(), "https://api.github.com")
                .await
                .unwrap_err();
        assert!(
            malformed
                .to_string()
                .contains(directory.path().to_str().unwrap()),
            "{malformed}"
        );
    }

    #[tokio::test]
    async fn one_registry_being_down_does_not_hide_the_others() {
        let directory = tempfile::tempdir().unwrap();
        write_registry(directory.path(), b"archive", &entry(b"archive"));
        let mining = MiningConfig {
            registries: vec![
                directory.path().join("gone").to_str().unwrap().to_string(),
                directory.path().to_str().unwrap().to_string(),
            ],
            ..MiningConfig::default()
        };

        let catalog = Catalog::open(&mining, None).await.unwrap();
        assert_eq!(catalog.registries.len(), 1);
        assert_eq!(catalog.warnings.len(), 1);
        assert!(catalog.locate("demo").is_some() && catalog.locate("other").is_none());
    }

    #[tokio::test]
    async fn an_explicitly_chosen_registry_replaces_the_rest_and_must_be_readable() {
        let directory = tempfile::tempdir().unwrap();
        write_registry(directory.path(), b"archive", &entry(b"archive"));
        // The configured registry is readable; the one asked for is not, and replaces it.
        let mining = MiningConfig {
            registries: vec![directory.path().to_str().unwrap().to_string()],
            ..MiningConfig::default()
        };
        let nowhere = directory.path().join("nowhere");
        let error = Catalog::open(&mining, Some(nowhere.to_str().unwrap()))
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::SourceRegistryUnavailable { .. }),
            "{error}"
        );
    }

    /// A server on this machine that answers each path with a fixed body and everything else with 404, standing in for
    /// the GitHub API and for the release downloads.
    ///
    /// `routes` is given the server's own address, because a release lists where its files download from.
    async fn serve(routes: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = routes(&base);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request).to_string();
                    let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (status, body) = routes
                        .iter()
                        .find(|(route, _)| *route == path)
                        .map_or(("404 Not Found", Vec::new()), |(_, body)| {
                            ("200 OK", body.clone())
                        });
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        base
    }

    /// A GitHub stand-in: `owner/name` has one release attaching `demo-1.0.0.tar.gz`, which GitHub says is `listed`
    /// and which downloads as `served`.
    async fn github_with_release(listed: &'static [u8], served: &'static [u8]) -> String {
        serve(|base| {
            let releases = serde_json::json!([{
                "tag_name": "v1",
                "draft": false,
                "prerelease": false,
                "assets": [{
                    "name": "demo-1.0.0.tar.gz",
                    "state": "uploaded",
                    "size": listed.len(),
                    "digest": format!("sha256:{}", sha256_hex(listed)),
                    "browser_download_url": format!("{base}/download/demo-1.0.0.tar.gz"),
                }],
            }]);
            vec![
                (
                    "/repos/owner/name/releases?per_page=100".to_string(),
                    releases.to_string().into_bytes(),
                ),
                ("/download/demo-1.0.0.tar.gz".to_string(), served.to_vec()),
            ]
        })
        .await
    }

    /// A registry directory with one source that names `repository`, and one that lists its own versions.
    fn write_registry_naming(directory: &std::path::Path, repository: &str) {
        let published = entry(b"archive");
        write_registry(directory, b"archive", &published);
        let mut index: SourceIndex =
            serde_json::from_slice(&std::fs::read(directory.join(INDEX_FILE)).unwrap()).unwrap();
        index.sources.push(IndexedSource {
            name: "demo".into(),
            description: "demo".into(),
            homepage: None,
            license: None,
            repository: Some(repository.into()),
            versions: Vec::new(),
        });
        index.sources.retain(|source| source.repository.is_some());
        std::fs::write(
            directory.join(INDEX_FILE),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_source_naming_a_repository_gets_its_releases_as_versions_and_the_archive_is_held_to_githubs_digest()
     {
        let github = github_with_release(b"archive", b"archive").await;
        let directory = tempfile::tempdir().unwrap();
        write_registry_naming(directory.path(), "owner/name");

        let registry = Registry::load(directory.path().to_str().unwrap(), &github)
            .await
            .unwrap();
        assert!(registry.warnings.is_empty(), "{:?}", registry.warnings);
        let entry = registry
            .index
            .find("demo")
            .unwrap()
            .pick(None, &semver::Version::new(0, 3, 1))
            .unwrap()
            .clone();
        assert_eq!(entry.version, "1.0.0");
        let policy = TrustPolicy::from_config(&MiningConfig::default()).unwrap();
        let fetched = registry.fetch("demo", &entry, &policy).await.unwrap();
        assert_eq!(fetched.archive, b"archive");
        assert_eq!(fetched.signed_by, None);
    }

    #[tokio::test]
    async fn a_download_that_is_not_what_github_computed_the_digest_of_is_an_integrity_failure() {
        let github = github_with_release(b"archive", b"substituted").await;
        let directory = tempfile::tempdir().unwrap();
        write_registry_naming(directory.path(), "owner/name");

        let registry = Registry::load(directory.path().to_str().unwrap(), &github)
            .await
            .unwrap();
        let entry = registry.index.find("demo").unwrap().versions[0].clone();
        let policy = TrustPolicy::from_config(&MiningConfig::default()).unwrap();
        let error = registry.fetch("demo", &entry, &policy).await.unwrap_err();
        assert!(matches!(error, Error::SourceIntegrity { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_repository_that_cannot_be_read_is_a_warning_and_leaves_the_rest_of_the_registry_working()
     {
        let github = github_with_release(b"archive", b"archive").await;
        let directory = tempfile::tempdir().unwrap();
        write_registry_naming(directory.path(), "owner/name");
        // A second source, in a repository the API answers 404 for, and one that lists itself.
        let mut index: SourceIndex =
            serde_json::from_slice(&std::fs::read(directory.path().join(INDEX_FILE)).unwrap())
                .unwrap();
        index.sources.push(IndexedSource {
            name: "gone".into(),
            description: "gone".into(),
            homepage: None,
            license: None,
            repository: Some("nobody/nothing".into()),
            versions: Vec::new(),
        });
        index.sources.push(IndexedSource {
            name: "local".into(),
            description: "local".into(),
            homepage: None,
            license: None,
            repository: None,
            versions: vec![entry(b"archive")],
        });
        std::fs::write(
            directory.path().join(INDEX_FILE),
            serde_json::to_string(&index).unwrap(),
        )
        .unwrap();

        let registry = Registry::load(directory.path().to_str().unwrap(), &github)
            .await
            .unwrap();
        assert!(
            registry.index.find("gone").is_none(),
            "{:?}",
            registry.index
        );
        assert!(registry.index.find("demo").is_some() && registry.index.find("local").is_some());
        assert_eq!(registry.warnings.len(), 1, "{:?}", registry.warnings);
        assert!(
            registry.warnings[0].contains("nobody/nothing"),
            "{:?}",
            registry.warnings
        );

        let mining = MiningConfig {
            registries: vec![directory.path().to_str().unwrap().to_string()],
            github_api_url: github,
            ..MiningConfig::default()
        };
        let catalog = Catalog::open(&mining, None).await.unwrap();
        assert_eq!(
            catalog.warnings.len(),
            1,
            "the registry's warning reaches the catalog's"
        );
    }

    #[tokio::test]
    async fn one_request_is_made_for_each_repository_however_many_sources_it_publishes() {
        // A server that counts: the second source in the same repository must not ask again.
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut stream, _)) = listener.accept().await {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]",
                    )
                    .await;
            }
        });
        let mut index = SourceIndex::new(None);
        for name in ["one", "two"] {
            index.sources.push(IndexedSource {
                name: name.into(),
                description: name.into(),
                homepage: None,
                license: None,
                repository: Some("owner/name".into()),
                versions: Vec::new(),
            });
        }

        let warnings = resolve_repositories(&mut index, &base).await;

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(index.sources.len(), 2);
    }
}
