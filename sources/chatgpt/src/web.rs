use std::collections::HashSet;

use serde_json::{Value, json};

use crate::memcastle::source::host::run_process;

const PAGE: usize = 20;
const MAX_LIST_PAGES: usize = 500;
const MAX_MESSAGE_PAGES: usize = 500;

fn escape(value: &str) -> Result<String, String> {
    if value.contains(['\r', '\n', '\0']) {
        return Err("a web session header contains a line break".into());
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn request(path: &str) -> Result<Value, String> {
    let bearer = std::env::var("MEMCASTLE_CHATGPT_BEARER").map_err(|_| {
        "set MEMCASTLE_CHATGPT_BEARER in the daemon environment for the experimental web source"
            .to_string()
    })?;
    let bearer = escape(&bearer)?;
    if bearer.is_empty() {
        return Err("the ChatGPT bearer session is empty".into());
    }
    let mut config = format!(
        "url = \"https://chatgpt.com{path}\"\nheader = \"Authorization: Bearer {bearer}\"\nheader = \"Accept: application/json\"\n"
    );
    if let Ok(cookie) = std::env::var("MEMCASTLE_CHATGPT_COOKIE") {
        config.push_str(&format!("header = \"Cookie: {}\"\n", escape(&cookie)?));
    }
    // Secrets go via stdin, never into a command argument or a URI. No redirect may forward them to another origin.
    let response = run_process(
        "curl",
        &[
            // -q must be first: a daemon user's ~/.curlrc must not redirect these secret-bearing requests.
            "-q".into(),
            "--silent".into(),
            "--show-error".into(),
            "--config".into(),
            "-".into(),
            "--max-time".into(),
            "20".into(),
            "--max-redirs".into(),
            "0".into(),
            "--write-out".into(),
            "\nMEMCASTLE_HTTP_STATUS:%{http_code}".into(),
        ],
        Some(config.as_bytes()),
    )
    .map_err(|_| "cannot run curl; check that it is installed and granted".to_string())?;
    if response.status != 0 {
        return Err("ChatGPT request failed; check connectivity, curl and the session (no response is logged)".into());
    }
    let text = String::from_utf8(response.stdout)
        .map_err(|_| "ChatGPT returned non-UTF-8 data".to_string())?;
    let (body, code) = text
        .rsplit_once("\nMEMCASTLE_HTTP_STATUS:")
        .ok_or("ChatGPT response lacks an HTTP status")?;
    match code.trim() {
        "200" => serde_json::from_str(body).map_err(|_| "ChatGPT returned malformed JSON".into()),
        "401" | "403" => {
            Err("ChatGPT refused the session; replace the daemon's session credentials".into())
        }
        "429" => Err("ChatGPT rate-limited this source; retry later".into()),
        "301" | "302" | "307" | "308" => Err(
            "ChatGPT redirected a private API request; refusing to forward session credentials"
                .into(),
        ),
        _ => Err(format!(
            "ChatGPT private API returned HTTP {}; its interface may have changed",
            code.trim()
        )),
    }
}

fn list_status(archived: bool) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    let mut expected_total = None;
    for page in 0..MAX_LIST_PAGES {
        let offset = page * PAGE;
        let response = request(&format!(
            "/backend-api/conversations?offset={offset}&limit={PAGE}&is_archived={archived}"
        ))?;
        let total = response
            .get("total")
            .and_then(Value::as_u64)
            .ok_or("ChatGPT conversation list has no total")?;
        if expected_total.is_some_and(|known| known != total) {
            return Err("ChatGPT conversation list changed during paging; retry the mine".into());
        }
        expected_total = Some(total);
        let items = response
            .get("items")
            .and_then(Value::as_array)
            .ok_or("ChatGPT conversation list has no items")?;
        if items.len() > PAGE {
            return Err("ChatGPT returned an oversized conversation page".into());
        }
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .ok_or("a ChatGPT conversation lacks an id")?;
            if !seen.insert(id.to_string()) {
                return Err(
                    "ChatGPT conversation list shifted during paging; retry the mine".into(),
                );
            }
            ids.push(id.to_string());
        }
        if ids.len() as u64 >= total {
            ids.sort();
            return Ok(ids);
        }
        if items.is_empty() {
            return Err("ChatGPT conversation listing ended before its reported total".into());
        }
    }
    Err(
        "ChatGPT conversation listing exceeds 10000 entries; narrow the source or use an export"
            .into(),
    )
}

pub(super) fn list() -> Result<Vec<String>, String> {
    let mut ids = list_status(false)?;
    ids.extend(list_status(true)?);
    ids.sort();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(
            "a ChatGPT conversation changed archive status during paging; retry the mine".into(),
        );
    }
    Ok(ids)
}

fn assembled(id: &str, detail: &Value, mut pages: Vec<Vec<Value>>) -> Result<Value, String> {
    pages.reverse();
    let messages: Vec<Value> = pages.into_iter().flatten().collect();
    let mut seen_ids = HashSet::new();
    for message in &messages {
        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .ok_or("a ChatGPT message has no id")?;
        if !seen_ids.insert(message_id) {
            return Err("ChatGPT message pages overlap; refusing a partial transcript".into());
        }
    }
    // Keep every source-specific field from the detail response, except the transient paging state.
    let mut conversation = detail.clone();
    let fields = conversation
        .as_object_mut()
        .ok_or("ChatGPT returned no conversation object")?;
    fields.remove("page_info");
    fields.insert("id".into(), json!(id));
    fields.insert("messages".into(), json!(messages));
    Ok(conversation)
}

pub(super) fn conversation(id: &str) -> Result<Value, String> {
    // An opaque ID is never allowed to rewrite the fixed origin or path.
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("ChatGPT returned an invalid conversation ID".into());
    }
    let mut detail = request(&format!(
        "/backend-api/conversations/{}?num_turns=100&include_has_versions=true",
        encode(id)
    ))?;
    let initial = detail
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("ChatGPT conversation has no messages array")?
        .clone();
    let mut pages = vec![initial];
    let mut seen_cursors = HashSet::new();
    for _ in 0..MAX_MESSAGE_PAGES {
        let info = detail
            .get("page_info")
            .ok_or("ChatGPT conversation has no page_info")?;
        if !info
            .get("has_previous_page")
            .and_then(Value::as_bool)
            .ok_or("ChatGPT conversation has no paging flag")?
        {
            return assembled(id, &detail, pages);
        }
        let before = info
            .get("start_cursor")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("ChatGPT history has no previous-page cursor")?;
        if !seen_cursors.insert(before.to_string()) {
            return Err("ChatGPT message cursor repeated; refusing a partial transcript".into());
        }
        // Keep the initial metadata: older pages may contain only messages and page_info.
        let page = request(&format!(
            "/backend-api/conversations/{}/messages?before={}&num_turns=100&include_has_versions=true",
            encode(id),
            encode(before)
        ))?;
        let messages = page
            .get("messages")
            .and_then(Value::as_array)
            .ok_or("ChatGPT message page has no messages")?;
        if messages.is_empty() {
            return Err("ChatGPT returned an empty page before the end of history".into());
        }
        pages.push(messages.clone());
        let info = page
            .get("page_info")
            .ok_or("ChatGPT message page has no page_info")?;
        if !info
            .get("has_previous_page")
            .and_then(Value::as_bool)
            .ok_or("ChatGPT message page has no paging flag")?
        {
            return assembled(id, &detail, pages);
        }
        detail["page_info"] = info.clone();
    }
    Err("ChatGPT history exceeded the message-page limit; refusing a partial transcript".into())
}
