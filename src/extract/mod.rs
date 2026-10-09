//! Entity extraction: reading text and finding the entities and relationships in it, kept apart from storage.
//!
//! What an extractor finds is *derived* data. This module is how MemCastle obtains it — from a small built-in
//! heuristic, a program the operator provides, an OpenAI-compatible chat endpoint, or not at all — and nothing here
//! touches storage: [`job`] writes what comes back through the same store operations a checkpoint uses
//! (docs/adr/024). No model is linked into MemCastle, and a `command` provider owns its own credentials.
//!
//! Whatever a provider says is held to one closed vocabulary ([`crate::domain::EntityKind`],
//! [`crate::domain::Predicate`]) and to size bounds, centrally in [`Extraction::extract`], so a provider can stay
//! dumb and a chatty model cannot fragment the graph.

pub mod command;
pub mod heuristic;
pub mod http;
pub mod job;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;

use crate::config::{ExtractionConfig, ExtractionProvider};
use crate::domain::{
    EntityKind, ExtractedEntity, ExtractedGraph, ExtractedRelation, Limits, Predicate,
    PreferenceMatch,
};
use crate::error::{Error, Result};

/// The most characters of one text sent to a provider.
///
/// Mining stores chunks of at most a few thousand characters, but a drawer written another way can hold 256 KiB and
/// a provider caps its input in tokens. The head of a text carries its subject; only what is extracted is computed
/// from the truncated text, the drawer's content is untouched.
pub const MAX_EXTRACT_CHARS: usize = 8_000;

/// The future an [`Extractor`] returns.
pub type ExtractFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<ExtractedGraph>>> + Send + 'a>>;

/// Something that reads texts and says what entities and relationships they hold.
///
/// One call handles a batch and returns one graph per text, in order. Implementations parse into the closed
/// vocabulary but need not bound sizes or check endpoints; [`Extraction`] does, once, for every provider.
pub trait Extractor: Send + Sync + 'static {
    /// The name recorded as the extractor of every fact it yields.
    fn name(&self) -> &'static str;

    /// Extract from `texts`.
    fn extract<'a>(&'a self, texts: &'a [String]) -> ExtractFuture<'a>;

    /// Implementations that can consume per-text evidence context override this; others still see the same text.
    fn extract_with_preferences<'a>(
        &'a self,
        texts: &'a [String],
        _context: &'a [PreferenceMatch],
    ) -> ExtractFuture<'a> {
        self.extract(texts)
    }
}

/// The daemon's extraction handle: a provider, or nothing.
///
/// Cheap to clone and always safe to hold: with no provider every request answers "not configured", which the
/// scheduler treats as "do not queue an extraction sweep" rather than as a failure.
#[derive(Clone)]
pub struct Extraction {
    provider: Option<Arc<dyn Extractor>>,
    batch_size: usize,
    limits: Limits,
}

impl Default for Extraction {
    fn default() -> Self {
        Self::disabled()
    }
}

impl std::fmt::Debug for Extraction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nothing about the provider is printed: its configuration may hold a key.
        f.debug_struct("Extraction")
            .field("configured", &self.provider.is_some())
            .finish()
    }
}

impl Extraction {
    /// No provider.
    #[must_use]
    pub fn disabled() -> Self {
        let defaults = ExtractionConfig::default();
        Self {
            provider: None,
            batch_size: defaults.batch_size,
            limits: limits_of(&defaults),
        }
    }

    /// Wrap a provider with the bounds in `config` (its `provider` field is not consulted).
    #[must_use]
    pub fn new(provider: impl Extractor, config: &ExtractionConfig) -> Self {
        Self {
            provider: Some(Arc::new(provider)),
            batch_size: config.batch_size.max(1),
            limits: limits_of(config),
        }
    }

    /// Build the handle `config` describes.
    ///
    /// # Errors
    ///
    /// [`Error::ExtractionFailed`] if the HTTP client cannot be built.
    pub fn from_config(config: &ExtractionConfig) -> Result<Self> {
        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        Ok(match config.provider {
            ExtractionProvider::None => Self::disabled(),
            ExtractionProvider::Heuristic => Self::new(heuristic::HeuristicExtractor, config),
            ExtractionProvider::Command => Self::new(
                command::CommandExtractor::new(
                    config.command.clone(),
                    config.model.clone(),
                    timeout,
                ),
                config,
            ),
            ExtractionProvider::Http => Self::new(
                http::HttpExtractor::new(
                    config.url.clone().unwrap_or_default(),
                    config.model.clone().unwrap_or_default(),
                    config.api_key.clone(),
                    timeout,
                )?,
                config,
            ),
        })
    }

