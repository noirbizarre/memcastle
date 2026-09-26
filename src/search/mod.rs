//! The search abstraction.
//!
//! Only lexical (BM25 full-text) search is implemented — the "basic working
//! search path" the bootstrap milestone asks for. Semantic/vector search,
//! metadata/wing/room/temporal filtering, graph-aware retrieval, and hybrid
//! ranking are later phases: the reason this is its own module rather than
//! `app` calling `store::lexical_search` directly is so those phases add
//! functions here (and combine their results) without `app` or the
//! interfaces above it changing shape.

use crate::error::Result;
use crate::store::{SearchHit, SurrealStore};

/// Search drawer content lexically, returning at most `limit` hits ordered
/// by BM25 relevance.
///
/// # Errors
///
/// Returns an error if the underlying store query fails.
pub async fn lexical_search(
    store: &SurrealStore,
    query: &str,
    limit: u32,
) -> Result<Vec<SearchHit>> {
    store.lexical_search(query, limit).await
}
