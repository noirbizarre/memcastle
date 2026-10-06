//! Source packages published as GitHub releases (docs/adr/040).
//!
//! An index entry that names a repository carries no versions: they are the repository's releases, read when the index
//! is loaded. The convention is the one `memcastle source package` already follows: a release attaches
//! `<name>-<version>.tar.gz`, and the version is read from that file name. What makes the archive trustworthy is the
//! SHA-256 GitHub computed when it was uploaded and reports as the asset's `digest`, which `Registry::fetch` then checks
//! against the bytes it downloads, exactly as it does for an index that lists a digest itself.
//!
//! This is *data* from a third party, so everything here is defensive: a release that does not follow the convention is
//! skipped, never an error, and a repository that cannot be read is a warning that leaves the rest of the registry
//! working.

use serde::Deserialize;

use crate::domain::IndexedVersion;

/// One release of a repository, as `GET /repos/{owner}/{repo}/releases` answers. Only what is read is declared, so
/// GitHub adding a field never breaks a registry.
#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    /// The release's tag, used in warnings.
    #[serde(default)]
    pub tag_name: String,
    /// A release nobody has published yet.
    #[serde(default)]
    pub draft: bool,
    /// A release its author marked as not ready for everyone.
    #[serde(default)]
    pub prerelease: bool,
    /// The files attached to it.
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

/// A file attached to a release.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    /// The file name.
    pub name: String,
    /// Where it downloads from.
    #[serde(default)]
    pub browser_download_url: String,
    /// `uploaded` once the upload finished.
    #[serde(default)]
    pub state: String,
    /// The size in bytes.
    #[serde(default)]
    pub size: Option<u64>,
    /// `sha256:<hex>`, computed by GitHub; `null` for a file uploaded before GitHub did.
    #[serde(default)]
    pub digest: Option<String>,
}

/// The versions of the source `name` that `releases` publish, and what was passed over and why.
///
/// `releases` are newest first, as GitHub lists them, so when a version appears in more than one release the newest
/// wins: a release that only re-attaches an unchanged package must not change what a published version means.
#[must_use]
pub fn versions_from_releases(
    name: &str,
    releases: &[Release],
) -> (Vec<IndexedVersion>, Vec<String>) {
    let mut versions: Vec<IndexedVersion> = Vec::new();
    let mut skipped = Vec::new();
    for release in releases.iter().filter(|r| !r.draft && !r.prerelease) {
        for asset in &release.assets {
            let Some(version) = asset
                .name
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(|rest| rest.strip_suffix(".tar.gz"))
                .filter(|version| semver::Version::parse(version).is_ok())
            else {
                continue;
            };
            if versions.iter().any(|known| known.version == version) {
                continue;
            }
            match usable(asset) {
                Ok(sha256) => versions.push(IndexedVersion {
                    version: version.to_string(),
                    // Not known before the archive is read: the install checks the manifest inside it.
                    contract: None,
                    memcastle: None,
                    url: asset.browser_download_url.clone(),
                    sha256,
                    size: asset.size,
                    signature: None,
                    yanked: false,
                }),
                Err(reason) => skipped.push(format!(
                    "{} of release {}: {reason}",
                    asset.name, release.tag_name
                )),
            }
        }
    }
    (versions, skipped)
}

/// The SHA-256 of an asset that can be installed, or why it cannot.
fn usable(asset: &ReleaseAsset) -> std::result::Result<String, String> {
    if asset.state != "uploaded" {
        return Err(format!("its upload is `{}`, not finished", asset.state));
    }
    // The same rule a registry location is held to: `https`, or plain `http` to this machine.
    super::Location::parse(&asset.browser_download_url)
        .map_err(|reason| format!("its download URL is not usable: {reason}"))?;
    let Some(digest) = asset.digest.as_deref() else {
        return Err(
            "GitHub reports no digest for it (it was uploaded before GitHub computed them); upload it again"
                .to_string(),
        );
    };
    match digest.strip_prefix("sha256:") {
        Some(hex)
            if hex.len() == 64
                && hex
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) =>
        {
            Ok(hex.to_string())
        }
        _ => Err(format!(
            "its digest `{digest}` is not `sha256:` and 64 lowercase hex characters"
        )),
    }
}

