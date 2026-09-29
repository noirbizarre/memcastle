//! The search abstraction.
//!
//! Only lexical (BM25 full-text) search is implemented — the "basic working
//! search path" the bootstrap milestone asks for, now with optional
//! wing/room scope (issue #10). Semantic/vector search, temporal filtering,
//! graph-aware retrieval, and hybrid ranking are later phases (#42): the
//! reason this is its own module rather than `app` calling
//! `store::lexical_search` directly is so those phases add functions here
//! (and combine their results, reusing the same scope parameters) without
//! `app` or the interfaces above it changing shape.

use crate::error::Result;
use crate::store::SurrealStore;

// Re-exported so the interface layers (notably `client`, which only needs
// the wire type) can name a search result without importing `crate::store`
// — which AGENTS.md invariant #1 and the `store-isolation` hook forbid them.
pub use crate::store::SearchHit;

/// Search drawer content lexically, returning at most `limit` hits ordered
/// by BM25 relevance, optionally scoped to one wing and/or room by name.
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
    store.lexical_search(query, limit, wing, room).await
}
