//! The web UI's static files, under `/ui` (docs/adr/035).
//!
//! Only merged into the router when `web.enable` is set, so a daemon that did not ask for a UI answers none. The
//! files are read through [`Assets::find`], which is the same lookup in a package (`<prefix>/share/memcastle`) and
//! in a worktree (`--assets-dir <checkout>`), and which refuses a path that climbs out of the asset root.
//!
//! The files hold no data: the dashboard is a client of `/api` like the CLI, and the authentication layer lets
//! these routes through (and only these) so a browser can load the page that then asks for a token. Nothing here
//! touches storage or jobs.

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HeaderName, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;

use crate::assets::{Asset, Assets, WEB_DIST_DIR, WEB_INDEX};

/// The URL prefix the dashboard is served under; also the `base` the frontend is built with.
pub const UI_PREFIX: &str = "/ui";

/// Whether `path` is the dashboard's: `/ui` or anything below `/ui/`, and not a sibling such as `/uix`.
///
/// Used by the authentication layer, which must agree with this router on what it admits.
#[must_use]
pub fn is_ui_path(path: &str) -> bool {
    path == UI_PREFIX
        || path
            .strip_prefix(UI_PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A page can only load what comes from here: the dashboard talks to its own origin and nothing else, and cannot be
/// framed. Styles allow `unsafe-inline` because the component library injects `<style>` elements at runtime.
const CONTENT_SECURITY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; \
    form-action 'self'; frame-ancestors 'none'";

/// The routes of the dashboard.
pub fn router(assets: Arc<Assets>) -> Router {
    Router::new()
        // `/ui` without the slash would resolve the page's relative URLs against `/`, so it is sent to `/ui/`.
        .route(UI_PREFIX, get(|| async { Redirect::permanent("/ui/") }))
        // A wildcard does not match an empty tail, so the index has its own route.
        .route("/ui/", get(index))
        .route("/ui/{*path}", get(file))
        .with_state(assets)
}

async fn index(State(assets): State<Arc<Assets>>) -> Response {
    serve(&assets, "").await
}

async fn file(State(assets): State<Arc<Assets>>, UrlPath(path): UrlPath<String>) -> Response {
    serve(&assets, &path).await
}

/// Answer one request for `requested`, relative to `web/dist`.
async fn serve(assets: &Assets, requested: &str) -> Response {
    let target = requested.trim_matches('/');
    let target = if target.is_empty() {
        "index.html"
    } else {
        target
    };
    let has_extension = Path::new(target).extension().is_some();

    let found = assets.find(&format!("{WEB_DIST_DIR}/{target}"));
    let response = match found {
        Some(Asset::File(path)) => file_response(&path, target).await,
        // The one embedded asset is the page that says the build is missing.
        Some(Asset::Embedded(bytes)) => unavailable(bytes),
        // A path without an extension is a client-side route, which the page itself resolves; a file that is not
        // there is a 404, never the index, so a broken script URL does not come back as HTML.
        None if !has_extension => match assets.find(WEB_INDEX) {
            Some(Asset::File(path)) => file_response(&path, "index.html").await,
            Some(Asset::Embedded(bytes)) => unavailable(bytes),
            None => not_found(),
        },
        None => not_found(),
    };
    with_security_headers(response)
}

async fn file_response(path: &Path, target: &str) -> Response {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            // Build output under `assets/` carries a content hash in its name, so it never changes under that
            // name; everything else (the index, which names those files) must be revalidated, or an upgraded
            // daemon would keep serving a page that points at files that are gone.
            let cache = if target.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (CONTENT_TYPE, HeaderValue::from_static(content_type(target))),
                    (CACHE_CONTROL, HeaderValue::from_static(cache)),
                ],
                bytes,
            )
                .into_response()
        }
        // Found a moment ago and unreadable now (removed mid-upgrade, or a permissions problem).
        Err(_) => not_found(),
    }
}

/// The page served when the dashboard is enabled but not installed: a 503, so a monitor sees a service that is
/// not ready rather than a page that merely looks fine.
fn unavailable(page: &'static [u8]) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        page,
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        "not found",
    )
        .into_response()
}

fn with_security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // The page's URLs carry no secret (the token lives in `sessionStorage`), but there is no reason to tell
    // another site which dashboard a user came from.
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    response
}

/// The `Content-Type` for a built file, from its extension. A small table: a Vite build emits only these, and an
/// unknown type is served as opaque bytes (with `nosniff`) rather than guessed.
fn content_type(target: &str) -> &'static str {
    match Path::new(target)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "webmanifest") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_ui_prefix_and_what_is_below_it_is_a_dashboard_path() {
        for path in ["/ui", "/ui/", "/ui/assets/index.js", "/ui/a/b"] {
            assert!(is_ui_path(path), "{path}");
        }
        for path in [
            "/uix",
            "/ui-admin",
            "/api/ui",
            "/",
            "/u",
            "/UI",
            "/api/status",
            "/mcp",
        ] {
            assert!(!is_ui_path(path), "{path}");
        }
    }

    #[test]
    fn a_built_file_is_typed_by_its_extension_and_an_unknown_one_is_opaque() {
        assert_eq!(
            content_type("assets/index-ab12.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/index-ab12.CSS"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("favicon.svg"), "image/svg+xml");
        assert_eq!(content_type("assets/font.woff2"), "font/woff2");
        assert_eq!(
            content_type("assets/unknown.bin"),
            "application/octet-stream"
        );
        assert_eq!(content_type("no-extension"), "application/octet-stream");
    }
}
