//! The retrieval contract: what a caller may ask for and what comes back.
//!
//! Pure types, so the interfaces (`api`, `mcp`, `client`) can speak retrieval
//! without ever naming a SurrealDB type, and a backend change stays inside
//! `store`. Ranking itself runs in the database; see `crate::search`.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{Drawer, SourceKind};

/// The length of every stored embedding.
///
/// Fixed because the vector index declares its dimension in
/// `database/schema/palace.surql` (DDL may not live in Rust), so one number
/// has to be agreed by the schema, the embedding providers and the callers
/// supplying their own vectors. A unit test keeps this constant equal to the
/// schema's `DIMENSION`.
pub const EMBEDDING_DIMENSION: usize = 768;

/// How a search ranks drawers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingMode {
    /// Hybrid when the query can be embedded, lexical otherwise.
    ///
    /// The default, so a palace without an embedding provider keeps working
    /// exactly as before instead of failing every search.
    #[default]
    Auto,
    /// BM25 full-text only.
    Lexical,
    /// Vector similarity only; an error if the query cannot be embedded.
    Semantic,
    /// Lexical and vector results fused by reciprocal rank; an error if the
    /// query cannot be embedded.
    Hybrid,
}

impl RankingMode {
    /// Every spelling [`FromStr`] accepts, for CLI help and error messages.
    pub const NAMES: [&'static str; 4] = ["auto", "lexical", "semantic", "hybrid"];
}

impl FromStr for RankingMode {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "lexical" => Ok(Self::Lexical),
            "semantic" => Ok(Self::Semantic),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(format!(
                "unknown ranking `{other}`; expected one of: {}",
                Self::NAMES.join(", ")
            )),
        }
    }
}

/// Which time a search looks at: a point, a window, or all of it.
///
/// Every variant is about *validity time* (`valid_from`/`valid_to`: when the
/// knowledge was true), never *record time* (`created_at`/`updated_at`: when
/// MemCastle learned it). A memory discovered long after the period it
/// describes is found by asking about that period.
///
/// Validity is the half-open range `[valid_from, valid_to)`, with no
/// `valid_to` meaning open-ended. A drawer or edge is *valid at* `t` when
/// `valid_from <= t` and it has no `valid_to` or `valid_to > t`: the end is
/// exclusive, so a record superseded at `t` is gone at `t` and its replacement
/// (valid from `t`) takes over without a moment where both or neither match.
///
/// It *overlaps* the window `[from, until)` when `valid_from < until` and it
/// has no `valid_to` or `valid_to > from`. Touching is not overlapping: a
/// record closed exactly at `from`, or opened exactly at `until`, is excluded.
/// A point `t` is the window `[t, t + 1ns)`, which is why one predicate serves
/// both and they cannot drift apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temporal {
    /// Valid right now. The default: superseded memory is not recalled
    /// unless asked for.
    #[default]
    Current,
    /// Valid at this instant (point-in-time retrieval).
    AsOf(DateTime<Utc>),
    /// Valid at some moment of the window `[from, until)`: its validity
    /// overlaps the window (interval retrieval).
    Between {
        /// The inclusive start of the window.
        from: DateTime<Utc>,
        /// The exclusive end of the window; later than `from`.
        until: DateTime<Utc>,
    },
    /// Every record regardless of validity: current and historical.
    All,
}

impl Temporal {
    /// The window `[from, until)`, or an explanation of why it is empty.
    ///
    /// # Errors
    ///
    /// A message when `from` is not strictly before `until`: such a window
    /// contains no instant, so every search over it would silently match
    /// nothing.
    pub fn between(from: DateTime<Utc>, until: DateTime<Utc>) -> Result<Self, String> {
        Self::Between { from, until }.checked()
    }

    /// This value, or an explanation of why it cannot be searched.
    ///
    /// The one check every entry point runs, including the ones that
    /// deserialise a [`Temporal`] straight from JSON and so never pass
    /// through [`Temporal::between`].
    ///
    /// # Errors
    ///
    /// A message when a window's `from` is not strictly before its `until`.
    pub fn checked(self) -> Result<Self, String> {
        match self {
            Self::Between { from, until } if from >= until => Err(format!(
                "the interval is empty: `from` ({}) must be before `until` ({})",
                from.to_rfc3339(),
                until.to_rfc3339()
            )),
            other => Ok(other),
        }
    }
}

/// The constraints every retrieval leg shares.
///
/// One struct so lexical, semantic and graph legs cannot drift apart: each is
/// handed the same filter and the database applies it before ranking.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchFilter {
    /// Restrict to one wing, by name.
    pub wing: Option<String>,
    /// Restrict to rooms with this name.
    pub room: Option<String>,
    /// A drawer must carry every one of these tags.
    pub tags: Vec<String>,
    /// Restrict to drawers from one kind of source.
    pub source_kind: Option<SourceKind>,
    /// The point in time to look at.
    pub temporal: Temporal,
}

