//! Phase 9: Optional LLM labels for community and process names.

use anyhow::Result;

use crate::codegraph::db::SharedCodeGraphDb;

/// Enrich community and process nodes with LLM-generated labels.
///
/// This phase is optional (controlled by `config.llm_enrichment`) and is
/// auto-skipped if the node count exceeds `config.node_embedding_skip_threshold`.
pub async fn enrich(
    project_id: &str,
    _db: &SharedCodeGraphDb,
) -> Result<()> {
    tracing::debug!(
        project_id = %project_id,
        "enriching with LLM labels (stub)"
    );

    // Will be implemented when LLM integration is ready.
    // The flow:
    // 1. Query all Community nodes
    // 2. For each, gather its top symbols and file contents
    // 3. Send to LLM for a concise name and description
    // 4. Update the Community node with the generated labels
    // 5. Similarly for Process nodes

    Ok(())
}