    /// Whether a provider is configured.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.provider.is_some()
    }

    /// How many drawers one pass reads.
    #[must_use]
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// The configured provider's name, for provenance (`None` when disabled).
    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        self.provider.as_ref().map(|provider| provider.name())
    }

    /// Extract from every text, in order, holding what comes back to the vocabulary and the bounds.
    ///
    /// Texts go to the provider in batches of the configured size, each truncated to [`MAX_EXTRACT_CHARS`]. The
    /// answer must hold one graph per text; each is then normalised ([`ExtractedGraph::normalise`]).
    ///
    /// # Errors
    ///
    /// [`Error::ExtractionNotConfigured`] without a provider, [`Error::ExtractionFailed`] when it fails or answers
    /// with the wrong number of graphs.
    pub async fn extract(&self, texts: &[String]) -> Result<Vec<ExtractedGraph>> {
        self.extract_with_preferences(texts, &[]).await
    }

    /// Pass provenance context alongside the text without changing the stored drawer or provider confidence.
    pub async fn extract_with_preferences(
        &self,
        texts: &[String],
        context: &[PreferenceMatch],
    ) -> Result<Vec<ExtractedGraph>> {
        let Some(provider) = &self.provider else {
            return Err(Error::ExtractionNotConfigured);
        };
        let mut graphs = Vec::with_capacity(texts.len());
        for (index, batch) in texts.chunks(self.batch_size.max(1)).enumerate() {
            let truncated: Vec<String> = batch.iter().map(|text| truncate(text)).collect();
            let start = index * self.batch_size.max(1);
            let matching = context.get(start..start + batch.len()).unwrap_or(&[]);
            let answered = provider
                .extract_with_preferences(&truncated, matching)
                .await?;
            if answered.len() != batch.len() {
                return Err(Error::ExtractionFailed {
                    message: format!(
                        "the provider returned {} extraction(s) for {} text(s)",
                        answered.len(),
                        batch.len()
                    ),
                });
            }
            graphs.extend(
                answered
                    .into_iter()
                    .map(|graph| graph.normalise(self.limits)),
            );
        }
        Ok(graphs)
    }
}

/// The bounds a config asks for.
fn limits_of(config: &ExtractionConfig) -> Limits {
    Limits {
        max_entities: config.max_entities,
        max_relations: config.max_relations,
        min_confidence: config.min_confidence,
    }
}

/// `text` cut to [`MAX_EXTRACT_CHARS`] characters, on a character boundary.
fn truncate(text: &str) -> String {
    text.chars().take(MAX_EXTRACT_CHARS).collect()
}

/// The JSON shape `command` and `http` providers answer in, before it is read into the closed vocabulary.
///
/// Every field is optional or defaulted: a model that omits an empty list, or a confidence, is not wrong.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireGraph {
    #[serde(default)]
    entities: Vec<WireEntity>,
    #[serde(default)]
    relations: Vec<WireRelation>,
}

#[derive(Debug, Deserialize)]
struct WireEntity {
    name: String,
    #[serde(default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct WireRelation {
    subject: String,
    #[serde(default)]
    predicate: String,
    object: String,
    /// A model that gives no confidence is taken at a middling one rather than rejected.
    #[serde(default = "default_confidence")]
    confidence: f32,
}

fn default_confidence() -> f32 {
    0.5
}

impl WireGraph {
    /// Read the wire shape into the closed vocabulary: an unknown kind becomes `other`, an unknown predicate
    /// `related_to`.
    pub(crate) fn into_graph(self) -> ExtractedGraph {
        ExtractedGraph {
            entities: self
                .entities
                .into_iter()
                .map(|entity| ExtractedEntity {
                    name: entity.name,
                    kind: EntityKind::parse(&entity.kind),
                })
                .collect(),
            relations: self
                .relations
                .into_iter()
                .map(|relation| ExtractedRelation {
                    subject: relation.subject,
                    predicate: Predicate::parse(&relation.predicate),
                    object: relation.object,
                    confidence: relation.confidence,
                })
                .collect(),
        }
    }
}

/// A provider for tests: answers whatever graph it was built with for every text, or fails.
#[cfg(test)]
pub(crate) mod fake {
    use super::{ExtractFuture, Extractor};
    use crate::domain::ExtractedGraph;
    use crate::error::Error;

    /// Answers `graph` for every text.
    pub(crate) struct Canned(pub(crate) ExtractedGraph);

