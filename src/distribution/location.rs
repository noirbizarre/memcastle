//! Where an index or a package archive is, and how to read it.
//!
//! A location is `https://`, `http://` to this machine only, `file://`, or an absolute path. The same four forms
//! serve a public registry, a mirror on a share and an offline directory, so "install without the network" is not a
//! separate mechanism: it is a registry whose location is a path.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one download may take in total. A package is megabytes; this bounds a server that never finishes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How many redirects a download follows.
const MAX_REDIRECTS: usize = 5;

/// A place an index or an archive can be read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// An `https://` URL, or an `http://` one on this machine.
    Http(reqwest::Url),
    /// A file or directory on disk.
    File(PathBuf),
}

/// Whether plain `http` is acceptable for this URL: only when it never leaves the machine.
///
/// A package fetched over plain `http` could be swapped in transit, and although the index's digest would catch that,
/// the index itself is fetched the same way and has no digest to catch it.
fn is_acceptable(url: &reqwest::Url) -> bool {
    match url.scheme() {
        "https" => true,
        // `host_str` writes an IPv6 address in brackets.
        "http" => matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        _ => false,
    }
}

impl Location {
    /// Read a location as the user wrote it.
    ///
    /// # Errors
    ///
    /// A sentence saying what is wrong with it.
    pub fn parse(raw: &str) -> std::result::Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("it is empty".to_string());
        }
        if raw.starts_with("file://") {
            // The URL's own conversion, because `file:///C:/x` is a Windows path and `/C:/x` is not.
            let url =
                reqwest::Url::parse(raw).map_err(|e| format!("it is not a valid URL: {e}"))?;
            let path = url.to_file_path().map_err(|()| {
                "a `file://` URL must name an absolute path on this machine, like `file:///srv/index.json`"
                    .to_string()
            })?;
            return Self::absolute_path(&path.to_string_lossy());
        }
        if let Some((scheme, _)) = raw.split_once("://") {
            if scheme != "http" && scheme != "https" {
                return Err(format!(
                    "the scheme `{scheme}://` is not supported; use `https://`, `file://` or a path"
                ));
            }
            let url =
                reqwest::Url::parse(raw).map_err(|e| format!("it is not a valid URL: {e}"))?;
            if !is_acceptable(&url) {
                return Err(
                    "plain `http://` is only allowed to this machine (localhost); use `https://`"
                        .to_string(),
                );
            }
            return Ok(Self::Http(url));
        }
        Self::absolute_path(raw)
    }

    fn absolute_path(path: &str) -> std::result::Result<Self, String> {
        let path = PathBuf::from(path);
        // Relative to whatever directory the daemon started in, which is not a place anyone chose.
        if !path.is_absolute() {
            return Err(
                "a path must be absolute (the daemon does not share your working directory)"
                    .to_string(),
            );
        }
        Ok(Self::File(path))
    }

    /// The location of the index itself: a directory means the index file inside it, and a URL ending in `/` too.
    #[must_use]
    pub fn as_index(self, file_name: &str) -> Self {
        match self {
            Self::File(path) if path.is_dir() => Self::File(path.join(file_name)),
            Self::Http(url) if url.path().ends_with('/') => {
                Self::Http(url.join(file_name).unwrap_or(url))
            }
            other => other,
        }
    }

    /// The location `relative` names, from this one: an absolute location as itself, anything else beside this file.
    ///
    /// # Errors
    ///
    /// A sentence saying why it names nothing acceptable.
    pub fn join(&self, relative: &str) -> std::result::Result<Self, String> {
        if relative.contains("://") {
            return Self::parse(relative);
        }
        match self {
            Self::Http(base) => {
                let url = base
                    .join(relative)
                    .map_err(|e| format!("`{relative}` is not a valid relative URL: {e}"))?;
                // `//other.example/x` is relative in form and absolute in effect: it must still be acceptable.
                if is_acceptable(&url) {
                    Ok(Self::Http(url))
                } else {
                    Err(format!(
                        "`{relative}` leads somewhere that is not allowed: {url}"
                    ))
                }
            }
            Self::File(base) => {
                let relative_path = Path::new(relative);
                // An index is data from elsewhere: it may point beside itself, never elsewhere on the disk.
                let beside = relative_path
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)));
                if !beside {
                    return Err(format!(
                        "`{relative}` must stay beside the index (no `..`, no absolute path); write an absolute location as `file://`"
                    ));
                }
                Ok(Self::File(
                    base.parent()
                        .unwrap_or_else(|| Path::new("/"))
                        .join(relative_path),
                ))
            }
        }
    }

    /// Read at most `limit` bytes.
    ///
    /// # Errors
    ///
    /// A sentence saying what failed: unreachable, a refusal, too large.
    pub async fn read(&self, limit: usize) -> std::result::Result<Vec<u8>, String> {
        match self {
            Self::File(path) => {
                let path = path.clone();
                tokio::task::spawn_blocking(move || read_file(&path, limit))
                    .await
                    .map_err(|e| format!("reading was interrupted: {e}"))?
            }
            Self::Http(url) => read_http(url, limit).await,
        }
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(url) => write!(f, "{url}"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

fn read_file(path: &Path, limit: usize) -> std::result::Result<Vec<u8>, String> {
    use std::io::Read;
    let file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    // One byte past the limit is how an oversized file is told from one that is exactly the limit.
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!("{} is larger than {limit} bytes", path.display()));
    }
    Ok(bytes)
}

