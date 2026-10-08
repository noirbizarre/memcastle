use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use crate::memcastle::source::host::{access_token, run_process};

const PAGE: usize = 20;
const MAX_LIST_PAGES: usize = 500;
const MAX_MESSAGE_PAGES: usize = 500;
const MAX_PROJECT_PAGES: usize = 100;
// A plain curl user agent received an HTML edge challenge even with a freshly issued OAuth token.
// This browser-style value received a JSON 200 without any cookie, including after token refresh.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub(super) fn check_auth() -> Result<(), String> {
    bearer().map(|_| ())
}

fn bearer() -> Result<String, String> {
    if let Ok(token) = std::env::var("MEMCASTLE_CHATGPT_BEARER")
        && !token.is_empty()
    {
        return Ok(token);
    }
    access_token().map_err(|_| {
        "sign in with `memcastle source auth chatgpt` before mining the web source".into()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Project {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub(super) struct Listing {
    pub id: String,
    pub project: Option<Project>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn next_cursor(response: &Value) -> Result<Option<&str>, String> {
    match response.get("cursor") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cursor)) if !cursor.is_empty() && cursor.len() <= 2048 => {
            Ok(Some(cursor))
        }
        _ => Err("ChatGPT returned an invalid project-page cursor".into()),
    }
}

pub(super) fn projects() -> Result<Vec<Project>, String> {
    let mut result = Vec::new();
    let mut ids = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PROJECT_PAGES {
        let mut path = format!("/backend-api/gizmos/snorlax/sidebar?limit={PAGE}");
        if let Some(cursor) = &cursor {
            path.push_str(&format!("&cursor={}", encode(cursor)));
        }
        let response = request(&path)?;
        let items = response
            .get("items")
            .and_then(Value::as_array)
            .ok_or("ChatGPT project sidebar has no items")?;
        if items.len() > PAGE {
            return Err("ChatGPT returned an oversized project sidebar page".into());
        }
        for item in items {
            let project = &item["gizmo"]["gizmo"];
            let id = project
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| valid_id(id))
                .ok_or("a ChatGPT project has no valid ID")?;
            let name = project["display"]["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or("a ChatGPT project has no display name")?;
            if !ids.insert(id.to_string()) {
                return Err("ChatGPT project sidebar repeated a project; retry the mine".into());
            }
            result.push(Project {
                id: id.to_string(),
                name: name.to_string(),
            });
        }
        let Some(next) = next_cursor(&response)? else {
            return Ok(result);
        };
        if items.is_empty() || !cursors.insert(next.to_string()) {
            return Err("ChatGPT project sidebar repeated a cursor or ended prematurely".into());
        }
        cursor = Some(next.to_string());
    }
    Err("ChatGPT project sidebar exceeds its paging limit; refusing a partial listing".into())
}

pub(super) fn select_projects(
    selection: &str,
    available: &[Project],
) -> Result<Vec<Project>, String> {
    let mut selected = BTreeMap::new();
    if selection.is_empty() {
        return Err("projects is empty; omit it to mine everything".into());
    }
    for selector in selection.split(',') {
        let project = if let Some(id) = selector.strip_prefix("id:") {
            if !valid_id(id) {
                return Err("projects=id:… needs a valid ChatGPT project ID".into());
            }
            available
                .iter()
                .find(|project| project.id == id)
                .ok_or("the selected ChatGPT project ID is unavailable; check its ID and account")?
        } else if let Some(name) = selector.strip_prefix("name:") {
            if name.is_empty() {
                return Err("projects=name:… needs a nonempty display name".into());
            }
            let mut matches = available.iter().filter(|project| project.name == name);
            let first = matches.next().ok_or("the named ChatGPT project was not found; a renamed project needs its new name or ID")?;
            if matches.next().is_some() {
                return Err(
                    "more than one ChatGPT project has that name; use projects=id:…".into(),
                );
            }
            first
        } else {
            return Err(
                "projects accepts comma-separated id:PROJECT_ID or name:EXACT_NAME values".into(),
            );
        };
        if selected
            .insert(project.id.clone(), project.clone())
            .is_some()
        {
            return Err("the same ChatGPT project is selected twice".into());
        }
    }
    Ok(selected.into_values().collect())
}

fn project_conversations(project: &Project) -> Result<Vec<Listing>, String> {
    let mut listings = Vec::new();
    let mut ids = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_LIST_PAGES {
        let mut path = format!(
            "/backend-api/gizmos/{}/conversations?limit={PAGE}",
            encode(&project.id)
        );
        if let Some(cursor) = &cursor {
            path.push_str(&format!("&cursor={}", encode(cursor)));
        }
        let response = request(&path)?;
        let items = response
            .get("items")
            .and_then(Value::as_array)
            .ok_or("ChatGPT project conversations have no items")?;
        if items.len() > PAGE {
            return Err("ChatGPT returned an oversized project conversation page".into());
        }
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| valid_id(id))
                .ok_or("a ChatGPT project conversation has no valid ID")?;
            if item.get("gizmo_id").and_then(Value::as_str) != Some(project.id.as_str()) {
                return Err(
                    "a ChatGPT conversation belongs to another project; retry the mine".into(),
                );
            }
            if !ids.insert(id.to_string()) {
                return Err(
                    "ChatGPT project conversation pages repeated an ID; retry the mine".into(),
                );
            }
            listings.push(Listing {
                id: id.to_string(),
                project: Some(project.clone()),
            });
        }
        let Some(next) = next_cursor(&response)? else {
            return Ok(listings);
        };
        if items.is_empty() || !cursors.insert(next.to_string()) {
            return Err(
                "ChatGPT project conversations repeated a cursor or ended prematurely".into(),
            );
        }
        cursor = Some(next.to_string());
    }
    Err("ChatGPT project conversations exceed the page limit; refusing partial history".into())
}

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
    let bearer = escape(&bearer()?)?;
    if bearer.is_empty() {
        return Err("the ChatGPT bearer session is empty".into());
    }
    let mut config = format!(
        "url = \"https://chatgpt.com{path}\"\nheader = \"Authorization: Bearer {bearer}\"\nheader = \"Accept: application/json\"\nheader = \"User-Agent: {USER_AGENT}\"\n"
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

pub(super) fn list(selection: Option<&[Project]>) -> Result<Vec<Listing>, String> {
    let mut found: BTreeMap<String, Option<Project>> = BTreeMap::new();
    if selection.is_none() {
        for id in list_status(false)? {
            found.insert(id, None);
        }
        for id in list_status(true)? {
            if found.insert(id, None).is_some() {
                return Err(
                    "a ChatGPT conversation changed archive status during paging; retry the mine"
                        .into(),
                );
            }
        }
    }
    let available;
    let selected = if let Some(selected) = selection {
        selected
    } else {
        available = projects()?;
        &available
    };
    for project in selected {
        for entry in project_conversations(project)? {
            if found
                .get(&entry.id)
                .and_then(Option::as_ref)
                .is_some_and(|known| known.id != project.id)
            {
                return Err(
                    "a ChatGPT conversation appears in multiple projects; retry the mine".into(),
                );
            }
            found.insert(entry.id, Some(project.clone()));
        }
    }
    Ok(found
        .into_iter()
        .map(|(id, project)| Listing { id, project })
        .collect())
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
    if !valid_id(id) {
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
