//! The search abstraction: MemCastle's retrieval contract and ranking policy.
//!
//! Candidate selection, vector search, full-text search, fusion and graph
//! traversal all execute inside SurrealDB (`store::retrieval`); this module
//! decides *which* of them a request needs and combines their answers into
//! the domain's [`SearchHit`]. It owns no index, vector store or ranking
//! engine, so a different backend would change `store`, not this contract.
//!
//! The reason this is its own module rather than `app` calling the store
//! directly is that the interfaces above it never change shape as ranking
//! grows: they hand over a [`SearchQuery`] and get hits back.

use chrono::{DateTime, Utc};

use crate::domain::SourceKind;
use crate::error::{Error, Result};
use crate::store::{MatchMode, SurrealStore};

// Re-exported so the interface layers (notably `client`, which only needs
// the wire types) can name a search request or result without importing
// `crate::store` — which AGENTS.md invariant #1 and the `store-isolation`
// hook forbid them.
pub use crate::domain::{
    EMBEDDING_DIMENSION, RankingMode, SearchFilter, SearchHit, SearchQuery, Signals, Temporal,
};

/// Search options as the text-based interfaces (REST query strings, MCP
/// arguments, CLI flags) receive them, before validation.
///
/// One conversion, [`SearchOptions::into_query`], so a bad `mode`, `as_of` or
/// `source_kind` gets the same diagnostic whichever interface it arrived by.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// The most hits to return; `None` means the service default.
    pub limit: Option<u32>,
    /// Restrict to one wing, by name.
    pub wing: Option<String>,
    /// Restrict to rooms with this name.
    pub room: Option<String>,
    /// A ranking name (see [`RankingMode::NAMES`]); `None` means `auto`.
    pub ranking: Option<String>,
    /// A drawer must carry all of these tags.
    pub tags: Vec<String>,
    /// A source kind name (`file`, `manual`, `transcript`, `note`, `other`).
    pub source_kind: Option<String>,
    /// An RFC 3339 instant: search as the palace stood then.
    pub as_of: Option<String>,
    /// Include superseded memory as well as current.
    pub include_historical: bool,
    /// Enrich the hits through the knowledge graph.
    pub expand: bool,
}

impl SearchOptions {
    /// Validate these options into a [`SearchQuery`] for `text`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] naming the offending option: an unknown mode or
    /// source kind, an `as_of` that is not RFC 3339, or `as_of` together with
    /// `include_historical` (a point in time and "all time" contradict).
    pub fn into_query(self, text: impl Into<String>) -> Result<SearchQuery> {
        let ranking = match self.ranking.as_deref() {
            None => RankingMode::Auto,
            Some(raw) => raw
                .parse()
                .map_err(|message: String| Error::invalid_input("ranking", message))?,
        };
        let source_kind = self
            .source_kind
            .as_deref()
            .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                "file" => Ok(SourceKind::File),
                "manual" => Ok(SourceKind::Manual),
                "transcript" => Ok(SourceKind::Transcript),
                "note" => Ok(SourceKind::Note),
                "other" => Ok(SourceKind::Other),
                _ => Err(Error::invalid_input(
                    "source_kind",
                    format!(
                        "unknown source kind `{raw}`; expected file, manual, transcript, note or other"
                    ),
                )),
            })
            .transpose()?;
        let temporal = match (&self.as_of, self.include_historical) {
            (Some(_), true) => {
                return Err(Error::invalid_input(
                    "as_of",
                    "cannot be combined with include_historical: a point in time already \
                     says which memory is valid",
                ));
            }
            (Some(raw), false) => {
                let at: DateTime<Utc> = DateTime::parse_from_rfc3339(raw.trim())
                    .map_err(|_| {
                        Error::invalid_input(
                            "as_of",
                            format!(
                                "`{raw}` is not an RFC 3339 timestamp such as 2026-01-31T12:00:00Z"
                            ),
                        )
                    })?
                    .with_timezone(&Utc);
                Temporal::AsOf(at)
            }
            (None, true) => Temporal::All,
            (None, false) => Temporal::Current,
        };
        Ok(SearchQuery {
            text: text.into(),
            ranking,
            limit: self.limit.unwrap_or(0),
            filter: SearchFilter {
                wing: self.wing,
                room: self.room,
                tags: self.tags,
                source_kind,
                temporal,
            },
            expand: self.expand,
            query_embedding: None,
        })
    }
}

/// The ranking strategy a query will actually use, once it is known whether a query
/// vector exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// BM25 only.
    Lexical,
    /// Vector similarity only.
    Semantic,
    /// Both, fused.
    Hybrid,
}

/// Decide which ranking serves `mode`, given whether a query vector is
/// available.
///
/// `Auto` degrades to lexical rather than failing, so a palace with no
/// embedding provider behaves exactly as it did before vectors existed. An
/// explicit `Semantic` or `Hybrid` is a promise the caller is relying on, so
/// without a vector it is an error, never a silent lexical answer that looks
/// like a semantic one.
///
/// # Errors
///
/// [`Error::SemanticUnavailable`] for `Semantic` or `Hybrid` without a vector.
pub fn resolve_strategy(mode: RankingMode, has_vector: bool) -> Result<Strategy> {
    match (mode, has_vector) {
        (RankingMode::Lexical, _) | (RankingMode::Auto, false) => Ok(Strategy::Lexical),
        (RankingMode::Auto | RankingMode::Hybrid, true) => Ok(Strategy::Hybrid),
        (RankingMode::Semantic, true) => Ok(Strategy::Semantic),
        (RankingMode::Semantic | RankingMode::Hybrid, false) => Err(Error::SemanticUnavailable {
            ranking: format!("{mode:?}").to_lowercase(),
        }),
    }
}

