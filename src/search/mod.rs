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

use chrono::{DateTime, NaiveDate, Utc};

use crate::domain::{SourceKind, SourcePreferences};
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
    /// An RFC 3339 instant or a `YYYY-MM-DD` date: search as the palace stood then.
    pub as_of: Option<String>,
    /// The inclusive start of an interval (same forms as `as_of`): search what
    /// was valid at some moment of `[from, until)`. Needs `until`.
    pub from: Option<String>,
    /// The exclusive end of an interval. Needs `from`.
    pub until: Option<String>,
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
        let temporal = self.temporal()?;
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

    /// The temporal constraint these options ask for.
    ///
    /// The four ways of saying it (nothing, `as_of`, `from` and `until`,
    /// `include_historical`) are mutually exclusive: each already says which
    /// memory is valid, so combining two would silently let one win.
    fn temporal(&self) -> Result<Temporal> {
        let window = self.from.is_some() || self.until.is_some();
        let chosen = [
            ("as_of", self.as_of.is_some()),
            ("from", window),
            ("include_historical", self.include_historical),
        ];
        let named: Vec<&str> = chosen
            .iter()
            .filter(|(_, set)| *set)
            .map(|(name, _)| *name)
            .collect();
        if let [first, second, ..] = named.as_slice() {
            let (first, second) = (*first, *second);
            return Err(Error::invalid_input(
                first,
                format!(
                    "cannot be combined with {second}: each one already says which memory is \
                     valid, so use only one of `as_of`, `from` with `until`, or `include_historical`"
                ),
            ));
        }
        if let Some(raw) = &self.as_of {
            return Ok(Temporal::AsOf(parse_instant("as_of", raw)?));
        }
        match (&self.from, &self.until) {
            (Some(from), Some(until)) => {
                let from = parse_instant("from", from)?;
                let until = parse_instant("until", until)?;
                Temporal::between(from, until)
                    .map_err(|message| Error::invalid_input("from", message))
            }
            (Some(_), None) => Err(Error::invalid_input(
                "until",
                "an interval needs both ends: add `until` (exclusive), or use `as_of` for a single \
                 point in time",
            )),
            (None, Some(_)) => Err(Error::invalid_input(
                "from",
                "an interval needs both ends: add `from` (inclusive), or use `as_of` for a single \
                 point in time",
            )),
            (None, None) if self.include_historical => Ok(Temporal::All),
            (None, None) => Ok(Temporal::Current),
        }
    }
}

/// Parse a point in time: an RFC 3339 timestamp, or a bare `YYYY-MM-DD` date
/// meaning midnight UTC at the start of that day.
///
/// A date is accepted so `--as-of 2026-01-01` works as written, and because
/// midnight at the start of the day makes `--from 2026-01-01 --until 2026-02-01`
/// exactly January. `option` names the offending input in the diagnostic.
///
/// # Errors
///
/// [`Error::InvalidInput`] naming `option` and showing both accepted forms.
pub fn parse_instant(option: &str, raw: &str) -> Result<DateTime<Utc>> {
    let text = raw.trim();
    if let Ok(instant) = DateTime::parse_from_rfc3339(text) {
        return Ok(instant.with_timezone(&Utc));
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d")
        && let Some(midnight) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(midnight.and_utc());
    }
    Err(Error::invalid_input(
        option,
        format!(
            "`{raw}` is not an RFC 3339 timestamp such as 2026-01-31T12:00:00Z \
             or a date such as 2026-01-31"
        ),
    ))
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
    preferences: &SourcePreferences,
) -> Result<Vec<SearchHit>> {
    // Keep enough relevant candidates that a preference can lift a near miss without letting an unrelated memory
    // enter the results. The original relevance leg still chooses what is eligible.
    let pool = if preferences.sources.is_empty() && preferences.default == Default::default() {
        limit
    } else {
        limit.saturating_mul(4).clamp(20, 200)
    };
    let mut hits = match resolve_strategy(query.ranking, vector.is_some())? {
        Strategy::Lexical => lexical_search(store, &query.text, pool, &query.filter).await?,
        Strategy::Semantic => {
            // `resolve_strategy` returned `Semantic` only with a vector.
            let vector = vector.unwrap_or_default();
            store.search_vector(vector, pool, &query.filter).await?
        }
        Strategy::Hybrid => {
            let vector = vector.unwrap_or_default();
            store
                .search_hybrid(&query.text, vector, pool, &query.filter)
                .await?
        }
    };
    rank_preferred(&mut hits, preferences);
    hits.truncate(limit as usize);
    if query.expand {
        // Graph expansion enriches, it never reorders or replaces: the direct
        // hits keep their ranks and the graph-reached drawers follow them, best
        // first, each saying which entities connect it. Up to `limit` more, so
        // a full page of direct hits still gets its related memory.
        let seeds: Vec<_> = hits.iter().map(|hit| hit.drawer.id).collect();
        let mut related = store.expand_via_graph(&seeds, pool, &query.filter).await?;
        rank_preferred(&mut related, preferences);
        related.truncate(limit as usize);
        hits.extend(related);
    }
    Ok(hits)
}

