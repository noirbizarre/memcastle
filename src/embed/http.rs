//! The `http` embedding provider: an OpenAI-compatible `/embeddings` endpoint.
//!
//! OpenAI, Ollama, llama.cpp's server and vLLM all speak this shape, so one
//! implementation covers hosted and local models. A local server needs no key;
//! a hosted one gets its key as a bearer token from `config::Secret`, which
//! never reaches a log line or an error message.

use std::time::Duration;

use serde::Deserialize;

use super::{EmbedFuture, Embedder};
use crate::domain::{EMBEDDING_DIMENSION, Secret};
use crate::error::{Error, Result};

/// Calls `POST {base_url}/embeddings`.
pub struct HttpEmbedder {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: Option<Secret>,
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    index: usize,
    embedding: Vec<f32>,
}

impl HttpEmbedder {
    /// A provider for the API at `base_url` (no trailing `/embeddings`).
    ///
    /// # Errors
    ///
    /// [`Error::EmbeddingFailed`] if the HTTP client cannot be built.
    pub fn new(
        base_url: String,
        model: String,
        api_key: Option<Secret>,
        timeout: Duration,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|source| Error::EmbeddingFailed {
                message: format!("could not build the HTTP client: {source}"),
            })?;
        Ok(Self {
            client,
            endpoint: format!("{}/embeddings", base_url.trim_end_matches('/')),
            model,
            api_key,
        })
    }

    async fn run(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut request = self.client.post(&self.endpoint).json(&serde_json::json!({
            "model": self.model,
            "input": texts,
            // Lets a model with a longer native vector (OpenAI's v3 family)
            // return the dimension the palace stores; servers that do not
            // support it ignore the field.
            "dimensions": EMBEDDING_DIMENSION,
        }));
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key.expose());
        }
        // `without_url`: the message must not echo a URL that may carry
        // credentials in its userinfo.
        let response = request
            .send()
            .await
            .map_err(|source| Error::EmbeddingFailed {
                message: format!("the request failed: {}", source.without_url()),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body: String = body.trim().chars().take(300).collect();
            return Err(Error::EmbeddingFailed {
                message: format!("the endpoint answered {status}: {body}"),
            });
        }
        let mut parsed: Response =
            response
                .json()
                .await
                .map_err(|source| Error::EmbeddingFailed {
                    message: format!(
                        "the endpoint's answer is not an OpenAI-style embeddings response: {}",
                        source.without_url()
                    ),
                })?;
        // The API numbers its items; a server is free to reorder them.
        parsed.data.sort_by_key(|item| item.index);
        Ok(parsed.data.into_iter().map(|item| item.embedding).collect())
    }
}

impl Embedder for HttpEmbedder {
    fn embed<'a>(&'a self, texts: &'a [String]) -> EmbedFuture<'a> {
        Box::pin(self.run(texts))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Json;
    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use serde_json::{Value, json};

    use super::*;

    /// What the stub saw: the authorization header and the JSON body.
    type Seen = Arc<Mutex<Vec<(Option<String>, Value)>>>;

    /// A local OpenAI-style server answering `answer` for every request.
    async fn stub(status: StatusCode, answer: Value) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let state = (seen.clone(), status, answer);
        let app = Router::new()
            .route(
                "/v1/embeddings",
                post(
                    |State((seen, status, answer)): State<(Seen, StatusCode, Value)>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        let auth = headers
                            .get("authorization")
                            .map(|v| v.to_str().unwrap_or_default().to_string());
                        seen.lock().unwrap().push((auth, body));
                        (status, Json(answer))
                    },
                ),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://127.0.0.1:{port}/v1"), seen)
    }

    fn vector(value: f32) -> Vec<f32> {
        vec![value; EMBEDDING_DIMENSION]
    }

    #[tokio::test]
    async fn vectors_come_back_in_input_order_even_when_the_server_reorders_them() {
        let answer = json!({"data": [
            {"index": 1, "embedding": vector(2.0)},
            {"index": 0, "embedding": vector(1.0)},
        ]});
        let (url, _) = stub(StatusCode::OK, answer).await;
        let embedder = HttpEmbedder::new(url, "m".into(), None, Duration::from_secs(10)).unwrap();
        let vectors = embedder
            .run(&["a".to_string(), "b".to_string()])
            .await
            .expect("answers");
        assert_eq!(vectors[0][0], 1.0);
        assert_eq!(vectors[1][0], 2.0);
    }

    #[tokio::test]
    async fn the_request_names_the_model_and_the_stored_dimension() {
        let answer = json!({"data": [{"index": 0, "embedding": vector(1.0)}]});
        let (url, seen) = stub(StatusCode::OK, answer).await;
        let embedder =
            HttpEmbedder::new(url, "nomic".into(), None, Duration::from_secs(10)).unwrap();
        embedder.run(&["a".to_string()]).await.expect("answers");
        let (_, body) = seen.lock().unwrap()[0].clone();
        assert_eq!(body["model"], "nomic");
        assert_eq!(body["dimensions"], 768);
        assert_eq!(body["input"], json!(["a"]));
    }

    #[tokio::test]
    async fn a_key_is_sent_as_a_bearer_token_only_when_configured() {
        let answer = json!({"data": [{"index": 0, "embedding": vector(1.0)}]});
        let (url, seen) = stub(StatusCode::OK, answer).await;
        let keyed = HttpEmbedder::new(
            url.clone(),
            "m".into(),
            Some(Secret::new("sk-test")),
            Duration::from_secs(10),
        )
        .unwrap();
        keyed.run(&["a".to_string()]).await.unwrap();
        let keyless = HttpEmbedder::new(url, "m".into(), None, Duration::from_secs(10)).unwrap();
        keyless.run(&["a".to_string()]).await.unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0.as_deref(), Some("Bearer sk-test"));
        assert_eq!(
            seen[1].0, None,
            "a local server must not get an empty bearer"
        );
    }

    #[tokio::test]
    async fn an_error_status_is_reported_with_the_servers_message_and_never_the_key() {
        let (url, _) = stub(StatusCode::UNAUTHORIZED, json!({"error": "bad key"})).await;
        let embedder = HttpEmbedder::new(
            url,
            "m".into(),
            Some(Secret::new("sk-secret")),
            Duration::from_secs(10),
        )
        .unwrap();
        let message = embedder
            .run(&["a".to_string()])
            .await
            .expect_err("401")
            .to_string();
        assert!(message.contains("401"), "{message}");
        assert!(message.contains("bad key"), "{message}");
        assert!(!message.contains("sk-secret"), "{message}");
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_is_a_failure_not_a_panic() {
        let embedder = HttpEmbedder::new(
            "http://127.0.0.1:1/v1".into(),
            "m".into(),
            None,
            Duration::from_secs(2),
        )
        .unwrap();
        let error = embedder.run(&["a".to_string()]).await.expect_err("refused");
        assert!(matches!(error, Error::EmbeddingFailed { .. }), "{error}");
    }
}
