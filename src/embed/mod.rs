//! Embedding generation: turning text into vectors, kept apart from retrieval.
//!
//! An embedding is *derived* data. This module is how MemCastle obtains one
//! — from a program the operator provides, an OpenAI-compatible HTTP
//! endpoint, or not at all — and nothing here touches storage: `store` keeps
//! the vectors and SurrealDB searches them. Because the provider is
//! pluggable behind [`Embedder`], the palace works with none configured, and
//! a provider that is down degrades search to lexical instead of breaking it.
//!
//! No LLM or model is linked into MemCastle, and credentials are never
//! MemCastle's to manage when a `command` provider is used: the program the
//! operator names owns them.

pub mod command;
pub mod http;
pub mod job;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::config::{EmbeddingProvider, EmbeddingsConfig};
use crate::domain::EMBEDDING_DIMENSION;
use crate::error::{Error, Result};

/// The most characters of one text sent to a provider.
///
/// Mining stores whole files (up to 256 KiB) and providers cap their input in
/// tokens, so an over-long text would make the provider reject the whole
/// batch. The head of a document carries its subject; only the vector is
/// computed from the truncated text, the drawer's content is untouched.
pub const MAX_EMBED_CHARS: usize = 8_000;

/// The future an [`Embedder`] returns.
pub type EmbedFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>>> + Send + 'a>>;

/// Something that turns texts into vectors.
///
/// One call embeds a batch and returns one vector per text, in order.
/// Implementations do not validate dimensions; [`Embeddings`] does, once, for
/// every provider.
pub trait Embedder: Send + Sync + 'static {
    /// Embed `texts`.
    fn embed<'a>(&'a self, texts: &'a [String]) -> EmbedFuture<'a>;
}

/// The daemon's embedding handle: a provider, or nothing.
///
/// Cheap to clone and always safe to hold: with no provider every embedding
/// request answers "not configured", which callers treat as "search
/// lexically" rather than as a failure.
#[derive(Clone, Default)]
pub struct Embeddings {
    provider: Option<Arc<dyn Embedder>>,
    batch_size: usize,
}

impl std::fmt::Debug for Embeddings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nothing about the provider is printed: its configuration may hold a key.
        f.debug_struct("Embeddings")
            .field("configured", &self.provider.is_some())
            .finish()
    }
}

impl Embeddings {
    /// No provider.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Wrap a provider, sending at most `batch_size` texts per call.
    #[must_use]
    pub fn new(provider: impl Embedder, batch_size: usize) -> Self {
        Self {
            provider: Some(Arc::new(provider)),
            batch_size: batch_size.max(1),
        }
    }

    /// Build the handle `config` describes.
    ///
    /// # Errors
    ///
    /// [`Error::EmbeddingFailed`] if the HTTP client cannot be built.
    pub fn from_config(config: &EmbeddingsConfig) -> Result<Self> {
        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        match config.provider {
            EmbeddingProvider::None => Ok(Self::disabled()),
            EmbeddingProvider::Command => Ok(Self::new(
                command::CommandEmbedder::new(
                    config.command.clone(),
                    config.model.clone(),
                    timeout,
                ),
                config.batch_size,
            )),
            EmbeddingProvider::Http => Ok(Self::new(
                http::HttpEmbedder::new(
                    config.url.clone().unwrap_or_default(),
                    config.model.clone().unwrap_or_default(),
                    config.api_key.clone(),
                    timeout,
                )?,
                config.batch_size,
            )),
        }
    }

    /// Whether a provider is configured.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.provider.is_some()
    }

    /// Embed every text, in order, validating what comes back.
    ///
    /// Texts go to the provider in batches of the configured size, each
    /// truncated to [`MAX_EMBED_CHARS`]. The result is checked once for every
    /// provider: one vector per text, each of the stored dimension and made of
    /// finite numbers. A vector that fails would otherwise be rejected by the
    /// database later, deep inside a job, with a message about an index.
    ///
    /// # Errors
    ///
    /// [`Error::EmbeddingFailed`] when no provider is configured or it fails,
    /// [`Error::EmbeddingDimension`] when it answers with the wrong length.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let Some(provider) = &self.provider else {
            return Err(Error::EmbeddingsNotConfigured);
        };
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.batch_size.max(1)) {
            let truncated: Vec<String> = batch.iter().map(|t| truncate(t)).collect();
            let answered = provider.embed(&truncated).await?;
            if answered.len() != batch.len() {
                return Err(Error::EmbeddingFailed {
                    message: format!(
                        "the provider returned {} vector(s) for {} text(s)",
                        answered.len(),
                        batch.len()
                    ),
                });
            }
            for vector in &answered {
                if vector.len() != EMBEDDING_DIMENSION {
                    return Err(Error::EmbeddingDimension {
                        expected: EMBEDDING_DIMENSION,
                        actual: vector.len(),
                    });
                }
                if vector.iter().any(|x| !x.is_finite()) {
                    return Err(Error::EmbeddingFailed {
                        message: "the provider returned a non-finite number".to_string(),
                    });
                }
            }
            vectors.extend(answered);
        }
        Ok(vectors)
    }

    /// Embed one text.
    ///
    /// # Errors
    ///
    /// As [`Embeddings::embed`].
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut vectors = self.embed(&[text.to_string()]).await?;
        vectors.pop().ok_or_else(|| Error::EmbeddingFailed {
            message: "the provider returned no vector".to_string(),
        })
    }
}