/// Normalize relevance within the candidate page before adding bounded source and freshness signals.
fn rank_preferred(hits: &mut [SearchHit], preferences: &SourcePreferences) {
    if hits.is_empty()
        || (preferences.sources.is_empty() && preferences.default == Default::default())
    {
        return;
    }
    let high = hits
        .iter()
        .map(|hit| hit.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let low = hits
        .iter()
        .map(|hit| hit.score)
        .fold(f32::INFINITY, f32::min);
    // A microscopic BM25/RRF difference between equally matching documents must not become a whole point of
    // normalized relevance. Anchor the scale to the top score, so authority can break *near* ties, never large gaps.
    let range = (high - low).max(high.abs().max(low.abs()).max(0.01) * 0.5);
    let now = chrono::Utc::now();
    for hit in hits.iter_mut() {
        let matched = preferences.resolve(hit.drawer.source.origin.as_ref());
        let occurred_at = hit
            .drawer
            .source
            .origin
            .as_ref()
            .and_then(|origin| origin.occurred_at)
            .unwrap_or(hit.drawer.valid_from);
        let age = (now - occurred_at).num_days().max(0) as f32;
        let freshness = 0.12 / (1.0 + age / 30.0);
        // Original per-leg scores remain in `signals`; a bounded normalized score is comparable within this page.
        hit.score = (hit.score - low) / range + freshness + matched.level.adjustment();
        hit.signals.preference = Some(matched);
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.drawer.id.to_string().cmp(&b.drawer.id.to_string()))
    });
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
    use crate::domain::{Drawer, DrawerId, PreferenceLevel, Provenance, RoomId, Source};

    #[test]
    fn a_stale_preferred_hit_does_not_defeat_fresher_evidence_with_equal_relevance() {
        let make = |source: &str, age: i64| {
            let mut drawer = Drawer::new(
                DrawerId::new(),
                RoomId::new(),
                "same terms".into(),
                Source::new(SourceKind::File, None, None),
                Vec::new(),
                Provenance {
                    requested_by: "http".into(),
                    job_id: None,
                },
            );
            drawer.source.origin = Some(crate::domain::Origin {
                source_id: crate::domain::SourceId::new(),
                source: source.into(),
                document: "d".into(),
                chunk: 0,
                revision: "r".into(),
                metadata: None,
                occurred_at: Some(chrono::Utc::now() - chrono::Duration::days(age)),
            });
            SearchHit {
                drawer,
                score: 1.0,
                signals: Signals::default(),
                via: Vec::new(),
            }
        };
        let mut policy = SourcePreferences::default();
        policy.sources.insert(
            "favored".into(),
            crate::domain::ConnectorPreference {
                level: Some(PreferenceLevel::High),
                ..Default::default()
            },
        );
        let mut hits = vec![make("favored", 365), make("ordinary", 0)];
        rank_preferred(&mut hits, &policy);
        assert_eq!(
            hits[0].drawer.source.origin.as_ref().unwrap().source,
            "ordinary"
        );
        assert_eq!(
            hits[1].signals.preference.as_ref().unwrap().level,
            PreferenceLevel::High
        );
    }

    #[test]
    fn a_preferred_source_wins_a_near_tie_but_not_a_substantial_relevance_gap() {
        let hit = |source: &str, score| {
            let mut drawer = Drawer::new(
                DrawerId::new(),
                RoomId::new(),
                "same terms".into(),
                Source::new(SourceKind::File, None, None),
                Vec::new(),
                Provenance {
                    requested_by: "http".into(),
                    job_id: None,
                },
            );
            drawer.source.origin = Some(crate::domain::Origin {
                source_id: crate::domain::SourceId::new(),
                source: source.into(),
                document: "d".into(),
                chunk: 0,
                revision: "r".into(),
                metadata: None,
                occurred_at: None,
            });
            SearchHit {
                drawer,
                score,
                signals: Signals::default(),
                via: Vec::new(),
            }
        };
        let mut policy = SourcePreferences::default();
        policy.sources.insert(
            "favored".into(),
            crate::domain::ConnectorPreference {
                level: Some(PreferenceLevel::High),
                ..Default::default()
            },
        );
        let mut close = vec![hit("ordinary", 1.0), hit("favored", 0.999_999_9)];
        rank_preferred(&mut close, &policy);
        assert_eq!(
            close[0].drawer.source.origin.as_ref().unwrap().source,
            "favored"
        );

        let mut far = vec![hit("ordinary", 1.0), hit("favored", 0.1)];
        rank_preferred(&mut far, &policy);
        assert_eq!(
            far[0].drawer.source.origin.as_ref().unwrap().source,
            "ordinary"
        );
    }

    #[test]
    fn a_neutral_policy_preserves_original_ranking_scores() {
        let drawer = Drawer::new(
            DrawerId::new(),
            RoomId::new(),
            "test".into(),
            Source::new(SourceKind::Manual, None, None),
            Vec::new(),
            Provenance {
                requested_by: "http".into(),
                job_id: None,
            },
        );
        let mut hits = vec![SearchHit {
            drawer,
            score: 4.2,
            signals: Signals {
                lexical: Some(4.2),
                ..Default::default()
            },
            via: Vec::new(),
        }];
        rank_preferred(&mut hits, &SourcePreferences::default());
        assert_eq!(hits[0].score, 4.2);
        assert!(hits[0].signals.preference.is_none());
    }

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

    fn at(raw: &str) -> DateTime<Utc> {
        parse_instant("as_of", raw).unwrap()
    }

    #[test]
    fn a_bare_date_means_midnight_utc_at_the_start_of_that_day() {
        assert_eq!(at("2026-01-01"), at("2026-01-01T00:00:00Z"));
        let options = SearchOptions {
            as_of: Some(" 2026-01-01 ".into()),
            ..Default::default()
        };
        assert_eq!(
            options.into_query("x").unwrap().filter.temporal,
            Temporal::AsOf(at("2026-01-01T00:00:00Z"))
        );
    }

    #[test]
    fn an_offset_timestamp_is_normalised_to_utc() {
        assert_eq!(at("2026-01-01T02:00:00+02:00"), at("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn a_from_and_until_ask_for_the_interval_between_them() {
        let options = SearchOptions {
            from: Some("2026-01-01".into()),
            until: Some("2026-02-01".into()),
            ..Default::default()
        };
        assert_eq!(
            options.into_query("x").unwrap().filter.temporal,
            Temporal::Between {
                from: at("2026-01-01"),
                until: at("2026-02-01"),
            }
        );
    }

    #[test]
    fn an_interval_with_only_one_end_says_which_end_is_missing() {
        let only_from = SearchOptions {
            from: Some("2026-01-01".into()),
            ..Default::default()
        };
        let error = only_from.into_query("x").unwrap_err().to_string();
        assert!(error.contains("until"), "{error}");
        let only_until = SearchOptions {
            until: Some("2026-01-01".into()),
            ..Default::default()
        };
        let error = only_until.into_query("x").unwrap_err().to_string();
        assert!(error.contains("from"), "{error}");
    }

    #[test]
    fn an_empty_or_reversed_interval_is_rejected() {
        for (from, until) in [("2026-02-01", "2026-01-01"), ("2026-01-01", "2026-01-01")] {
            let options = SearchOptions {
                from: Some(from.into()),
                until: Some(until.into()),
                ..Default::default()
            };
            assert!(
                matches!(options.into_query("x"), Err(Error::InvalidInput { .. })),
                "{from}..{until}"
            );
        }
    }

    #[test]
    fn an_interval_cannot_be_combined_with_a_point_or_all_time() {
        let window = |extra: fn(&mut SearchOptions)| {
            let mut options = SearchOptions {
                from: Some("2026-01-01".into()),
                until: Some("2026-02-01".into()),
                ..Default::default()
            };
            extra(&mut options);
            options.into_query("x")
        };
        assert!(window(|o| o.as_of = Some("2026-01-15".into())).is_err());
        assert!(window(|o| o.include_historical = true).is_err());
    }

    #[test]
    fn a_bad_interval_end_names_the_option_that_is_wrong() {
        let options = SearchOptions {
            from: Some("2026-01-01".into()),
            until: Some("soon".into()),
            ..Default::default()
        };
        let error = options.into_query("x").unwrap_err().to_string();
        assert!(error.contains("until"), "{error}");
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