async fn read_http(url: &reqwest::Url, limit: usize) -> std::result::Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("memcastle/", env!("CARGO_PKG_VERSION")))
        // A redirect is where a request leaves the place that was checked, so each hop is checked again.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if is_acceptable(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error(
                    "it redirected to a place that is not allowed (plain http beyond this machine)",
                )
            }
        }))
        .build()
        .map_err(|e| format!("cannot start a request: {e}"))?;
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("cannot reach {url}: {}", describe(e)))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("{url} is larger than {limit} bytes"));
    }
    let mut bytes = Vec::new();
    // Streamed against the limit, because a server may omit or lie about `Content-Length`.
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the download of {url} was cut short: {}", describe(e)))?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(format!("{url} is larger than {limit} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// A request error's message without the URL it repeats.
fn describe(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on whatever platform the tests run on: `/srv/...` is not absolute on Windows.
    fn absolute(tail: &str) -> PathBuf {
        std::env::temp_dir().join("memcastle-location").join(tail)
    }

    fn file_url(path: &Path) -> String {
        reqwest::Url::from_file_path(path).unwrap().to_string()
    }

    #[test]
    fn the_four_forms_of_a_location_are_read_and_the_unsafe_ones_refused() {
        let index = absolute("index.json");
        assert!(matches!(
            Location::parse("https://example.org/index.json"),
            Ok(Location::Http(_))
        ));
        assert!(matches!(
            Location::parse("http://localhost:8000/i.json"),
            Ok(Location::Http(_))
        ));
        assert!(matches!(
            Location::parse("http://127.0.0.1/i.json"),
            Ok(Location::Http(_))
        ));
        assert_eq!(
            Location::parse(&file_url(&index)),
            Ok(Location::File(index.clone()))
        );
        assert_eq!(
            Location::parse(&index.to_string_lossy()),
            Ok(Location::File(index))
        );

        for (raw, expected) in [
            ("http://example.org/i.json", "plain `http://`"),
            ("ftp://example.org/i.json", "not supported"),
            ("relative/index.json", "absolute"),
            ("", "empty"),
        ] {
            let error = Location::parse(raw).unwrap_err();
            assert!(error.contains(expected), "{raw}: {error}");
        }
        // A host in a `file://` URL is a network share on Windows, which is a legitimate absolute path there, and
        // a path that is not on this machine anywhere else.
        #[cfg(unix)]
        assert!(
            Location::parse("file://relative/index.json")
                .unwrap_err()
                .contains("absolute")
        );
    }

    #[test]
    fn a_relative_package_url_resolves_beside_the_index() {
        let http = Location::parse("https://example.org/sources/index.json").unwrap();
        assert_eq!(
            http.join("demo-1.0.0.tar.gz").unwrap().to_string(),
            "https://example.org/sources/demo-1.0.0.tar.gz"
        );
        assert_eq!(
            http.join("https://cdn.example.org/d.tar.gz")
                .unwrap()
                .to_string(),
            "https://cdn.example.org/d.tar.gz"
        );
        let file = Location::File(absolute("sources/index.json"));
        assert_eq!(
            file.join("demo-1.0.0.tar.gz").unwrap(),
            Location::File(absolute("sources/demo-1.0.0.tar.gz"))
        );
    }

    #[test]
    fn an_index_cannot_point_a_relative_url_out_of_its_own_place() {
        let file = Location::File(absolute("sources/index.json"));
        for bad in ["../secret", "/etc/passwd", "a/../../b"] {
            assert!(file.join(bad).is_err(), "{bad} was accepted");
        }
        let http = Location::parse("https://example.org/sources/index.json").unwrap();
        // Protocol-relative: the host changes, so the scheme rules apply to the result.
        assert!(http.join("//example.org/x").is_ok());
        let local = Location::parse("http://localhost/index.json").unwrap();
        assert!(local.join("//evil.example/x").is_err());
    }

    #[test]
    fn a_directory_stands_for_the_index_inside_it() {
        let directory = tempfile::tempdir().unwrap();
        let location =
            Location::File(directory.path().to_path_buf()).as_index("memcastle-index.json");
        assert_eq!(
            location,
            Location::File(directory.path().join("memcastle-index.json"))
        );
        let url = Location::parse("https://example.org/sources/")
            .unwrap()
            .as_index("memcastle-index.json");
        assert_eq!(
            url.to_string(),
            "https://example.org/sources/memcastle-index.json"
        );
    }

    #[tokio::test]
    async fn a_file_is_read_up_to_its_limit_and_no_further() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a");
        std::fs::write(&path, b"12345").unwrap();
        let location = Location::File(path);
        assert_eq!(location.read(5).await.unwrap(), b"12345");
        assert!(
            location
                .read(4)
                .await
                .unwrap_err()
                .contains("larger than 4")
        );
        assert!(
            Location::File(directory.path().join("missing"))
                .read(5)
                .await
                .is_err()
        );
    }

    /// Serve `body` once on a loopback port, as an HTTP server would.
    async fn serve_once(status: &str, body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        });
        format!("http://{address}/index.json")
    }

    #[tokio::test]
    async fn a_loopback_http_server_is_read_and_its_refusals_and_excess_are_reported() {
        let ok = Location::parse(&serve_once("200 OK", b"hello".to_vec()).await).unwrap();
        assert_eq!(ok.read(100).await.unwrap(), b"hello");

        let missing = Location::parse(&serve_once("404 Not Found", Vec::new()).await).unwrap();
        assert!(missing.read(100).await.unwrap_err().contains("404"));

        let large = Location::parse(&serve_once("200 OK", vec![0; 50]).await).unwrap();
        assert!(large.read(10).await.unwrap_err().contains("larger than 10"));
    }
}