/// Run `query` for at most `limit` hits, ranking with `vector` when one is
/// supplied and the mode wants it.
///
/// # Errors
///
/// [`Error::SemanticUnavailable`] if the mode needs a vector and there is
/// none, [`Error::EmbeddingDimension`] if the vector has the wrong length,
/// or a store error.
pub async fn search(
    store: &SurrealStore,
    query: &SearchQuery,
    limit: u32,
    vector: Option<&[f32]>,
) -> Result<Vec<SearchHit>> {
    let mut hits = match resolve_strategy(query.ranking, vector.is_some())? {
        Strategy::Lexical => lexical_search(store, &query.text, limit, &query.filter).await?,
        Strategy::Semantic => {
            // `resolve_strategy` returned `Semantic` only with a vector.
            let vector = vector.unwrap_or_default();
            store.search_vector(vector, limit, &query.filter).await?
        }
        Strategy::Hybrid => {
            let vector = vector.unwrap_or_default();
            store
                .search_hybrid(&query.text, vector, limit, &query.filter)
                .await?
        }
    };
    if query.expand {
        // Graph expansion enriches, it never reorders or replaces: the direct
        // hits keep their ranks and the graph-reached drawers follow them, best
        // first, each saying which entities connect it. Up to `limit` more, so
        // a full page of direct hits still gets its related memory.
        let seeds: Vec<_> = hits.iter().map(|hit| hit.drawer.id).collect();
        let related = store.expand_via_graph(&seeds, limit, &query.filter).await?;
        hits.extend(related);
    }
    Ok(hits)
}

/// Search drawer content lexically, returning at most `limit` hits ordered
/// by BM25 relevance within `filter`.
///
/// Every query term must match first, so an exact query stays precise.
/// Only when that finds nothing does it retry with any single term
/// sufficing: SurrealDB has no stop-word filter, so a natural-language
/// query ("programming languages I use preferences") carries words the
/// stored text never contains and would otherwise return nothing at all.
///
/// # Errors
///
/// Returns an error if the underlying store query fails.
pub async fn lexical_search(
    store: &SurrealStore,
    query: &str,
    limit: u32,
    filter: &SearchFilter,
) -> Result<Vec<SearchHit>> {
    let strict = store
        .search_lexical(query, limit, filter, MatchMode::All)
        .await?;
    // A single term (or none) means AND and OR are the same query, so a
    // retry could only repeat the empty answer at the cost of a query.
    if !strict.is_empty() || query.split_whitespace().nth(1).is_none() {
        return Ok(strict);
    }
    store
        .search_lexical(query, limit, filter, MatchMode::Any)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_to_a_current_auto_search() {
        let query = SearchOptions::default().into_query("x").unwrap();
        assert_eq!(query.ranking, RankingMode::Auto);
        assert_eq!(query.filter.temporal, Temporal::Current);
        assert_eq!(query.limit, 0);
    }

    #[test]
    fn a_point_in_time_and_include_historical_are_contradictory() {
        let options = SearchOptions {
            as_of: Some("2026-01-01T00:00:00Z".into()),
            include_historical: true,
            ..Default::default()
        };
        assert!(matches!(
            options.into_query("x"),
            Err(Error::InvalidInput { .. })
        ));
    }

    #[test]
    fn an_as_of_that_is_not_rfc_3339_names_the_option_and_an_example() {
        let options = SearchOptions {
            as_of: Some("yesterday".into()),
            ..Default::default()
        };
        let error = options.into_query("x").unwrap_err().to_string();
        assert!(
            error.contains("as_of") && error.contains("2026-01-31"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_mode_or_source_kind_is_rejected() {
        let mode = SearchOptions {
            ranking: Some("fuzzy".into()),
            ..Default::default()
        };
        assert!(mode.into_query("x").is_err());
        let kind = SearchOptions {
            source_kind: Some("web".into()),
            ..Default::default()
        };
        assert!(kind.into_query("x").is_err());
    }

    #[test]
    fn include_historical_searches_all_time() {
        let options = SearchOptions {
            include_historical: true,
            ..Default::default()
        };
        assert_eq!(
            options.into_query("x").unwrap().filter.temporal,
            Temporal::All
        );
    }

    #[test]
    fn auto_without_a_vector_falls_back_to_lexical() {
        assert_eq!(
            resolve_strategy(RankingMode::Auto, false).unwrap(),
            Strategy::Lexical
        );
    }

    #[test]
    fn auto_with_a_vector_is_hybrid() {
        assert_eq!(
            resolve_strategy(RankingMode::Auto, true).unwrap(),
            Strategy::Hybrid
        );
    }

    #[test]
    fn an_explicit_semantic_or_hybrid_without_a_vector_is_an_error_not_a_silent_fallback() {
        for mode in [RankingMode::Semantic, RankingMode::Hybrid] {
            let error = resolve_strategy(mode, false).unwrap_err();
            assert!(
                matches!(error, Error::SemanticUnavailable { .. }),
                "{mode:?}: {error}"
            );
        }
    }

    #[test]
    fn an_explicit_lexical_ignores_an_available_vector() {
        assert_eq!(
            resolve_strategy(RankingMode::Lexical, true).unwrap(),
            Strategy::Lexical
        );
    }
}