/// One retrieval request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchQuery {
    /// What to look for, in the caller's words.
    ///
    /// `query` is accepted as an alias: MCP, the CLI and the `GET` form of this route all call it that, and with the
    /// whole struct defaulted a body that said `query` would otherwise search for nothing without saying so.
    #[serde(alias = "query")]
    pub text: String,
    /// How to rank.
    pub ranking: RankingMode,
    /// The most hits to return; `0` means the service default.
    pub limit: u32,
    /// The shared scope.
    pub filter: SearchFilter,
    /// Also pull in drawers related to the hits through the knowledge graph.
    pub expand: bool,
    /// A vector for `text`, computed by the caller.
    ///
    /// Lets a client with its own model search semantically against a daemon
    /// that has no embedding provider configured.
    pub query_embedding: Option<Vec<f32>>,
}

impl SearchQuery {
    /// A plain query for `text`: every default, no scope.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// The per-leg evidence behind a hit's score.
///
/// Additive to the wire shape: a client that only reads `score` is
/// unaffected, and one that wants to explain a ranking can read this.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    /// The BM25 score, when the full-text leg matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical: Option<f32>,
    /// Cosine similarity to the query (1 is identical), when the vector leg
    /// matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<f32>,
    /// How many knowledge-graph paths link this drawer to the other hits,
    /// when it was reached through the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<f32>,
}

impl Signals {
    /// Whether no leg contributed (nothing worth serialising).
    pub fn is_empty(&self) -> bool {
        self.lexical.is_none() && self.semantic.is_none() && self.graph.is_none()
    }
}

/// One retrieval result: the drawer, verbatim, plus how it ranked.
///
/// The drawer is always the canonical record; scores and signals are derived
/// and never replace its content.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchHit {
    /// The matching drawer.
    #[serde(flatten)]
    pub drawer: Drawer,
    /// Relevance, higher is better. Its meaning depends on the ranking used:
    /// BM25 for lexical, cosine similarity for semantic, reciprocal-rank
    /// fusion for hybrid. Comparable only within one response.
    pub score: f32,
    /// Which legs matched and how strongly.
    #[serde(default, skip_serializing_if = "Signals::is_empty")]
    pub signals: Signals,
    /// The entities that linked this hit to the others, when it was reached
    /// through graph expansion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ranking_mode_round_trips_through_its_documented_names() {
        for name in RankingMode::NAMES {
            let mode: RankingMode = name.parse().expect("documented name parses");
            let json = serde_json::to_value(mode).expect("serialises");
            assert_eq!(json, name, "wire spelling must equal the CLI spelling");
        }
    }

    #[test]
    fn an_unknown_ranking_mode_names_the_valid_ones() {
        let error = "fuzzy".parse::<RankingMode>().unwrap_err();
        assert!(error.contains("hybrid"), "got: {error}");
    }

    #[test]
    fn a_query_with_only_text_deserialises_to_defaults() {
        let query: SearchQuery = serde_json::from_str(r#"{"text":"hello"}"#).expect("parses");
        assert_eq!(query, SearchQuery::new("hello"));
        assert_eq!(query.filter.temporal, Temporal::Current);
    }

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn an_interval_round_trips_through_its_documented_wire_form() {
        let temporal = Temporal::between(at("2026-01-01T00:00:00Z"), at("2026-02-01T00:00:00Z"))
            .expect("a forward window is valid");
        let json = serde_json::to_value(temporal).expect("serialises");
        assert_eq!(
            json,
            serde_json::json!({"between": {
                "from": "2026-01-01T00:00:00Z",
                "until": "2026-02-01T00:00:00Z",
            }})
        );
        let back: Temporal = serde_json::from_value(json).expect("parses");
        assert_eq!(back, temporal);
    }

    #[test]
    fn an_empty_or_reversed_interval_is_refused_with_both_bounds_named() {
        let t = at("2026-01-01T00:00:00Z");
        let equal = Temporal::between(t, t).unwrap_err();
        assert!(equal.contains("before"), "got: {equal}");
        let reversed = Temporal::between(at("2026-02-01T00:00:00Z"), t).unwrap_err();
        assert!(reversed.contains("2026-02-01"), "got: {reversed}");
    }

    #[test]
    fn an_interval_deserialised_from_json_is_still_checked() {
        // `POST /api/search` takes a `SearchQuery` verbatim, so the check
        // cannot live only in the constructor.
        let raw = r#"{"between":{"from":"2026-02-01T00:00:00Z","until":"2026-01-01T00:00:00Z"}}"#;
        let temporal: Temporal = serde_json::from_str(raw).expect("the shape parses");
        assert!(temporal.checked().is_err());
    }
}
