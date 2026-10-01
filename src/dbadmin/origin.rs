//! Which web pages may talk to the endpoint.
//!
//! A WebSocket to `127.0.0.1` is not protected by the network boundary:
//! any page open in the developer's browser can dial it, whatever site it came
//! from. Browsers do send an `Origin` header on that dial, and unlike a token
//! it cannot be forged by a page, so this is what stops a random tab from
//! reading the palace. Clients that are not browsers send no `Origin` and are
//! unaffected; they are held to the authentication layer instead.

use axum::http::{HeaderMap, Uri, header::ORIGIN};

/// The schemes a local development tool is served from. `tauri` is the desktop
/// Surrealist app's own scheme.
const LOCAL_SCHEMES: [&str; 3] = ["http", "https", "tauri"];

/// The origins allowed to use the endpoint: any page served from this machine,
/// plus the exact origins the operator added.
#[derive(Debug, Clone, Default)]
pub struct OriginPolicy {
    /// Extra origins, compared whole (`https://app.surrealdb.com`), never by
    /// suffix or pattern: a wildcard here would reopen the hole this closes.
    extra: Vec<String>,
}

impl OriginPolicy {
    /// A policy allowing this machine's own pages and `extra` origins.
    #[must_use]
    pub fn new(extra: impl IntoIterator<Item = String>) -> Self {
        Self {
            // Trailing slashes are not part of an origin; tolerate them so a
            // pasted URL works instead of silently never matching.
            extra: extra
                .into_iter()
                .map(|origin| origin.trim_end_matches('/').to_string())
                .collect(),
        }
    }

    /// Whether a request carrying `origin` may proceed.
    #[must_use]
    pub fn allows(&self, origin: &str) -> bool {
        self.extra
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(origin))
            || is_local_origin(origin)
    }

    /// Check a request's headers: `Ok(None)` for a non-browser client with no
    /// `Origin`, `Ok(Some(origin))` for an allowed one, `Err(origin)` for a
    /// refused one.
    ///
    /// # Errors
    ///
    /// The offending origin (or an empty string when it is not valid text).
    pub fn check<'h>(&self, headers: &'h HeaderMap) -> Result<Option<&'h str>, String> {
        let Some(value) = headers.get(ORIGIN) else {
            return Ok(None);
        };
        let origin = value.to_str().map_err(|_| String::new())?;
        if self.allows(origin) {
            Ok(Some(origin))
        } else {
            Err(origin.to_string())
        }
    }
}

/// Whether `origin` is a page served from this machine: a loopback address or a
/// `localhost` name (including `*.localhost`, which browsers resolve locally).
fn is_local_origin(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    let Some(scheme) = uri.scheme_str() else {
        return false;
    };
    if !LOCAL_SCHEMES.contains(&scheme) {
        return false;
    }
    // `Uri::host` keeps the brackets of an IPv6 literal.
    let host = uri.host().unwrap_or_default().trim_matches(['[', ']']);
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_served_from_this_machine_are_allowed() {
        let policy = OriginPolicy::default();
        for origin in [
            "http://localhost:3000",
            "https://localhost",
            "http://127.0.0.1:8000",
            "http://[::1]:5173",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(policy.allows(origin), "{origin}");
        }
    }

    #[test]
    fn a_foreign_site_is_refused_unless_it_was_added_explicitly() {
        let policy = OriginPolicy::default();
        assert!(!policy.allows("https://app.surrealdb.com"));
        assert!(!policy.allows("https://evil.example"));

        let policy = OriginPolicy::new(["https://app.surrealdb.com/".to_string()]);
        assert!(policy.allows("https://app.surrealdb.com"));
        assert!(!policy.allows("https://app.surrealdb.com.evil.example"));
    }

    #[test]
    fn a_hostname_that_merely_contains_localhost_is_not_local() {
        let policy = OriginPolicy::default();
        assert!(!policy.allows("http://localhost.evil.example"));
        assert!(!policy.allows("http://evillocalhost"));
        assert!(!policy.allows("http://127.0.0.1.evil.example"));
    }

    #[test]
    fn a_local_looking_origin_with_an_unknown_scheme_is_refused() {
        assert!(!OriginPolicy::default().allows("file://localhost"));
        assert!(!OriginPolicy::default().allows("null"));
    }

    #[test]
    fn a_request_without_an_origin_header_is_a_non_browser_client() {
        let headers = HeaderMap::new();
        assert_eq!(OriginPolicy::default().check(&headers), Ok(None));
    }

    #[test]
    fn a_refused_origin_is_reported_back_for_the_log() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, "https://evil.example".parse().unwrap());
        assert_eq!(
            OriginPolicy::default().check(&headers),
            Err("https://evil.example".to_string())
        );
    }
}