    impl Extractor for Canned {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn extract<'a>(&'a self, texts: &'a [String]) -> ExtractFuture<'a> {
            Box::pin(async move { Ok(texts.iter().map(|_| self.0.clone()).collect()) })
        }
    }

    /// Always fails.
    pub(crate) struct Broken;

    impl Extractor for Broken {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn extract<'a>(&'a self, _texts: &'a [String]) -> ExtractFuture<'a> {
            Box::pin(async move {
                Err(Error::ExtractionFailed {
                    message: "the provider is down".to_string(),
                })
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ExtractionConfig {
        ExtractionConfig::default()
    }

    #[tokio::test]
    async fn extracting_without_a_provider_says_so_instead_of_returning_nothing() {
        let error = Extraction::disabled()
            .extract(&["x".to_string()])
            .await
            .expect_err("no provider");
        assert!(matches!(error, Error::ExtractionNotConfigured), "{error}");
    }

    #[tokio::test]
    async fn a_provider_answering_with_too_few_graphs_is_rejected() {
        struct Short;
        impl Extractor for Short {
            fn name(&self) -> &'static str {
                "short"
            }
            fn extract<'a>(&'a self, _texts: &'a [String]) -> ExtractFuture<'a> {
                Box::pin(async { Ok(vec![]) })
            }
        }
        let extraction = Extraction::new(Short, &config());
        let error = extraction
            .extract(&["a".to_string()])
            .await
            .expect_err("count mismatch");
        assert!(matches!(error, Error::ExtractionFailed { .. }), "{error}");
    }

    #[tokio::test]
    async fn what_a_provider_returns_is_held_to_the_bounds_before_anyone_sees_it() {
        let graph = ExtractedGraph {
            entities: vec![
                ExtractedEntity {
                    name: "  ".into(),
                    kind: EntityKind::Other,
                },
                ExtractedEntity {
                    name: "Ada".into(),
                    kind: EntityKind::Person,
                },
            ],
            relations: vec![ExtractedRelation {
                subject: "Ada".into(),
                predicate: Predicate::WorksOn,
                object: "Nobody".into(),
                confidence: 0.9,
            }],
        };
        let extraction = Extraction::new(fake::Canned(graph), &config());
        let out = extraction.extract(&["x".to_string()]).await.unwrap();
        assert_eq!(out[0].entities.len(), 1);
        assert!(out[0].relations.is_empty(), "the object is not an entity");
    }

    #[tokio::test]
    async fn texts_are_sent_in_batches_no_larger_than_the_configured_size() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counting(Arc<AtomicUsize>);
        impl Extractor for Counting {
            fn name(&self) -> &'static str {
                "counting"
            }
            fn extract<'a>(&'a self, texts: &'a [String]) -> ExtractFuture<'a> {
                assert!(texts.len() <= 2, "batch of {}", texts.len());
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(
                    async move { Ok(texts.iter().map(|_| ExtractedGraph::default()).collect()) },
                )
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let extraction = Extraction::new(
            Counting(calls.clone()),
            &ExtractionConfig {
                batch_size: 2,
                ..config()
            },
        );
        let texts: Vec<String> = (0..5).map(|i| format!("text {i}")).collect();
        assert_eq!(extraction.extract(&texts).await.unwrap().len(), 5);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_text_is_truncated_on_a_character_boundary() {
        let long = "é".repeat(MAX_EXTRACT_CHARS + 10);
        assert_eq!(truncate(&long).chars().count(), MAX_EXTRACT_CHARS);
    }

    #[test]
    fn the_debug_form_reveals_nothing_about_the_provider() {
        let text = format!(
            "{:?}",
            Extraction::new(heuristic::HeuristicExtractor, &config())
        );
        assert_eq!(text, "Extraction { configured: true }");
    }

    #[test]
    fn a_wire_answer_outside_the_vocabulary_is_read_as_other_and_related_to() {
        let wire: WireGraph = serde_json::from_str(
            r#"{"entities":[{"name":"Ada","kind":"Human"},{"name":"Rust"}],
                "relations":[{"subject":"Ada","predicate":"is fond of","object":"Rust"}]}"#,
        )
        .unwrap();
        let graph = wire.into_graph();
        assert_eq!(graph.entities[0].kind, EntityKind::Other);
        assert_eq!(graph.relations[0].predicate, Predicate::RelatedTo);
        assert!((graph.relations[0].confidence - 0.5).abs() < f32::EPSILON);
    }
}
