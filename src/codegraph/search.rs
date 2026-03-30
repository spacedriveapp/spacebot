//! Hybrid BM25 + semantic + RRF search for the code graph.

use anyhow::Result;

use super::db::SharedCodeGraphDb;
use super::types::GraphSearchResult;

/// Execute a hybrid search across the code graph.
///
/// Combines BM25 full-text search with semantic vector search using
/// reciprocal rank fusion (RRF) for result merging.
pub async fn hybrid_search(
    project_id: &str,
    query: &str,
    limit: usize,
    _db: &SharedCodeGraphDb,
) -> Result<Vec<GraphSearchResult>> {
    tracing::debug!(
        project_id = %project_id,
        query = %query,
        limit = limit,
        "executing hybrid search (stub)"
    );

    // Will be implemented with LadybugDB's FTS and VECTOR extensions.
    // The flow:
    // 1. BM25 search via FTS index → ranked list A
    // 2. Semantic search via VECTOR index → ranked list B
    // 3. Reciprocal Rank Fusion: score(d) = Σ 1/(k + rank_i(d))
    // 4. Merge and return top-k results

    Ok(Vec::new())
}
