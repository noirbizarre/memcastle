//! GitHub REST calls. A token is passed on curl's stdin, not in a URL, argument, or error.

use serde_json::Value;

use crate::memcastle::source::host::run_process;

const MAX_PAGES: usize = 100;
const PAGE_SIZE: usize = 100;

pub(crate) fn token() -> Option<String> {
    ["GH_TOKEN", "GITHUB_TOKEN"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|token| !token.is_empty()))
}

fn escape(value: &str) -> Result<String, String> {
    if value.contains(['\r', '\n', '\0']) {
        return Err("the GitHub credential contains a line break".into());
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(crate) fn request(path: &str) -> Result<Value, String> {
    if !path.starts_with('/') || path.contains(['\r', '\n', '\0']) || path.starts_with("//") {
        return Err("invalid GitHub API path".into());
    }
    let mut config = format!(
        "url = \"https://api.github.com{path}\"\nheader = \"Accept: application/vnd.github+json\"\nheader = \"X-GitHub-Api-Version: 2022-11-28\"\nheader = \"User-Agent: memcastle-github-source\"\n"
    );
    if let Some(token) = token() {
        config.push_str(&format!(
            "header = \"Authorization: Bearer {}\"\n",
            escape(&token)?
        ));
    }
    let output = run_process(
        "curl",
        &[
            "-q".into(),
            "--silent".into(),
            "--show-error".into(),
            "--config".into(),
            "-".into(),
            "--max-time".into(),
            "30".into(),
            "--max-redirs".into(),
            "0".into(),
            "--write-out".into(),
            "\nMEMCASTLE_HTTP_STATUS:%{http_code}".into(),
        ],
        Some(config.as_bytes()),
    )
    .map_err(|_| "cannot run curl; install it on the daemon's PATH".to_string())?;
    if output.status != 0 {
        return Err("GitHub request failed; check connectivity and the daemon's token".into());
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "GitHub returned non-UTF-8 data")?;
    let (body, code) = text
        .rsplit_once("\nMEMCASTLE_HTTP_STATUS:")
        .ok_or("GitHub returned no HTTP status")?;
    match code.trim() {
        "200" => serde_json::from_str(body).map_err(|_| "GitHub returned malformed JSON".into()),
        "401" | "403" => {
            Err("GitHub refused the request; check token permissions or rate limits".into())
        }
        "404" => Err(
            "GitHub repository or resource is unavailable; check its scope and token permissions"
                .into(),
        ),
        "429" => Err("GitHub rate-limited the source; retry later".into()),
        "301" | "302" | "307" | "308" => {
            Err("GitHub redirected an API request; refusing to forward credentials".into())
        }
        other => Err(format!(
            "GitHub API returned HTTP {other}; retry or check API availability"
        )),
    }
}

/// All pages of a list, bounded so a large account cannot silently yield an incomplete result.
pub(crate) fn pages(path: &str) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    for page in 1..=MAX_PAGES {
        let separator = if path.contains('?') { '&' } else { '?' };
        let response = request(&format!(
            "{path}{separator}per_page={PAGE_SIZE}&page={page}"
        ))?;
        let values = response.as_array().ok_or("GitHub list is not an array")?;
        if values.len() > PAGE_SIZE {
            return Err("GitHub returned an oversized page".into());
        }
        result.extend(values.iter().cloned());
        if values.len() < PAGE_SIZE {
            return Ok(result);
        }
    }
    Err("GitHub list exceeds 10,000 entries; narrow the repository selection".into())
}
