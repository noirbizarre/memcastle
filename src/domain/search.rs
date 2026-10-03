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

/// Which point in time a search looks at.
///
/// A drawer or edge is *valid at* `t` when `valid_from <= t` and it has no
/// `valid_to` or `valid_to > t`: the end is exclusive, so a record superseded
/// at `t` is gone at `t` and its replacement (valid from `t`) takes over
/// without a moment where both or neither match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temporal {
    /// Valid right now. The default: superseded memory is not recalled
    /// unless asked for.
    #[default]
    Current,
    /// Valid at this instant (point-in-time retrieval).
    AsOf(DateTime<Utc>),
    /// Every record regardless of validity: current and historical.
    All,
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
}