/// The URL that lists a repository's releases under the API at `api`.
#[must_use]
pub fn releases_url(api: &str, repository: &str) -> String {
    format!(
        "{}/repos/{repository}/releases?per_page=100",
        api.trim_end_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, digest: Option<&str>) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            browser_download_url: format!("https://github.com/o/r/releases/download/t/{name}"),
            state: "uploaded".to_string(),
            size: Some(10),
            digest: digest.map(str::to_string),
        }
    }

    fn release(tag: &str, assets: Vec<ReleaseAsset>) -> Release {
        Release {
            tag_name: tag.to_string(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    fn digest(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    #[test]
    fn a_release_that_attaches_the_package_publishes_its_version_with_the_digest_github_computed() {
        let releases = vec![release(
            "v1",
            vec![asset("pi-0.2.0.tar.gz", Some(&digest('a')))],
        )];
        let (versions, skipped) = versions_from_releases("pi", &releases);

        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, "0.2.0");
        assert_eq!(versions[0].sha256, "a".repeat(64));
        assert_eq!(versions[0].size, Some(10));
        assert!(versions[0].url.ends_with("/pi-0.2.0.tar.gz"));
        assert!(versions[0].contract.is_none() && versions[0].signature.is_none());
    }

    #[test]
    fn drafts_prereleases_and_other_files_publish_nothing() {
        let mut draft = release("d", vec![asset("pi-1.0.0.tar.gz", Some(&digest('a')))]);
        draft.draft = true;
        let mut pre = release("p", vec![asset("pi-1.1.0.tar.gz", Some(&digest('a')))]);
        pre.prerelease = true;
        let other = release(
            "o",
            vec![
                asset("pi-sessions-1.2.0.tar.gz", Some(&digest('a'))),
                asset("opencode-1.2.0.tar.gz", Some(&digest('a'))),
                asset("pi-1.2.0.tar.gz.sha256", Some(&digest('a'))),
                asset("pi-latest.tar.gz", Some(&digest('a'))),
                asset("memcastle_v1_linux-amd64", Some(&digest('a'))),
            ],
        );
        let (versions, skipped) = versions_from_releases("pi", &[draft, pre, other]);

        assert!(versions.is_empty(), "{versions:?}");
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    #[test]
    fn the_newest_release_decides_a_version_that_several_releases_attach() {
        let newest = release("v2", vec![asset("pi-0.1.0.tar.gz", Some(&digest('b')))]);
        let oldest = release("v1", vec![asset("pi-0.1.0.tar.gz", Some(&digest('a')))]);
        let (versions, _) = versions_from_releases("pi", &[newest, oldest]);

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].sha256, "b".repeat(64));
    }

    #[test]
    fn an_asset_without_a_usable_digest_or_download_is_skipped_and_says_why() {
        let mut unfinished = asset("pi-0.1.0.tar.gz", Some(&digest('a')));
        unfinished.state = "open".to_string();
        let mut insecure = asset("pi-0.2.0.tar.gz", Some(&digest('a')));
        insecure.browser_download_url = "http://example.test/pi-0.2.0.tar.gz".to_string();
        let releases = vec![release(
            "v1",
            vec![
                unfinished,
                insecure,
                asset("pi-0.3.0.tar.gz", None),
                asset("pi-0.4.0.tar.gz", Some("md5:abc")),
                asset(
                    "pi-0.5.0.tar.gz",
                    Some(&format!("sha256:{}", "A".repeat(64))),
                ),
            ],
        )];
        let (versions, skipped) = versions_from_releases("pi", &releases);

        assert!(versions.is_empty(), "{versions:?}");
        assert_eq!(skipped.len(), 5, "{skipped:?}");
        assert!(skipped[0].contains("not finished"), "{skipped:?}");
        assert!(skipped[1].contains("download URL"), "{skipped:?}");
        assert!(skipped[2].contains("no digest"), "{skipped:?}");
        assert!(skipped[3].contains("sha256:"), "{skipped:?}");
        assert!(skipped.iter().all(|line| line.contains("of release v1")));
    }

    #[test]
    fn a_releases_answer_with_extra_and_null_fields_still_reads() {
        let releases: Vec<Release> = serde_json::from_str(
            r#"[{"tag_name":"v1","draft":false,"prerelease":false,"author":{"login":"x"},
                "assets":[{"name":"pi-0.1.0.tar.gz","state":"uploaded","size":5,"digest":null,
                "browser_download_url":"https://github.com/o/r/releases/download/v1/pi-0.1.0.tar.gz",
                "uploader":null}]}]"#,
        )
        .unwrap();
        let (versions, skipped) = versions_from_releases("pi", &releases);

        assert!(versions.is_empty());
        assert!(skipped[0].contains("no digest"), "{skipped:?}");
    }

    #[test]
    fn the_releases_url_is_built_under_the_configured_api() {
        assert_eq!(
            releases_url("https://api.github.com/", "o/r"),
            "https://api.github.com/repos/o/r/releases?per_page=100"
        );
    }
}