/// `text` cut to [`MAX_EMBED_CHARS`] characters, on a character boundary.
fn truncate(text: &str) -> String {
    text.chars().take(MAX_EMBED_CHARS).collect()
}

/// A deterministic fake provider for tests: the same text always yields the
/// same unit vector, and texts sharing words yield similar vectors.
///
/// Each lower-cased word hashes onto a few of the 768 coordinates, so two
/// texts that share vocabulary have a high cosine similarity — enough to test
/// that semantic ranking follows meaning-by-overlap without a model.
#[cfg(test)]
pub(crate) mod fake {
    use super::{EMBEDDING_DIMENSION, EmbedFuture, Embedder};
    use sha2::{Digest, Sha256};

    /// See the module doc.
    pub(crate) struct WordHashEmbedder;

    /// The vector [`WordHashEmbedder`] derives for `text`.
    pub(crate) fn vector_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; EMBEDDING_DIMENSION];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let digest = Sha256::digest(word.to_lowercase().as_bytes());
            let slot = (usize::from(digest[0]) << 8 | usize::from(digest[1])) % EMBEDDING_DIMENSION;
            v[slot] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        } else {
            // An empty text still needs a valid, non-zero vector for cosine.
            v[0] = 1.0;
        }
        v
    }

    impl Embedder for WordHashEmbedder {
        fn embed<'a>(&'a self, texts: &'a [String]) -> EmbedFuture<'a> {
            Box::pin(async move { Ok(texts.iter().map(|t| vector_for(t)).collect()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns whatever it was built with, whatever it is asked.
    struct Canned(Vec<Vec<f32>>);

    impl Embedder for Canned {
        fn embed<'a>(&'a self, _texts: &'a [String]) -> EmbedFuture<'a> {
            let vectors = self.0.clone();
            Box::pin(async move { Ok(vectors) })
        }
    }

    #[tokio::test]
    async fn embedding_without_a_provider_says_so_instead_of_returning_nothing() {
        let error = Embeddings::disabled()
            .embed_one("hello")
            .await
            .expect_err("no provider");
        assert!(matches!(error, Error::EmbeddingsNotConfigured), "{error}");
    }

    #[tokio::test]
    async fn a_vector_of_the_wrong_length_is_rejected_with_both_lengths() {
        let embeddings = Embeddings::new(Canned(vec![vec![1.0, 2.0]]), 8);
        let error = embeddings.embed_one("x").await.expect_err("wrong length");
        assert!(
            matches!(
                error,
                Error::EmbeddingDimension {
                    expected: EMBEDDING_DIMENSION,
                    actual: 2
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_provider_answering_with_too_few_vectors_is_rejected() {
        let embeddings = Embeddings::new(Canned(vec![]), 8);
        let error = embeddings
            .embed(&["a".to_string()])
            .await
            .expect_err("count mismatch");
        assert!(matches!(error, Error::EmbeddingFailed { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_non_finite_number_is_rejected_before_it_reaches_the_index() {
        let mut vector = vec![0.0; EMBEDDING_DIMENSION];
        vector[0] = f32::NAN;
        let embeddings = Embeddings::new(Canned(vec![vector]), 8);
        assert!(embeddings.embed_one("x").await.is_err());
    }

    #[tokio::test]
    async fn texts_are_sent_in_batches_no_larger_than_the_configured_size() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counting(Arc<AtomicUsize>);
        impl Embedder for Counting {
            fn embed<'a>(&'a self, texts: &'a [String]) -> EmbedFuture<'a> {
                assert!(texts.len() <= 2, "batch of {}", texts.len());
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Ok(texts.iter().map(|t| fake::vector_for(t)).collect()) })
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let embeddings = Embeddings::new(Counting(calls.clone()), 2);
        let texts: Vec<String> = (0..5).map(|i| format!("text {i}")).collect();
        let vectors = embeddings.embed(&texts).await.expect("embeds");
        assert_eq!(vectors.len(), 5);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_text_is_truncated_on_a_character_boundary() {
        let long = "é".repeat(MAX_EMBED_CHARS + 10);
        assert_eq!(truncate(&long).chars().count(), MAX_EMBED_CHARS);
    }

    #[test]
    fn the_debug_form_reveals_nothing_about_the_provider() {
        let text = format!("{:?}", Embeddings::new(fake::WordHashEmbedder, 4));
        assert_eq!(text, "Embeddings { configured: true }");
    }
}
