//! The search abstraction.
//!
//! Only lexical (BM25 full-text) search is implemented — the "basic working
//! search path" the bootstrap milestone asks for, now with optional
//! wing/room scope (issue #10). Semantic/vector search, temporal filtering,
//! graph-aware retrieval, and hybrid ranking are later phases (#42): the
//! reason this is its own module rather than `app` calling
//! `store::list_drawers_matching` directly is so those phases add functions here
//! (and combine their results, reusing the same scope parameters) without
//! `app` or the interfaces above it changing shape.

use crate::error::Result;
use crate::store::{MatchMode, SurrealStore};

// Re-exported so the interface layers (notably `client`, which only needs
// the wire type) can name a search result without importing `crate::store`
// — which AGENTS.md invariant #1 and the `store-isolation` hook forbid them.
pub use crate::store::SearchHit;

/// Search drawer content lexically, returning at most `limit` hits ordered
/// by BM25 relevance, optionally scoped to one wing and/or room by name.
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
    wing: Option<&str>,
    room: Option<&str>,
) -> Result<Vec<SearchHit>> {
    let strict = store
        .list_drawers_matching(query, limit, wing, room, MatchMode::All)
        .await?;
    // A single term (or none) means AND and OR are the same query, so a
    // retry could only repeat the empty answer at the cost of a query.
    if !strict.is_empty() || query.split_whitespace().nth(1).is_none() {
        return Ok(strict);
    }
    store
        .list_drawers_matching(query, limit, wing, room, MatchMode::Any)
        .await
}
