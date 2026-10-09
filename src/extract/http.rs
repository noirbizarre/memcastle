//! The `http` extraction provider: an OpenAI-compatible `/chat/completions` endpoint.
//!
//! OpenAI, Ollama, llama.cpp's server and vLLM all speak this shape, so one implementation covers hosted and local
//! models. A local server needs no key; a hosted one gets its key as a bearer token from `config::Secret`, which
//! never reaches a log line or an error message.
//!
//! Each text is one request: the model is told the closed vocabulary and asked for JSON. Two kinds of failure are
//! treated differently on purpose. An endpoint that cannot be reached or answers with an error status fails the job
//! (the next sweep or a retry picks up where it stopped). A model whose *reply* is not the JSON asked for is a
//! property of that one text, and failing on it would block every drawer behind it for ever: it is logged and read
//! as "nothing found".

use std::time::Duration;

use serde::Deserialize;

use super::{ExtractFuture, Extractor, WireGraph};
use crate::domain::{EntityKind, ExtractedGraph, Predicate, PreferenceMatch, Secret};
use crate::error::{Error, Result};

/// Calls `POST {base_url}/chat/completions`.
pub struct HttpExtractor {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: Option<Secret>,
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
}

impl HttpExtractor {
    /// A provider for the API at `base_url` (no trailing `/chat/completions`).
    ///
    /// # Errors
    ///
    /// [`Error::ExtractionFailed`] if the HTTP client cannot be built.
    pub fn new(
        base_url: String,
        model: String,
        api_key: Option<Secret>,
        timeout: Duration,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|source| Error::ExtractionFailed {
                message: format!("could not build the HTTP client: {source}"),
            })?;
        Ok(Self {
            client,
            endpoint: format!("{}/chat/completions", base_url.trim_end_matches('/')),
            model,
            api_key,
        })
    }

    /// What the model is told: the closed vocabulary and the answer's shape.
    fn system_prompt() -> String {
        format!(
            "You extract a knowledge graph from the text the user sends. Reply with one JSON object and nothing \
             else: {{\"entities\": [{{\"name\": string, \"kind\": string}}], \"relations\": [{{\"subject\": string, \
             \"predicate\": string, \"object\": string, \"confidence\": number}}]}}. \
             \"kind\" must be one of: {kinds}. \"predicate\" must be one of: {predicates}. \
             \"subject\" and \"object\" must be names listed in \"entities\". \"confidence\" is between 0 and 1. \
             Only state what the text says; if it names nothing, reply with empty lists.",
            kinds = EntityKind::ALL.map(EntityKind::as_str).join(", "),
            predicates = Predicate::ALL.map(Predicate::as_str).join(", "),
        )
    }

    async fn one(
        &self,
        text: &str,
        preference: Option<&PreferenceMatch>,
    ) -> Result<ExtractedGraph> {
        let context = preference.map(|preference| format!("Source preference: {:?} (connector: {}, matched criterion: {}). This is only a cue for what to examine; do not infer truth from it or invent facts.", preference.level, preference.source.as_deref().unwrap_or("unknown"), preference.criterion.as_deref().unwrap_or("none")));
        let mut messages =
            vec![serde_json::json!({"role": "system", "content": Self::system_prompt()})];
        if let Some(context) = context {
            messages.push(serde_json::json!({"role": "system", "content": context}));
        }
        messages.push(serde_json::json!({"role": "user", "content": text}));
        let mut request = self.client.post(&self.endpoint).json(&serde_json::json!({
            "model": self.model,
            "temperature": 0,
            "response_format": {"type": "json_object"},
            "messages": messages,
        }));
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key.expose());
        }
        // `without_url`: the message must not echo a URL that may carry credentials in its userinfo.
        let response = request
            .send()
            .await
            .map_err(|source| Error::ExtractionFailed {
                message: format!("the request failed: {}", source.without_url()),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body: String = body.trim().chars().take(300).collect();
            return Err(Error::ExtractionFailed {
                message: format!("the endpoint answered {status}: {body}"),
            });
        }
        let parsed: Response = response
            .json()
            .await
            .map_err(|source| Error::ExtractionFailed {
                message: format!(
                    "the endpoint's answer is not an OpenAI-style chat completion: {}",
                    source.without_url()
                ),
            })?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .unwrap_or_default();
        Ok(parse_reply(&content))
    }
}

/// Read a model's reply into a graph. Models wrap JSON in a Markdown fence or a sentence, so the outermost braces
/// are taken; a reply with no usable object is "nothing found", logged.
fn parse_reply(content: &str) -> ExtractedGraph {
    let object = content
        .find('{')
        .zip(content.rfind('}'))
        .filter(|(start, end)| start < end)
        .map(|(start, end)| &content[start..=end]);
    match object.map(serde_json::from_str::<WireGraph>) {
        Some(Ok(wire)) => wire.into_graph(),
        _ => {
            tracing::warn!(
                "the extraction model did not reply with the JSON asked for; reading it as empty"
            );
            ExtractedGraph::default()
        }
    }
}

impl Extractor for HttpExtractor {
    fn name(&self) -> &'static str {
        "http"
    }

    fn extract<'a>(&'a self, texts: &'a [String]) -> ExtractFuture<'a> {
        Box::pin(async move {
            let mut graphs = Vec::with_capacity(texts.len());
            for text in texts {
                graphs.push(self.one(text, None).await?);
            }
            Ok(graphs)
        })
    }

    fn extract_with_preferences<'a>(
        &'a self,
        texts: &'a [String],
        context: &'a [PreferenceMatch],
    ) -> ExtractFuture<'a> {
        Box::pin(async move {
            let mut graphs = Vec::with_capacity(texts.len());
            for (index, text) in texts.iter().enumerate() {
                graphs.push(self.one(text, context.get(index)).await?);
            }
            Ok(graphs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_wrapped_in_a_markdown_fence_is_still_read() {
        let graph = parse_reply(
            "```json\n{\"entities\":[{\"name\":\"Ada\",\"kind\":\"person\"}],\"relations\":[]}\n```",
        );
        assert_eq!(graph.entities[0].name, "Ada");
        assert_eq!(graph.entities[0].kind, EntityKind::Person);
    }

    #[test]
    fn a_reply_that_is_not_json_reads_as_nothing_found() {
        assert_eq!(
            parse_reply("I could not find anything."),
            ExtractedGraph::default()
        );
        assert_eq!(parse_reply(""), ExtractedGraph::default());
        assert_eq!(parse_reply("{not json}"), ExtractedGraph::default());
    }

    #[test]
    fn the_prompt_states_the_whole_closed_vocabulary() {
        let prompt = HttpExtractor::system_prompt();
        for kind in EntityKind::ALL {
            assert!(prompt.contains(kind.as_str()), "{kind:?}");
        }
        for predicate in Predicate::ALL {
            assert!(prompt.contains(predicate.as_str()), "{predicate:?}");
        }
    }
}
